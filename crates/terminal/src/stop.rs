//! Stopping everything a shell started.
//!
//! The shell leads its own session on its own pty (portable-pty calls
//! `setsid` before it starts the shell, and the sandbox launcher replaces
//! itself with the shell), so the session id is the shell's pid. With job
//! control on, every foreground and background job gets its own process group
//! in that session, so killing the shell alone leaves its jobs running.

use std::path::Path;
use std::time::{Duration, Instant};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

/// How long programs get to exit after SIGTERM before they are killed.
const TERM_GRACE: Duration = Duration::from_secs(1);
const TERM_POLL: Duration = Duration::from_millis(20);

/// How long the shell's jobs get to start before it is hung up. The shell
/// passes its SIGHUP on to every job, and `nohup` ignores SIGHUP only once it
/// runs, so a job forked just before the hang-up (`nohup server &` as the
/// last command) would die with the shell.
const JOB_START_GRACE: Duration = Duration::from_millis(200);

/// Sends SIGTERM to every process group in the shell's session, waits up to
/// [`TERM_GRACE`] for them to exit, then sends SIGKILL to whatever is left,
/// the shell included. `tty` is the shell's terminal; `foreground` is that
/// terminal's foreground group, signalled even when the session's processes
/// cannot be listed.
pub(crate) fn stop_session(shell: Pid, tty: Option<&Path>, foreground: Option<Pid>) {
    let mut groups = session_groups(shell, tty);
    groups.push(shell);
    groups.extend(foreground);
    groups.sort();
    groups.dedup();

    terminate(&groups, shell);

    // Jobs started during the grace period (e.g. by a trap) are caught too.
    groups.extend(session_groups(shell, tty));
    groups.sort();
    groups.dedup();
    signal_groups(&groups, Signal::SIGKILL);
}

/// Stops the command running in the foreground of the shell's terminal, if
/// it is not the shell itself: SIGTERM, then SIGKILL after [`TERM_GRACE`].
pub(crate) fn stop_foreground(shell: Pid, foreground: Option<Pid>) {
    let Some(group) = foreground.filter(|&g| g != shell) else {
        return;
    };
    terminate(&[group], shell);
    signal_groups(&[group], Signal::SIGKILL);
}

/// Waits [`JOB_START_GRACE`] if the shell has jobs, before it is hung up.
pub(crate) fn let_jobs_start(shell: Pid, tty: Option<&Path>) {
    if session_groups(shell, tty).iter().any(|&group| group != shell) {
        std::thread::sleep(JOB_START_GRACE);
    }
}

/// Sends SIGTERM to `groups` and waits up to [`TERM_GRACE`] for all of them
/// but the shell's own to exit (an interactive shell ignores SIGTERM).
fn terminate(groups: &[Pid], shell: Pid) {
    signal_groups(groups, Signal::SIGTERM);
    // Stopped jobs act on SIGTERM only once they are continued.
    signal_groups(groups, Signal::SIGCONT);
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline
        && groups
            .iter()
            .any(|&g| g != shell && killpg(g, None).is_ok())
    {
        std::thread::sleep(TERM_POLL);
    }
}

fn signal_groups(groups: &[Pid], signal: Signal) {
    for &group in groups {
        // ESRCH means the group already exited, which is the goal; there is
        // nothing more to do for a group that cannot be signalled.
        let _ = killpg(group, signal);
    }
}

/// Process groups with a member in the shell's session. Empty when the
/// processes cannot be listed; the caller still signals the groups it knows.
#[cfg(target_os = "linux")]
fn session_groups(shell: Pid, _tty: Option<&Path>) -> Vec<Pid> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.parse::<u32>().is_ok())
        })
        // A process can exit between listing and reading; it is skipped.
        .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
        .filter_map(|stat| proc_stat_group(&stat))
        .filter(|&(_, session)| session == shell)
        .map(|(group, _)| group)
        .collect()
}

/// Process group and session from a `/proc/<pid>/stat` line:
/// `pid (comm) state ppid pgrp session ...`, where comm may contain spaces
/// and parentheses.
#[cfg(target_os = "linux")]
fn proc_stat_group(stat: &str) -> Option<(Pid, Pid)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace().skip(2);
    let group = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    Some((Pid::from_raw(group), Pid::from_raw(session)))
}

/// Process groups with a member on the shell's terminal (macOS `ps` does not
/// report session ids). Empty when the processes cannot be listed; the caller
/// still signals the groups it knows.
#[cfg(target_os = "macos")]
fn session_groups(_shell: Pid, tty: Option<&Path>) -> Vec<Pid> {
    let Some(name) = tty.and_then(Path::file_name) else {
        return Vec::new();
    };
    let Ok(out) = std::process::Command::new("/bin/ps")
        .arg("-t")
        .arg(name)
        .args(["-o", "pgid="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|group| group.parse().ok())
        .map(Pid::from_raw)
        .collect()
}

/// Other systems: only the shell's and the foreground group are known.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn session_groups(_shell: Pid, _tty: Option<&Path>) -> Vec<Pid> {
    Vec::new()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn reads_group_and_session_from_proc_stat() {
        let stat = "4242 (a (b) c) S 1 4240 4200 34816 4240 4194560 0 0";
        assert_eq!(
            proc_stat_group(stat),
            Some((Pid::from_raw(4240), Pid::from_raw(4200)))
        );
    }
}
