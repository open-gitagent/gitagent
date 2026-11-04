//! The main shell plus background sessions for servers, REPLs and other
//! commands that keep running.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::{shell_quote, ExecResult, Session, ShellConfig};

pub const MAIN_SESSION: u32 = 1;
/// Upper bound on sessions alive at once, the main one included.
pub const MAX_SESSIONS: usize = 8;

pub struct Terminal {
    base: ShellConfig,
    sessions: BTreeMap<u32, Session>,
    next_id: u32,
}

impl Terminal {
    pub async fn start(base: ShellConfig) -> Result<Self> {
        let main = Session::start(session_cfg(&base, MAIN_SESSION)).await?;
        Ok(Terminal {
            base,
            sessions: BTreeMap::from([(MAIN_SESSION, main)]),
            next_id: MAIN_SESSION + 1,
        })
    }

    fn main(&mut self) -> &mut Session {
        self.sessions
            .get_mut(&MAIN_SESSION)
            .expect("the main session always exists")
    }

    /// Working directory of the main session.
    pub fn cwd(&self) -> &Path {
        self.sessions[&MAIN_SESSION].cwd()
    }

    /// Ids of background sessions still running a command.
    pub fn background_sessions(&self) -> Vec<u32> {
        self.sessions
            .keys()
            .copied()
            .filter(|&id| id != MAIN_SESSION)
            .collect()
    }

    /// Runs `cmd` in the main session, or in a new background session that
    /// starts with the main session's exported variables and directory.
    pub async fn exec(&mut self, cmd: &str, yield_ms: u64, background: bool) -> Result<ExecResult> {
        if !background {
            let mut r = self.main().exec(cmd, yield_ms).await?;
            r.session = MAIN_SESSION;
            return Ok(r);
        }
        if self.sessions.len() >= MAX_SESSIONS {
            bail!(
                "too many sessions are running ({}); stop one first, e.g. write_stdin with \"\\x03\". Running: {:?}",
                self.sessions.len(),
                self.background_sessions()
            );
        }
        let id = self.next_id;
        self.next_id += 1;

        let mut cfg = session_cfg(&self.base, id);
        let env_file = self.base.script_dir.join(format!("env-{id}.sh"));
        let main = self.main();
        if main.is_alive() && !main.is_running() {
            let export = format!("export -p > {}", shell_quote(&env_file));
            if main.exec(&export, 5000).await?.exit == Some(0) {
                cfg.init
                    .push(format!(". {} 2>/dev/null", shell_quote(&env_file)));
            }
        }
        cfg.cwd = main.cwd().to_path_buf();
        cfg.init
            .push(format!("cd {} 2>/dev/null", shell_quote(&cfg.cwd)));

        let mut session = Session::start(cfg).await?;
        let mut r = session.exec(cmd, yield_ms).await?;
        r.session = id;
        // A finished session only hangs up, so a server it started with
        // `nohup` keeps running for the rest of the run (and for `--linger`).
        if session.is_running() {
            self.sessions.insert(id, session);
        } else {
            session.hang_up();
        }
        Ok(r)
    }

    /// Sends `keys` to a running session (empty keys just wait for output).
    pub async fn write_stdin(&mut self, id: u32, keys: &[u8], yield_ms: u64) -> Result<ExecResult> {
        let running = self.background_sessions();
        let Some(session) = self.sessions.get_mut(&id) else {
            bail!("there is no session {id}; running background sessions: {running:?}");
        };
        if !session.is_running() {
            bail!("session {id} is not running a command; use exec to run one");
        }
        let mut r = session.write_stdin(keys, yield_ms).await?;
        r.session = id;
        // A background session is done once its command finishes.
        if id != MAIN_SESSION && !session.is_running() {
            if let Some(mut done) = self.sessions.remove(&id) {
                done.hang_up();
            }
        }
        Ok(r)
    }

    /// Keeps every shell open, discarding their output, until all of them
    /// exit. Processes the agent left running (servers, background jobs) keep
    /// a live terminal, as they would in a terminal multiplexer.
    pub async fn linger(&mut self) {
        loop {
            let mut alive = false;
            for session in self.sessions.values_mut() {
                if session.is_alive() {
                    alive = true;
                    session.drain(Duration::from_millis(500)).await;
                }
            }
            if !alive {
                return;
            }
        }
    }

    /// Stops every session.
    pub fn kill_all(&mut self) {
        for session in self.sessions.values_mut() {
            session.kill();
        }
        self.sessions.retain(|&id, _| id == MAIN_SESSION);
    }
}

