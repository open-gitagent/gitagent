//! Runs an agent's `hooks/hooks.yaml` hooks. Each hook gets one JSON object
//! on stdin:
//!
//! ```json
//! {"event": "pre_tool_use", "timestamp": "2026-10-21T09:00:00.000Z",
//!  "data": {"tool_name": "exec", "arguments": {"cmd": "ls"}},
//!  "session": {"id": "…", "agent": "lyra", "model_version": "claude-opus-5-5", "turn": 3},
//!  "session_id": "…", "tool": "cli", "args": {"command": "ls"}}
//! ```
//!
//! (`session_id`, `tool`, `args`, `prompt`, `error` and `file_path` repeat
//! `data` with gitagent 1.x names, so 1.x hook scripts keep working) and
//! answers on stdout with `{"action": "allow"}`, `{"action": "block",
//! "reason": "…"}` or `{"action": "modify", "modifications": {…}}`, where
//! `modifications` replaces fields of `data` (1.x's `"args": {…}` also
//! works for tool arguments). Empty output allows.
//!
//! A hook that crashes, exits non-zero, times out or prints something that
//! is not such an answer has failed: the action is blocked, unless the hook
//! says `fail_open: true`. With several hooks for an event they run in file
//! order, each seeing the changes of the ones before; the first block wins.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use gitagent_loader::{v1, HookEntry, HookEvent};
use serde_json::{json, Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::Decision;

pub struct AgentHooks {
    entries: Vec<HookEntry>,
    root: PathBuf,
    session: Mutex<Session>,
}

#[derive(Clone)]
struct Session {
    id: String,
    agent: String,
    model: String,
    turn: u32,
}

enum Answer {
    Allow,
    Block(String),
    Modify(Map<String, Value>),
}

impl AgentHooks {
    /// The hooks of an agent in `root`. With `all` false (hooks turned off)
    /// only the compliance hooks are kept. `None` when no hook is left.
    pub fn new(
        entries: &[HookEntry],
        root: &Path,
        session_id: &str,
        agent: &str,
        model: &str,
        all: bool,
    ) -> Option<AgentHooks> {
        let entries: Vec<HookEntry> = entries
            .iter()
            .filter(|e| all || e.compliance)
            .cloned()
            .collect();
        (!entries.is_empty()).then(|| AgentHooks {
            entries,
            root: root.to_path_buf(),
            session: Mutex::new(Session {
                id: session_id.to_string(),
                agent: agent.to_string(),
                model: model.to_string(),
                turn: 0,
            }),
        })
    }

    /// The turn now running and the model answering it (a fallback model
    /// when the first one failed).
    pub fn set_turn(&self, turn: u32, model: &str) {
        let mut s = self.session.lock().unwrap_or_else(|e| e.into_inner());
        s.turn = turn;
        s.model = model.to_string();
    }

    pub fn has(&self, event: HookEvent) -> bool {
        self.entries.iter().any(|e| e.event == event)
    }

    /// Runs the hooks of `event` (those for `tool`, for tool events) on
    /// `data`. `Modify` carries the changed data.
    pub async fn fire(
        &self,
        event: HookEvent,
        tool: Option<&str>,
        mut data: Map<String, Value>,
    ) -> Decision {
        let mut changed = false;
        let hooks = self.entries.iter().filter(|e| {
            e.event == event
                && (e.applies_to.is_empty()
                    || tool.is_some_and(|t| e.applies_to.iter().any(|a| a == t)))
        });
        for hook in hooks {
            let input = self.input(event, &data);
            match self.call(hook, &input).await {
                Ok(Answer::Allow) => {}
                Ok(Answer::Block(reason)) => return Decision::Block { reason },
                Ok(Answer::Modify(m)) => {
                    let m = match (m.get("args"), m.get("modifications"), tool) {
                        // 1.x: new tool arguments under 1.x names.
                        (Some(Value::Object(args)), None, Some(t)) => {
                            let mut out = Map::new();
                            out.insert(
                                "arguments".into(),
                                Value::Object(v1::from_v1_input(t, args)),
                            );
                            out
                        }
                        (_, Some(Value::Object(mods)), _) => mods.clone(),
                        _ => {
                            if hook.fail_open {
                                continue;
                            }
                            return Decision::Block {
                                reason: format!(
                                    "hook {} answered modify without `modifications`",
                                    hook.script
                                ),
                            };
                        }
                    };
                    for (k, v) in m {
                        if k != "tool_name" {
                            data.insert(k, v);
                        }
                    }
                    changed = true;
                }
                Err(why) if hook.fail_open => {
                    eprintln!(
                        "gitagent: hook {} ({}) failed: {why}; going on (fail_open)",
                        hook.script,
                        event.name()
                    );
                }
                Err(why) => {
                    return Decision::Block {
                        reason: format!("hook {} failed: {why}", hook.script),
                    }
                }
            }
        }
        if changed {
            Decision::Modify(Value::Object(data))
        } else {
            Decision::Allow
        }
    }

    fn input(&self, event: HookEvent, data: &Map<String, Value>) -> Value {
        let s = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut input = json!({
            "event": event.name(),
            "timestamp": gitagent_record::utc_now_rfc3339(),
            "data": data,
            "session": {"id": s.id, "agent": s.agent, "model_version": s.model, "turn": s.turn},
            "session_id": s.id,
        });
        // The same facts under the names 1.x hook scripts read.
        if let Some(tool) = data.get("tool_name").and_then(Value::as_str) {
            input["tool"] = json!(v1::v1_name(tool));
            if let Some(Value::Object(args)) = data.get("arguments") {
                input["args"] = Value::Object(v1::to_v1_input(tool, args));
            }
        }
        for key in ["prompt", "error", "file_path"] {
            if let Some(v) = data.get(key) {
                input[key] = v.clone();
            }
        }
        if event == HookEvent::SessionStart {
            input["agent"] = json!(s.agent);
        }
        input
    }

    async fn call(&self, hook: &HookEntry, input: &Value) -> Result<Answer, String> {
        let (program, rest) = hook.command.split_first().ok_or("no command")?;
        let session_id = input["session_id"].as_str().unwrap_or_default();
        let mut cmd = Command::new(program);
        cmd.args(rest)
            .current_dir(&self.root)
            .env("GITAGENT_AGENT_DIR", &self.root)
            .env("GITAGENT_SESSION_ID", session_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if std::env::var_os("GITAGENT_AUDIT_DIR").is_none() {
            cmd.env("GITAGENT_AUDIT_DIR", self.root.join(".gitagent/audit"));
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start {program}: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.to_string().as_bytes()).await;
        }
        let out = match tokio::time::timeout(hook.timeout, child.wait_with_output()).await {
            Ok(r) => r.map_err(|e| e.to_string())?,
            Err(_) => return Err(format!("timed out after {}s", hook.timeout.as_secs())),
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("exited with {}: {}", out.status, stderr.trim()));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            return Ok(Answer::Allow);
        }
        let answer: Value = serde_json::from_str(stdout.trim())
            .map_err(|_| "printed something that is not JSON".to_string())?;
        let reason = || {
            answer["reason"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("blocked by hook {}", hook.script))
        };
        match answer["action"].as_str().or(answer["decision"].as_str()) {
            Some("allow") => Ok(Answer::Allow),
            Some("block") => Ok(Answer::Block(reason())),
            Some("modify") => Ok(Answer::Modify(
                answer.as_object().cloned().unwrap_or_default(),
            )),
            Some(other) => Err(format!("unknown action {other:?}")),
            None => Err("the answer has no `action`".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn hook(dir: &Path, event: HookEvent, body: &str, fail_open: bool) -> HookEntry {
        let n = std::fs::read_dir(dir).unwrap().count();
        let path = dir.join(format!("h{n}.sh"));
        std::fs::write(&path, body).unwrap();
        HookEntry {
            event,
            script: format!("h{n}.sh"),
            command: vec!["sh".into(), path.display().to_string()],
            description: String::new(),
            timeout: Duration::from_secs(5),
            compliance: false,
            fail_open,
            applies_to: Vec::new(),
        }
    }

    fn data(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn hooks(dir: &Path, entries: Vec<HookEntry>) -> AgentHooks {
        AgentHooks::new(&entries, dir, "s1", "lyra", "m", true).unwrap()
    }

    #[tokio::test]
    async fn answers_allow_block_and_chain_modifications() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let h = hooks(
            d,
            vec![
                // 1.x shape: reads `tool` and `args`, answers with `args`.
                hook(
                    d,
                    HookEvent::PreToolUse,
                    r#"input=$(cat)
case "$input" in
  *'"args":{"command":"sudo ls"}'*) echo '{"action":"modify","args":{"command":"ls"}}' ;;
  *'rm -rf'*) echo '{"action":"block","reason":"no rm -rf"}' ;;
esac"#,
                    false,
                ),
                // OpenGAP shape: sees the change of the hook before it.
                hook(
                    d,
                    HookEvent::PreToolUse,
                    r#"input=$(cat)
case "$input" in
  *'"arguments":{"cmd":"ls"}'*) echo '{"action":"modify","modifications":{"arguments":{"cmd":"ls -la"}}}' ;;
  *) echo '{"action":"allow"}' ;;
esac"#,
                    false,
                ),
            ],
        );
        let pre = |cmd: &str| data(json!({"tool_name": "exec", "arguments": {"cmd": cmd}}));
        assert_eq!(
            h.fire(HookEvent::PreToolUse, Some("exec"), pre("sudo ls"))
                .await,
            Decision::Modify(json!({"tool_name": "exec", "arguments": {"cmd": "ls -la"}}))
        );
        assert_eq!(
            h.fire(HookEvent::PreToolUse, Some("exec"), pre("rm -rf /"))
                .await,
            Decision::Block {
                reason: "no rm -rf".into()
            }
        );
        assert_eq!(
            h.fire(HookEvent::PreToolUse, Some("exec"), pre("pwd"))
                .await,
            Decision::Allow
        );
        // No hooks for the event: allowed without running anything.
        assert_eq!(
            h.fire(HookEvent::OnError, None, Map::new()).await,
            Decision::Allow
        );
    }

    #[tokio::test]
    async fn failures_block_unless_fail_open() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for (body, why) in [
            ("exit 1", "exited with"),
            ("echo not json", "not JSON"),
            ("echo '{\"ok\": true}'", "no `action`"),
            ("sleep 3", "timed out"),
        ] {
            let mut e = hook(d, HookEvent::PreQuery, body, false);
            e.timeout = Duration::from_secs(1);
            let h = hooks(d, vec![e.clone()]);
            match h.fire(HookEvent::PreQuery, None, Map::new()).await {
                Decision::Block { reason } => assert!(reason.contains(why), "{reason}"),
                other => panic!("{body}: {other:?}"),
            }
            e.fail_open = true;
            let h = hooks(d, vec![e]);
            assert_eq!(
                h.fire(HookEvent::PreQuery, None, Map::new()).await,
                Decision::Allow
            );
        }
    }

    #[tokio::test]
    async fn input_carries_session_and_hooks_filter_by_tool() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let out = d.join("seen.json");
        let mut e = hook(
            d,
            HookEvent::PostToolFailure,
            &format!("cat > {}", out.display()),
            false,
        );
        e.applies_to = vec!["write_file".into()];
        let h = hooks(d, vec![e]);
        h.set_turn(4, "fallback-model");
        let failure =
            data(json!({"tool_name": "exec", "arguments": {"cmd": "x"}, "error": "boom"}));
        h.fire(HookEvent::PostToolFailure, Some("exec"), failure.clone())
            .await;
        assert!(!out.exists());
        h.fire(HookEvent::PostToolFailure, Some("write_file"), failure)
            .await;
        let seen: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(seen["event"], "post_tool_failure");
        assert_eq!(
            seen["session"],
            json!({"id": "s1", "agent": "lyra", "model_version": "fallback-model", "turn": 4})
        );
        assert_eq!(seen["error"], "boom");
        assert_eq!(seen["tool"], "cli");
        assert_eq!(seen["args"], json!({"command": "x"}));
    }

    #[test]
    fn only_compliance_hooks_survive_when_hooks_are_off() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let plain = hook(d, HookEvent::PreQuery, "true", false);
        assert!(AgentHooks::new(std::slice::from_ref(&plain), d, "s", "a", "m", false).is_none());
        let mut required = plain.clone();
        required.compliance = true;
        let h = AgentHooks::new(&[plain, required], d, "s", "a", "m", false).unwrap();
        assert_eq!(h.entries.len(), 1);
        assert!(h.has(HookEvent::PreQuery) && !h.has(HookEvent::OnError));
    }
}
