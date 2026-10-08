//! Process helpers shared by the tools that run shell commands (cli,
//! declarative tools, hooks, MCP servers) and by the sync SDK entry points.
//!
//! - Children are started in their own process group, so a timeout or abort
//!   can kill the whole tree (`sh -c` plus everything it spawned), not just
//!   the `sh` itself.
//! - `blocking` runs sync work (git, MCP handshakes) without stalling an async
//!   worker thread when called from inside a multi-threaded Tokio runtime.

/// Run blocking work from (possibly) async context. On a multi-threaded Tokio
/// runtime this uses `block_in_place`, so the worker hands off its other tasks
/// first; elsewhere (no runtime, current-thread runtime) it just runs `f`.
pub(crate) fn blocking<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// Start the child as the leader of a new process group (Unix).
pub(crate) fn new_group(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Std-command variant of [`new_group`].
pub(crate) fn new_group_std(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// SIGKILL every process in the group led by `pid` (Unix; no-op elsewhere —
/// `kill_on_drop` still reaps the direct child there).
pub(crate) fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        if let Ok(pgid) = i32::try_from(pid) {
            if pgid > 0 {
                // SAFETY: plain syscall; a negative pid addresses the group.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Kills a child's process group when dropped (timeout, abort, or the future
/// being dropped) unless `disarm`ed after a normal exit — so background jobs a
/// finished command deliberately left running are not touched.
pub(crate) struct GroupGuard {
    pid: Option<u32>,
}

impl GroupGuard {
    pub(crate) fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }
    pub(crate) fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid.take() {
            kill_group(pid);
        }
    }
}