fn session_cfg(base: &ShellConfig, id: u32) -> ShellConfig {
    let mut cfg = base.clone();
    cfg.script_dir = base.script_dir.join(format!("session-{id}"));
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(r: &ExecResult) -> String {
        String::from_utf8_lossy(&r.output).into_owned()
    }

    async fn start() -> (tempfile::TempDir, Terminal) {
        let dir = tempfile::tempdir().unwrap();
        let t = Terminal::start(ShellConfig::new(dir.path(), dir.path().join("scripts")))
            .await
            .unwrap();
        (dir, t)
    }

    #[tokio::test]
    async fn background_session_inherits_env_and_directory() {
        let (_d, mut t) = start().await;
        t.exec("mkdir -p sub && cd sub && export GA_V=7", 5000, false)
            .await
            .unwrap();
        let r = t
            .exec("echo $GA_V $(basename $PWD); sleep 30", 1000, true)
            .await
            .unwrap();
        assert_eq!(r.session, 2);
        assert!(!r.finished);
        // A trailing newline is held back while the command runs (it could
        // start the end marker).
        assert_eq!(text(&r).trim_end(), "7 sub");
        assert_eq!(t.background_sessions(), vec![2]);

        // The main session is free while the background one runs.
        let r = t.exec("echo main", 5000, false).await.unwrap();
        assert_eq!((r.session, text(&r).as_str()), (MAIN_SESSION, "main\n"));

        let r = t.write_stdin(2, b"\x03", 3000).await.unwrap();
        assert!(r.finished);
        assert!(t.background_sessions().is_empty());
    }

    #[tokio::test]
    async fn repl_in_a_background_session() {
        let (_d, mut t) = start().await;
        let r = t.exec("cat -n", 300, true).await.unwrap();
        assert!(!r.finished);
        let r = t.write_stdin(r.session, b"hello\n", 1000).await.unwrap();
        assert!(text(&r).contains("1\thello"), "{:?}", text(&r));
        // Ctrl-D ends cat; the session finishes and goes away.
        let r = t.write_stdin(2, b"\x04", 3000).await.unwrap();
        assert!(r.finished);
        assert_eq!(r.exit, Some(0));
        assert!(t.write_stdin(2, b"", 100).await.is_err());
    }

    #[tokio::test]
    async fn poll_and_interrupt_the_main_session() {
        let (_d, mut t) = start().await;
        let r = t.exec("sleep 1; echo slow", 100, false).await.unwrap();
        assert!(!r.finished);
        let r = t.write_stdin(MAIN_SESSION, b"", 5000).await.unwrap();
        assert!(r.finished);
        assert_eq!(text(&r), "slow\n");

        // Interrupting a shell builtin loop: the shell takes the Ctrl-C and
        // is asked for the end marker again.
        t.exec("while true; do :; done", 300, false).await.unwrap();
        let r = t.write_stdin(MAIN_SESSION, b"\x03", 2000).await.unwrap();
        assert!(r.finished, "{r:?}");
        let r = t.exec("echo alive", 5000, false).await.unwrap();
        assert_eq!(text(&r), "alive\n");
    }

    #[tokio::test]
    async fn linger_keeps_jobs_running_until_the_shells_exit() {
        let (dir, mut t) = start().await;
        let marker = dir.path().join("marker");
        let cmd = format!("(sleep 1; echo late; touch {}) &", marker.display());
        t.exec(&cmd, 5000, false).await.unwrap();
        // The shells stay up, so linger only ends with the timeout here...
        let res = tokio::time::timeout(Duration::from_secs(3), t.linger()).await;
        assert!(res.is_err());
        // ...and the job, which wrote to the terminal, finished meanwhile.
        assert!(marker.exists());
        // Once every shell has exited, linger returns.
        t.exec("exit", 5000, false).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), t.linger())
            .await
            .expect("linger returns when no shell is left");
    }

    #[tokio::test]
    async fn errors_for_unknown_or_idle_sessions_and_session_limit() {
        let (_d, mut t) = start().await;
        assert!(t.write_stdin(9, b"x", 100).await.is_err());
        assert!(t.write_stdin(MAIN_SESSION, b"x", 100).await.is_err());
        // A background command that finishes at once leaves no session behind.
        let r = t.exec("echo quick", 5000, true).await.unwrap();
        assert!(r.finished);
        assert!(t.background_sessions().is_empty());

        for _ in 1..MAX_SESSIONS {
            t.exec("sleep 30", 100, true).await.unwrap();
        }
        let err = t.exec("sleep 30", 100, true).await.unwrap_err();
        assert!(err.to_string().contains("too many sessions"));
        t.kill_all();
        assert!(t.background_sessions().is_empty());
    }

    #[tokio::test]
    async fn a_finished_background_session_leaves_nohup_jobs_until_the_terminal_ends() {
        use crate::tests::{parse_pid, process_exists, wait_until_gone};
        let (_d, mut t) = start().await;
        let r = t
            .exec("nohup sleep 300 >/dev/null 2>&1 & echo $!", 5000, true)
            .await
            .unwrap();
        assert!(r.finished);
        assert!(t.background_sessions().is_empty());
        let pid = parse_pid(&r);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            process_exists(pid),
            "the nohup job was stopped with its session"
        );

        let r = t.exec("sleep 300 & echo $!", 5000, false).await.unwrap();
        let main_job = parse_pid(&r);
        drop(t);
        assert!(wait_until_gone(main_job).await, "pid {main_job} survived");
        // The finished session's job is no longer the terminal's to stop.
        assert!(process_exists(pid));
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        )
        .expect("stopping the nohup job");
    }
}
