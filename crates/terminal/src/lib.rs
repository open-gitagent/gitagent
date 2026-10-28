//! Persistent shell sessions on a PTY.
//!
//! Each command is written to a script file and run with one short input line,
//! `. 'script'; printf '\n__GA_END_{seq}__ %s %s\n' "$?" "$PWD"`, so that
//! quotes, heredocs and commands reading stdin cannot swallow the end marker.
//! The shell runs interactively (for job control, so Ctrl-C reaches only the
//! foreground job) with line editing, echo and prompts turned off.

mod buffer;
mod keys;
mod manager;
mod stop;

pub use keys::decode_keys;
pub use manager::{Terminal, MAIN_SESSION, MAX_SESSIONS};

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use nix::unistd::Pid;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;
use tokio::time::timeout_at;

use buffer::{marker_prefix, OutputBuffer, Taken};

/// Environment set for every shell: no pagers, colors or interactive prompts.
const SHELL_ENV: &[(&str, &str)] = &[
    ("TERM", "dumb"),
    ("NO_COLOR", "1"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("MANPAGER", "cat"),
    ("SYSTEMD_PAGER", ""),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("PIP_PROGRESS_BAR", "off"),
    ("PYTHONUNBUFFERED", "1"),
    ("HISTFILE", "/dev/null"),
];

/// Run once after the shell starts. Echo, CR translation and prompts would
/// otherwise end up in command output.
const INIT_LINE: &str =
    "stty -echo -onlcr 2>/dev/null; PS1=''; PS2=''; PS0=''; PROMPT_COMMAND=''; set +o history 2>/dev/null";

const READ_CHUNK: usize = 8192;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const INTERRUPT_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct ShellConfig {
    /// Initial working directory.
    pub cwd: PathBuf,
    /// Where command scripts are written; created if missing.
    pub script_dir: PathBuf,
    /// Variables removed from the inherited environment (API keys, etc.).
    pub remove_env: Vec<String>,
    /// Extra variables, applied after `SHELL_ENV`.
    pub env: Vec<(String, String)>,
    /// Extra commands run once when the shell starts (e.g. restoring the
    /// environment of another session).
    pub init: Vec<String>,
    /// Program and arguments that start the shell for us (the sandbox
    /// launcher); the shell command follows a `--`.
    pub launcher: Vec<String>,
    /// In-memory output cap per command, in bytes.
    pub max_buffer_bytes: usize,
    pub cols: u16,
    pub rows: u16,
}

impl ShellConfig {
    pub fn new(cwd: impl Into<PathBuf>, script_dir: impl Into<PathBuf>) -> Self {
        ShellConfig {
            cwd: cwd.into(),
            script_dir: script_dir.into(),
            remove_env: Vec::new(),
            env: Vec::new(),
            init: Vec::new(),
            launcher: Vec::new(),
            max_buffer_bytes: 1 << 20,
            cols: 200,
            rows: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// Session that ran the command (set by [`Terminal`]; 0 for a bare [`Session`]).
    pub session: u32,
    pub output: Vec<u8>,
    /// Exit code, when the command finished and the shell survived.
    pub exit: Option<i32>,
    /// False when the wait time ran out first; the command keeps running.
    pub finished: bool,
    /// Bytes dropped from the middle of very large output.
    pub omitted_bytes: u64,
    pub elapsed: Duration,
    /// A new shell had to be started for this command.
    pub shell_restarted: bool,
    /// Things the model should know, e.g. that the shell was restarted.
    pub notes: Vec<String>,
}

enum ReadEvent {
    Data(Vec<u8>),
    Eof,
}

/// What ending a shell stops besides the shell.
enum Ending {
    /// Every process in the shell's session, background jobs included.
    Session,
    /// Only the command in the terminal's foreground.
    Foreground,
}

struct Shell {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: mpsc::UnboundedReceiver<ReadEvent>,
    buf: OutputBuffer,
    eof: bool,
}

/// One persistent shell. If the shell exits (e.g. `exit` or `set -e`), a new
/// one is started in the last known directory on the next command.
pub struct Session {
    cfg: ShellConfig,
    shell: Option<Shell>,
    seq: u64,
    /// Sequence number of a command still running after its wait time ran out.
    running: Option<u64>,
    /// Set when Ctrl-C was sent to the running command.
    interrupted: bool,
    cwd: PathBuf,
}

impl Session {
    pub async fn start(cfg: ShellConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.script_dir)
            .with_context(|| format!("creating {}", cfg.script_dir.display()))?;
        let mut session = Session {
            cwd: cfg.cwd.clone(),
            cfg,
            shell: None,
            seq: 0,
            running: None,
            interrupted: false,
        };
        // Starting a shell very rarely fails right after the fork (seen under
        // heavy parallel load); one fresh attempt is enough.
        if session.spawn().await.is_err() {
            session.kill();
            session.spawn().await?;
        }
        Ok(session)
    }

    /// Directory the shell was in after the last finished command.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// False after the shell exited; the next `exec` starts a new one.
    pub fn is_alive(&self) -> bool {
        self.shell.is_some()
    }

    /// Runs `cmd`, waiting up to `yield_ms` for it to finish. A command still
    /// running from an earlier call is interrupted first.
    pub async fn exec(&mut self, cmd: &str, yield_ms: u64) -> Result<ExecResult> {
        let started = Instant::now();
        let mut notes = Vec::new();
        if self.running.is_some() {
            notes.extend(self.interrupt_running().await?);
        }
        let mut restarted = false;
        if self.shell.is_none() {
            self.spawn().await?;
            restarted = true;
            notes.push(format!(
                "A new shell was started in {} (shell variables, functions and background jobs from before were lost).",
                self.cwd.display()
            ));
        }

        self.seq += 1;
        let seq = self.seq;
        let script = self.cfg.script_dir.join(format!("cmd-{seq}.sh"));
        let mut body = cmd.to_string();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        std::fs::write(&script, body).with_context(|| format!("writing {}", script.display()))?;
        let line = format!(
            ". {}; {}\n",
            shell_quote(&script),
            marker_command(seq, "\"$?\"")
        );
        self.write_input(line.as_bytes())?;
        self.running = Some(seq);
        self.interrupted = false;

        let mut result = self.wait(seq, Duration::from_millis(yield_ms)).await?;
        result.elapsed = started.elapsed();
        result.shell_restarted = restarted;
        notes.append(&mut result.notes);
        result.notes = notes;
        Ok(result)
    }

    /// Sends `keys` to the running command and waits up to `yield_ms` for it
    /// to finish. Empty `keys` just waits. After Ctrl-C (`0x03`), if the shell
    /// itself took the interrupt and dropped the rest of its input line, it is
    /// asked to print the end marker again.
    pub async fn write_stdin(&mut self, keys: &[u8], yield_ms: u64) -> Result<ExecResult> {
        let started = Instant::now();
        let Some(seq) = self.running else {
            bail!("no command is running");
        };
        if !keys.is_empty() {
            self.write_input(keys)?;
        }
        if keys.contains(&0x03) {
            self.interrupted = true;
        }
        let mut result = self.wait(seq, Duration::from_millis(yield_ms)).await?;
        if !result.finished && self.interrupted && self.shell_in_foreground() {
            self.interrupted = false;
            let line = format!("{}\n", marker_command(seq, "130"));
            self.write_input(line.as_bytes())?;
            let more = self.wait(seq, INTERRUPT_WAIT).await?;
            result.output.extend_from_slice(&more.output);
            result.omitted_bytes += more.omitted_bytes;
            result.exit = more.exit;
            result.finished = more.finished;
            result.notes.extend(more.notes);
        }
        result.elapsed = started.elapsed();
        Ok(result)
    }

    /// True when the shell, not a program it started, owns the terminal: it is
    /// at its prompt or running a shell builtin.
    fn shell_in_foreground(&self) -> bool {
        let Some(shell) = &self.shell else {
            return false;
        };
        match (
            shell.master.process_group_leader(),
            shell.child.process_id(),
        ) {
            (Some(pgrp), Some(pid)) => i64::from(pgrp) == i64::from(pid),
            _ => false,
        }
    }

    /// Waits up to `yield_ms` more for a command that is still running.
    pub async fn poll(&mut self, yield_ms: u64) -> Result<ExecResult> {
        let started = Instant::now();
        let Some(seq) = self.running else {
            bail!("no command is running");
        };
        let mut result = self.wait(seq, Duration::from_millis(yield_ms)).await?;
        result.elapsed = started.elapsed();
        Ok(result)
    }

    /// Reads and discards output for up to `max_wait`, so programs writing to
    /// the terminal never block. Notices when the shell exits.
    pub async fn drain(&mut self, max_wait: Duration) {
        let deadline = tokio::time::Instant::now() + max_wait;
        let Some(shell) = self.shell.as_mut() else {
            return;
        };
        let exited = loop {
            match timeout_at(deadline, shell.rx.recv()).await {
                Ok(Some(ReadEvent::Data(_))) => {}
                Ok(Some(ReadEvent::Eof)) | Ok(None) => break true,
                Err(_) => break false,
            }
        };
        if exited {
            let _ = shell.child.wait();
            self.shell = None;
            self.running = None;
        }
    }

    /// Stops the shell and everything started from it: the running command
    /// and background jobs, which job control puts in groups of their own.
    pub fn kill(&mut self) {
        self.end_shell(Ending::Session);
    }

    /// Stops the shell and the command it is running. Background jobs get
    /// the shell's SIGHUP, as when a terminal window closes, so jobs started
    /// with `nohup` (e.g. a server) keep running, including one started just
    /// before the hang-up.
    pub(crate) fn hang_up(&mut self) {
        self.end_shell(Ending::Foreground);
    }

    fn end_shell(&mut self, ending: Ending) {
        if let Some(mut shell) = self.shell.take() {
            if let Some(pid) = shell.child.process_id().and_then(|p| i32::try_from(p).ok()) {
                let pid = Pid::from_raw(pid);
                let foreground = shell.master.process_group_leader().map(Pid::from_raw);
                let tty = shell.master.tty_name();
                match ending {
                    Ending::Session => stop::stop_session(pid, tty.as_deref(), foreground),
                    Ending::Foreground => {
                        stop::stop_foreground(pid, foreground);
                        stop::let_jobs_start(pid, tty.as_deref());
                    }
                }
            }
            // SIGHUP, then SIGKILL, to the shell. Errors only mean it already
            // exited; `wait` then just reaps it.
            let _ = shell.child.kill();
            let _ = shell.child.wait();
        }
        self.running = None;
    }

    async fn spawn(&mut self) -> Result<()> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: self.cfg.rows,
                cols: self.cfg.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening a pty")?;

        let (shell, shell_args): (String, &[&str]) = match find_in_path("bash") {
            Some(bash) => (
                bash.to_string_lossy().into_owned(),
                &["--noprofile", "--norc", "--noediting", "-i"],
            ),
            None => ("sh".to_string(), &["-i"]),
        };
        let mut cmd = match self.cfg.launcher.split_first() {
            Some((program, args)) => {
                let mut c = CommandBuilder::new(program);
                c.args(args);
                c.arg("--");
                c.arg(&shell);
                c
            }
            None => CommandBuilder::new(&shell),
        };
        cmd.args(shell_args);
        let cwd = if self.cwd.is_dir() {
            self.cwd.clone()
        } else {
            self.cfg.cwd.clone()
        };
        cmd.cwd(&cwd);
        for key in &self.cfg.remove_env {
            cmd.env_remove(key);
        }
        for (k, v) in SHELL_ENV {
            cmd.env(k, v);
        }
        for (k, v) in &self.cfg.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("starting the shell")?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || read_loop(reader, tx));

        self.shell = Some(Shell {
            master: pair.master,
            writer,
            child,
            rx,
            buf: OutputBuffer::new(self.cfg.max_buffer_bytes),
            eof: false,
        });
        self.cwd = cwd;
        self.running = None;

        // Initialize, then discard everything up to the first marker (seq 0
        // is never used for commands).
        let mut init = INIT_LINE.to_string();
        for extra in &self.cfg.init {
            init.push_str("; ");
            init.push_str(extra);
        }
        let line = format!("{init}; {}\n", marker_command(0, "0"));
        let mut attempt = 0;
        while let Err(e) = self.write_input(line.as_bytes()) {
            attempt += 1;
            if attempt >= 10 {
                return Err(e);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let res = self.wait(0, STARTUP_TIMEOUT).await?;
        if !res.finished || res.exit.is_none() {
            self.kill();
            bail!(
                "the shell did not start: {}",
                String::from_utf8_lossy(&res.output)
            );
        }
        Ok(())
    }

    /// Sends Ctrl-C to the running command and waits for it to stop. If the
    /// shell itself was interrupted (no marker comes back), asks it to print
    /// the marker again; if that fails too, restarts the shell.
    async fn interrupt_running(&mut self) -> Result<Vec<String>> {
        let Some(seq) = self.running else {
            return Ok(Vec::new());
        };
        self.write_input(b"\x03")?;
        let res = self.wait(seq, INTERRUPT_WAIT).await?;
        if res.finished {
            return Ok(vec![
                "The previous command was still running and was interrupted with Ctrl-C.".into(),
            ]);
        }
        // Only re-ask for the marker when the shell is back in charge; otherwise
        // the line would be typed into the still-running program.
        if self.shell_in_foreground() {
            let line = format!("{}\n", marker_command(seq, "130"));
            self.write_input(line.as_bytes())?;
            if self.wait(seq, INTERRUPT_WAIT).await?.finished {
                return Ok(vec![
                    "The previous command was still running and was interrupted with Ctrl-C."
                        .into(),
                ]);
            }
        }
        self.hang_up();
        Ok(vec![
            "The previous command did not stop on Ctrl-C, so its shell was killed.".into(),
        ])
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        let shell = self.shell.as_mut().context("no shell")?;
        shell
            .writer
            .write_all(bytes)
            .context("writing to the shell")?;
        shell.writer.flush().context("writing to the shell")?;
        Ok(())
    }

    async fn wait(&mut self, seq: u64, max_wait: Duration) -> Result<ExecResult> {
        let deadline = tokio::time::Instant::now() + max_wait;
        let shell = self.shell.as_mut().context("no shell")?;
        loop {
            if let Some((marker, taken)) = shell.buf.take_marker(seq) {
                self.running = None;
                if !marker.cwd.is_empty() {
                    self.cwd = PathBuf::from(&marker.cwd);
                }
                return Ok(result(taken, Some(marker.exit), true, Vec::new()));
            }
            if shell.eof {
                let taken = shell.buf.take_all();
                let status = shell.child.wait().ok();
                self.shell = None;
                self.running = None;
                let code = status.map_or("unknown".to_string(), |s| s.exit_code().to_string());
                let note = format!(
                    "The shell exited (status {code}), e.g. because of `exit` or `set -e`. The next command starts a new shell in {}.",
                    self.cwd.display()
                );
                return Ok(result(taken, None, true, vec![note]));
            }
            match timeout_at(deadline, shell.rx.recv()).await {
                Ok(Some(ReadEvent::Data(bytes))) => shell.buf.push(&bytes),
                Ok(Some(ReadEvent::Eof)) | Ok(None) => shell.eof = true,
                Err(_) => {
                    let taken = shell.buf.take_partial(seq);
                    return Ok(result(taken, None, false, Vec::new()));
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

fn result(taken: Taken, exit: Option<i32>, finished: bool, notes: Vec<String>) -> ExecResult {
    ExecResult {
        session: 0,
        output: taken.output,
        exit,
        finished,
        omitted_bytes: taken.omitted_bytes,
        elapsed: Duration::ZERO,
        shell_restarted: false,
        notes,
    }
}

fn marker_command(seq: u64, status: &str) -> String {
    format!(
        "printf '\\n{}%s %s\\n' {status} \"$PWD\"",
        marker_prefix(seq)
    )
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: mpsc::UnboundedSender<ReadEvent>) {
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut chunk) {
            // Linux reports EIO instead of EOF once the shell side closes.
            Ok(0) | Err(_) => {
                let _ = tx.send(ReadEvent::Eof);
                return;
            }
            Ok(n) => {
                if tx.send(ReadEvent::Data(chunk[..n].to_vec())).is_err() {
                    return;
                }
            }
        }
    }
}

pub(crate) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests;
