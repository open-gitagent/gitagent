//! Script hooks — `hooks.yaml` `pre_tool_use` commands run as a `ToolGate`.
//!
//! Each hook command receives `{"tool": <name>, "args": <value>}` as JSON on
//! stdin and may print `{"action": "deny", "message": "..."}` on stdout to block
//! the call (exit 0 with any other output = allow). Hooks are gates, so they
//! fail closed: a hook that can't start, exits non-zero, or runs past its
//! timeout (`GITAGENT_HOOK_TIMEOUT_SECS`, default 30s) denies the call — the
//! model sees why, and the run continues. A timed-out or aborted hook is
//! killed together with its children.

use crate::pi::gate::{GateDecision, ToolGate};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use crate::sdk::proc::{new_group, GroupGuard};

#[derive(Deserialize)]
struct HooksFile {
    #[serde(default)]
    pre_tool_use: Vec<HookEntry>,
}

#[derive(Deserialize)]
struct HookEntry {
    command: String,
}

pub struct HookGate {
    commands: Vec<String>,
    cwd: PathBuf,
}

/// Load `hooks.yaml`; returns None when there are no pre_tool_use hooks.
pub fn load_hook_gate(dir: &Path) -> Option<HookGate> {
    let raw = std::fs::read_to_string(dir.join("hooks.yaml")).ok()?;
    let parsed: HooksFile = serde_yaml::from_str(&raw).ok()?;
    let commands: Vec<String> = parsed.pre_tool_use.into_iter().map(|h| h.command).collect();
    if commands.is_empty() {
        return None;
    }
    Some(HookGate { commands, cwd: dir.to_path_buf() })
}

fn hook_timeout() -> Duration {
    std::env::var("GITAGENT_HOOK_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30))
}

/// Run one hook. Ok(stdout JSON or Null) on exit 0; Err(reason) otherwise.
async fn run_hook(cmd: &str, cwd: &Path, payload: &str, timeout: Duration) -> Result<Value, String> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    new_group(&mut command);
    let mut child = command.spawn().map_err(|e| format!("hook failed to start: {e}"))?;
    let mut guard = GroupGuard::new(child.id());
    // Feed stdin concurrently with draining stdout, so a large payload can't
    // deadlock against a hook that writes before it finishes reading.
    let stdin = child.stdin.take();
    let payload = payload.as_bytes().to_vec();
    let write = async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&payload).await;
            // Dropped here → the hook sees EOF.
        }
    };
    let run = async {
        let (_, out) = tokio::join!(write, child.wait_with_output());
        out
    };
    let out = match tokio::time::timeout(timeout, run).await {
        Err(_) => return Err(format!("hook timed out after {}s", timeout.as_secs())),
        Ok(Err(e)) => return Err(format!("hook failed: {e}")),
        Ok(Ok(out)) => out,
    };
    guard.disarm();
    if !out.status.success() {
        return Err(format!("hook exited with status {}", out.status.code().unwrap_or(-1)));
    }
    Ok(serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null))
}

#[async_trait]
impl ToolGate for HookGate {
    async fn check(&self, name: &str, args: &Value) -> GateDecision {
        let payload = json!({ "tool": name, "args": args }).to_string();
        let timeout = hook_timeout();
        for cmd in &self.commands {
            match run_hook(cmd, &self.cwd, &payload, timeout).await {
                Ok(v) => {
                    if v.get("action").and_then(Value::as_str) == Some("deny") {
                        let msg = v.get("message").and_then(Value::as_str).unwrap_or("blocked by hook");
                        return GateDecision::Deny(msg.to_string());
                    }
                }
                Err(reason) => return GateDecision::Deny(format!("pre_tool_use {reason}")),
            }
        }
        GateDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(cmd: &str) -> GateDecision {
        std::env::set_var("GITAGENT_HOOK_TIMEOUT_SECS", "2");
        let gate = HookGate { commands: vec![cmd.to_string()], cwd: std::env::temp_dir() };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(gate.check("cli", &json!({ "command": "x".repeat(200_000) })))
    }

    #[test]
    fn hooks_fail_closed_and_handle_large_payloads() {
        assert!(matches!(decide("cat >/dev/null; echo '{}'"), GateDecision::Allow));
        // >64 KiB payload with a hook that writes before reading: no deadlock.
        assert!(matches!(decide("head -c 100000 /dev/zero | tr '\\0' ' '; cat >/dev/null"), GateDecision::Allow));
        assert!(matches!(decide("echo '{\"action\":\"deny\",\"message\":\"no\"}'"), GateDecision::Deny(m) if m == "no"));
        assert!(matches!(decide("exit 3"), GateDecision::Deny(_)));
        let t = std::time::Instant::now();
        assert!(matches!(decide("sleep 30"), GateDecision::Deny(m) if m.contains("timed out")));
        assert!(t.elapsed() < std::time::Duration::from_secs(10));
    }
}
