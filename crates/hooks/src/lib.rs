//! Tool hooks: scripts that see every tool call before and after it runs and
//! may allow, block or change it (BLUEPRINT §4). Two formats are read:
//!
//! **gitagent scripts**: `hooks/pre_tool_use` and `hooks/post_tool_use` in a
//! config directory. A script gets one JSON object on stdin:
//!
//! ```json
//! {"event": "pre_tool_use", "tool": "exec", "arguments": {"cmd": "rm -rf build"}}
//! {"event": "post_tool_use", "tool": "exec", "arguments": {...}, "output": "...", "is_error": false}
//! ```
//!
//! and answers on stdout with `{"decision": "allow"}`, `{"decision": "block",
//! "reason": "..."}` or `{"decision": "modify", "arguments": {...}}` (pre) /
//! `{"decision": "modify", "output": "..."}` (post). Empty output means allow.
//! A non-zero exit or a timeout blocks the call: a broken guard must not let
//! calls through.
//!
//! **`.claude/` settings hooks**: the `hooks` object of `settings.json` and
//! `settings.local.json` in a `.claude/` directory, with the protocol those
//! hook scripts expect, so they work unchanged: `PreToolUse` and
//! `PostToolUse` groups with a `matcher` (`*`, exact names such as
//! `Edit|Write`, or a regex) and `command` handlers; stdin carries
//! `hook_event_name`, `tool_name`, `tool_input` and `tool_use_id` with the
//! `.claude/` tool and argument names (`Bash` / `command` for gitagent's `exec` /
//! `cmd`); exit 2 blocks with stderr as the reason, other non-zero exits are
//! ignored; on exit 0 the JSON answer's `hookSpecificOutput.permissionDecision`
//! (`allow` / `deny` / `ask`), `updatedInput`, `updatedToolOutput`, and
//! top-level `decision: "block"` or `continue: false` are honoured.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use gitagent_loader::sdk;
use serde_json::{json, Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
/// The default timeout for a settings-file hook.
const SETTINGS_TIMEOUT: Duration = Duration::from_secs(600);

/// Hooks given in code (the library's `hooks` option) rather than as files. The
/// loop asks them after the file hooks, in the same way.
#[async_trait::async_trait]
pub trait ToolHooks: Send + Sync {
    async fn pre(&self, tool: &str, arguments: &Map<String, Value>) -> Decision;
    async fn post(
        &self,
        tool: &str,
        arguments: &Map<String, Value>,
        output: &str,
        is_error: bool,
    ) -> Decision;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Block {
        reason: String,
    },
    /// New arguments (pre) or new output (post).
    Modify(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Pre,
    Post,
}

impl Event {
    fn sdk_name(self) -> &'static str {
        match self {
            Event::Pre => "PreToolUse",
            Event::Post => "PostToolUse",
        }
    }
}

/// A settings-file hook matcher: `*` or empty for every tool, exact names
/// separated by `|` or `,`, or a regular expression. Tools match by their
/// gitagent name or their `.claude/` name.
#[derive(Debug, Clone)]
pub enum Matcher {
    All,
    Names(Vec<String>),
    Pattern(regex::Regex),
}

impl Matcher {
    pub fn parse(text: Option<&str>) -> Option<Matcher> {
        let text = text.unwrap_or("").trim();
        if text.is_empty() || text == "*" {
            return Some(Matcher::All);
        }
        let plain = text
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' ' | ',' | '|'));
        if plain {
            Some(Matcher::Names(
                text.split(['|', ','])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            ))
        } else {
            regex::Regex::new(text).ok().map(Matcher::Pattern)
        }
    }

    pub fn matches(&self, tool: &str) -> bool {
        let names = [tool, sdk::sdk_name(tool)];
        match self {
            Matcher::All => true,
            Matcher::Names(list) => names.iter().any(|n| list.iter().any(|l| l == n)),
            Matcher::Pattern(re) => names.iter().any(|n| re.is_match(n)),
        }
    }
}

/// One `command` handler from a settings file.
#[derive(Debug, Clone)]
struct SettingsHook {
    event: Event,
    matcher: Matcher,
    command: String,
    timeout: Duration,
    source: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Hooks {
    pre: Option<PathBuf>,
    post: Option<PathBuf>,
    settings: Vec<SettingsHook>,
    work_dir: PathBuf,
}

impl Hooks {
    /// Looks for the hook scripts in the first config directory that has
    /// each, and for settings-file hooks in every config directory. Returns
    /// `None` when there are none.
    pub fn discover(config_dirs: &[PathBuf], work_dir: &Path) -> Option<Hooks> {
        let find = |name: &str| {
            config_dirs.iter().find_map(|d| {
                [name.to_string(), format!("{name}.sh")]
                    .iter()
                    .map(|f| d.join("hooks").join(f))
                    .find(|p| p.is_file())
            })
        };
        let mut settings = Vec::new();
        for dir in config_dirs {
            for file in ["settings.json", "settings.local.json"] {
                let path = dir.join(file);
                if path.is_file() {
                    settings.extend(read_settings(&path));
                }
            }
        }
        let hooks = Hooks {
            pre: find("pre_tool_use"),
            post: find("post_tool_use"),
            settings,
            work_dir: work_dir.to_path_buf(),
        };
        (hooks.pre.is_some() || hooks.post.is_some() || !hooks.settings.is_empty()).then_some(hooks)
    }

    /// How many settings-file hooks were found (for logs and tests).
    pub fn settings_count(&self) -> usize {
        self.settings.len()
    }

    pub async fn pre(&self, tool: &str, arguments: &Map<String, Value>) -> Decision {
        let mut args = arguments.clone();
        let mut changed = false;
        if let Some(script) = &self.pre {
            let input = json!({"event": "pre_tool_use", "tool": tool, "arguments": args});
            match self.call_script(script, &input, "arguments").await {
                Decision::Allow => {}
                Decision::Modify(Value::Object(new_args)) => {
                    args = new_args;
                    changed = true;
                }
                other => return other,
            }
        }
        for hook in self
            .settings
            .iter()
            .filter(|h| h.event == Event::Pre && h.matcher.matches(tool))
        {
            let input = json!({
                "session_id": "",
                "cwd": self.work_dir,
                "hook_event_name": "PreToolUse",
                "tool_name": sdk::sdk_name(tool),
                "tool_input": sdk::to_sdk_input(tool, &args),
                "tool_use_id": "",
            });
            match self.call_settings(hook, &input).await {
                Decision::Allow => {}
                Decision::Modify(Value::Object(sdk_args)) => {
                    args = sdk::from_sdk_input(tool, &sdk_args);
                    changed = true;
                }
                other => return other,
            }
        }
        if changed {
            Decision::Modify(Value::Object(args))
        } else {
            Decision::Allow
        }
    }

    pub async fn post(
        &self,
        tool: &str,
        arguments: &Map<String, Value>,
        output: &str,
        is_error: bool,
    ) -> Decision {
        let mut out = output.to_string();
        let mut changed = false;
        if let Some(script) = &self.post {
            let input = json!({
                "event": "post_tool_use",
                "tool": tool,
                "arguments": arguments,
                "output": out,
                "is_error": is_error,
            });
            match self.call_script(script, &input, "output").await {
                Decision::Allow => {}
                Decision::Modify(Value::String(text)) => {
                    out = text;
                    changed = true;
                }
                other => return other,
            }
        }
        for hook in self
            .settings
            .iter()
            .filter(|h| h.event == Event::Post && h.matcher.matches(tool))
        {
            let input = json!({
                "session_id": "",
                "cwd": self.work_dir,
                "hook_event_name": "PostToolUse",
                "tool_name": sdk::sdk_name(tool),
                "tool_input": sdk::to_sdk_input(tool, arguments),
                "tool_use_id": "",
                "tool_response": {"output": out, "is_error": is_error},
            });
            match self.call_settings(hook, &input).await {
                Decision::Allow => {}
                Decision::Modify(Value::String(text)) => {
                    out = text;
                    changed = true;
                }
                other => return other,
            }
        }
        if changed {
            Decision::Modify(Value::String(out))
        } else {
            Decision::Allow
        }
    }

    async fn run(
        &self,
        mut cmd: Command,
        input: &Value,
        timeout: Duration,
        what: &str,
    ) -> Result<std::process::Output, Decision> {
        cmd.current_dir(&self.work_dir)
            .env("CLAUDE_PROJECT_DIR", &self.work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| Decision::Block {
            reason: format!("hook {what} could not start: {e}"),
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.to_string().as_bytes()).await;
        }
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(e)) => Err(Decision::Block {
                reason: format!("hook {what} failed: {e}"),
            }),
            Err(_) => Err(Decision::Block {
                reason: format!("hook {what} timed out"),
            }),
        }
    }

    /// A gitagent script hook: any failure blocks.
    async fn call_script(&self, script: &Path, input: &Value, modify_field: &str) -> Decision {
        let mut cmd = Command::new("sh");
        cmd.arg(script);
        let what = script.display().to_string();
        let out = match self.run(cmd, input, SCRIPT_TIMEOUT, &what).await {
            Ok(out) => out,
            Err(d) => return d,
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Decision::Block {
                reason: if stderr.is_empty() {
                    format!("hook {what} exited with {}", out.status)
                } else {
                    stderr
                },
            };
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            return Decision::Allow;
        }
        let Ok(answer) = serde_json::from_str::<Value>(stdout.trim()) else {
            return Decision::Block {
                reason: format!("hook {what} printed something that is not JSON"),
            };
        };
        match answer["decision"].as_str() {
            Some("allow") | None => Decision::Allow,
            Some("block") => Decision::Block {
                reason: answer["reason"]
                    .as_str()
                    .unwrap_or("blocked by a hook")
                    .to_string(),
            },
            Some("modify") if !answer[modify_field].is_null() => {
                Decision::Modify(answer[modify_field].clone())
            }
            Some(other) => Decision::Block {
                reason: format!("hook {what} gave an unusable decision {other:?}"),
            },
        }
    }

    /// A settings-file hook, with the exit-code and JSON rules its scripts expect.
    async fn call_settings(&self, hook: &SettingsHook, input: &Value) -> Decision {
        let command = hook
            .command
            .replace("${CLAUDE_PROJECT_DIR}", &self.work_dir.to_string_lossy())
            .replace("$CLAUDE_PROJECT_DIR", &self.work_dir.to_string_lossy());
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
        let what = format!("{} ({})", hook.command, hook.source.display());
        let out = match self.run(cmd, input, hook.timeout, &what).await {
            Ok(out) => out,
            Err(d) => return d,
        };
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        match out.status.code() {
            Some(0) => {}
            Some(2) => {
                return Decision::Block {
                    reason: if stderr.is_empty() {
                        "blocked by a hook".into()
                    } else {
                        stderr
                    },
                }
            }
            // Other failures are non-blocking.
            _ => return Decision::Allow,
        }
        let Ok(answer) = serde_json::from_str::<Value>(&stdout) else {
            return Decision::Allow;
        };
        let reason = |keys: &[&str], default: &str| {
            keys.iter()
                .find_map(|k| answer.pointer(k).and_then(Value::as_str))
                .unwrap_or(default)
                .to_string()
        };
        if answer["continue"] == Value::Bool(false) {
            return Decision::Block {
                reason: reason(&["/stopReason"], "stopped by a hook"),
            };
        }
        if answer["decision"] == "block" {
            return Decision::Block {
                reason: reason(&["/reason"], "blocked by a hook"),
            };
        }
        let specific = &answer["hookSpecificOutput"];
        match hook.event {
            Event::Pre => {
                match specific["permissionDecision"].as_str() {
                    Some("deny") | Some("ask") => {
                        return Decision::Block {
                            reason: reason(
                                &["/hookSpecificOutput/permissionDecisionReason"],
                                "denied by a hook",
                            ),
                        }
                    }
                    _ => {}
                }
                match &specific["updatedInput"] {
                    Value::Object(args) => Decision::Modify(Value::Object(args.clone())),
                    _ => Decision::Allow,
                }
            }
            Event::Post => match specific["updatedToolOutput"].as_str() {
                Some(text) => Decision::Modify(Value::String(text.to_string())),
                None => Decision::Allow,
            },
        }
    }
}

/// `command` handlers for PreToolUse and PostToolUse from one settings file.
fn read_settings(path: &Path) -> Vec<SettingsHook> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for event in [Event::Pre, Event::Post] {
        for group in root["hooks"][event.sdk_name()]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(matcher) = Matcher::parse(group["matcher"].as_str()) else {
                continue;
            };
            for handler in group["hooks"].as_array().into_iter().flatten() {
                if handler["type"].as_str().unwrap_or("command") != "command" {
                    continue;
                }
                let Some(command) = handler["command"].as_str() else {
                    continue;
                };
                let timeout = handler["timeout"]
                    .as_f64()
                    .map(Duration::from_secs_f64)
                    .unwrap_or(SETTINGS_TIMEOUT);
                out.push(SettingsHook {
                    event,
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    timeout,
                    source: path.to_path_buf(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_hooks(pre: Option<&str>, post: Option<&str>) -> (tempfile::TempDir, Hooks) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("hooks")).unwrap();
        if let Some(s) = pre {
            std::fs::write(dir.path().join("hooks/pre_tool_use"), s).unwrap();
        }
        if let Some(s) = post {
            std::fs::write(dir.path().join("hooks/post_tool_use.sh"), s).unwrap();
        }
        let hooks = Hooks::discover(&[dir.path().to_path_buf()], dir.path()).unwrap();
        (dir, hooks)
    }

    fn args(cmd: &str) -> Map<String, Value> {
        json!({"cmd": cmd}).as_object().unwrap().clone()
    }

    #[tokio::test]
    async fn pre_hook_can_allow_block_and_modify() {
        let script = r#"input=$(cat)
case "$input" in
  *rm\ -rf*) echo '{"decision":"block","reason":"no rm -rf"}' ;;
  *sudo*) echo '{"decision":"modify","arguments":{"cmd":"echo no sudo"}}' ;;
  *) ;;
esac"#;
        let (_d, h) = with_hooks(Some(script), None);
        assert_eq!(h.pre("exec", &args("ls")).await, Decision::Allow);
        assert_eq!(
            h.pre("exec", &args("rm -rf /")).await,
            Decision::Block {
                reason: "no rm -rf".into()
            }
        );
        assert_eq!(
            h.pre("exec", &args("sudo ls")).await,
            Decision::Modify(json!({"cmd": "echo no sudo"}))
        );
        // No post hook: everything passes.
        assert_eq!(
            h.post("exec", &args("ls"), "x", false).await,
            Decision::Allow
        );
    }

    #[tokio::test]
    async fn post_hook_can_rewrite_output() {
        let script = r#"cat >/dev/null; echo '{"decision":"modify","output":"[redacted]"}'"#;
        let (_d, h) = with_hooks(None, Some(script));
        assert_eq!(
            h.post("exec", &args("cat secrets"), "token=abc", false)
                .await,
            Decision::Modify(json!("[redacted]"))
        );
    }

    #[tokio::test]
    async fn failures_block() {
        let (_d, h) = with_hooks(
            Some("cat >/dev/null; echo 'denied by policy' >&2; exit 2"),
            None,
        );
        assert_eq!(
            h.pre("exec", &args("ls")).await,
            Decision::Block {
                reason: "denied by policy".into()
            }
        );
        let (_d, h) = with_hooks(Some("cat >/dev/null; echo not json"), None);
        assert!(matches!(
            h.pre("exec", &args("ls")).await,
            Decision::Block { .. }
        ));
    }

    #[test]
    fn no_hook_files_means_no_hooks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Hooks::discover(&[dir.path().to_path_buf()], dir.path()).is_none());
    }

    /// A `.claude/settings.json` with hooks in the `.claude/` format.
    fn with_settings(settings: &str, scripts: &[(&str, &str)]) -> (tempfile::TempDir, Hooks) {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        std::fs::create_dir_all(claude.join("hooks")).unwrap();
        std::fs::write(claude.join("settings.json"), settings).unwrap();
        for (name, body) in scripts {
            std::fs::write(claude.join("hooks").join(name), body).unwrap();
        }
        let hooks = Hooks::discover(&[claude], dir.path()).unwrap();
        (dir, hooks)
    }

    #[tokio::test]
    async fn sdk_settings_hooks_follow_the_sdk_protocol() {
        let settings = r#"{
          "hooks": {
            "PreToolUse": [
              {"matcher": "Bash", "hooks": [
                {"type": "command", "command": "sh ${CLAUDE_PROJECT_DIR}/.claude/hooks/guard.sh", "timeout": 10}
              ]},
              {"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "sh .claude/hooks/deny.sh"}]},
              {"matcher": "mcp__.*", "hooks": [{"type": "command", "command": "exit 2"}]}
            ],
            "PostToolUse": [
              {"matcher": "*", "hooks": [{"type": "command", "command": "sh .claude/hooks/post.sh"}]}
            ]
          }
        }"#;
        // The guard sees `.claude/` names (`Bash`, `command`) and answers in the hook JSON.
        let guard = r#"input=$(cat)
case "$input" in
  *'"tool_name":"Bash"'*) ;;
  *) echo "expected the .claude/ tool name, got: $input" >&2; exit 2 ;;
esac
case "$input" in
  *'"command":"rm -rf'*) echo 'Database writes are not allowed' >&2; exit 2 ;;
  *npm\ test*) echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"npm test --silent"}}}' ;;
  *secret*) echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"no secrets"}}' ;;
  *flaky*) echo oops >&2; exit 1 ;;
  *) echo 'not json at all' ;;
esac"#;
        let deny =
            "cat >/dev/null; echo '{\"decision\":\"block\",\"reason\":\"read-only project\"}'";
        let post = r#"input=$(cat)
case "$input" in
  *'"tool_name":"Bash"'*) echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"[trimmed]"}}' ;;
  *) ;;
esac"#;
        let (_d, h) = with_settings(
            settings,
            &[("guard.sh", guard), ("deny.sh", deny), ("post.sh", post)],
        );
        assert_eq!(h.settings_count(), 4);

        assert_eq!(h.pre("exec", &args("ls")).await, Decision::Allow);
        assert_eq!(
            h.pre("exec", &args("rm -rf /")).await,
            Decision::Block {
                reason: "Database writes are not allowed".into()
            }
        );
        // updatedInput comes back in gitagent's argument names.
        assert_eq!(
            h.pre("exec", &args("npm test")).await,
            Decision::Modify(json!({"cmd": "npm test --silent"}))
        );
        assert_eq!(
            h.pre("exec", &args("cat secret")).await,
            Decision::Block {
                reason: "no secrets".into()
            }
        );
        // A non-2 failure is non-blocking.
        assert_eq!(h.pre("exec", &args("flaky")).await, Decision::Allow);
        // `Edit|Write` matches gitagent's edit tools by their `.claude/` names.
        let edit = json!({"path": "a.py", "old_str": "x", "new_str": "y"})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            h.pre("str_replace", &edit).await,
            Decision::Block {
                reason: "read-only project".into()
            }
        );
        assert_eq!(
            h.pre(
                "read_file",
                &json!({"path": "a"}).as_object().unwrap().clone()
            )
            .await,
            Decision::Allow
        );
        // The regex matcher never matches our tools, so its `exit 2` never fires.
        assert_eq!(h.pre("task_complete", &Map::new()).await, Decision::Allow);

        assert_eq!(
            h.post("exec", &args("ls"), "long output", false).await,
            Decision::Modify(json!("[trimmed]"))
        );
        assert_eq!(
            h.post("read_file", &Map::new(), "text", false).await,
            Decision::Allow
        );
    }

    #[tokio::test]
    async fn script_and_settings_hooks_chain() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".gitagent/hooks")).unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".gitagent/hooks/pre_tool_use"),
            "cat >/dev/null; echo '{\"decision\":\"modify\",\"arguments\":{\"cmd\":\"ls -a\"}}'",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"input=$(cat); case \"$input\" in *'ls -a'*) echo '{\"hookSpecificOutput\":{\"updatedInput\":{\"command\":\"ls -la\"}}}';; esac"}]}]}}"#,
        )
        .unwrap();
        let h = Hooks::discover(
            &[dir.path().join(".gitagent"), dir.path().join(".claude")],
            dir.path(),
        )
        .unwrap();
        // The script's change is what the settings hook sees, and both apply.
        assert_eq!(
            h.pre("exec", &args("ls")).await,
            Decision::Modify(json!({"cmd": "ls -la"}))
        );
    }
}
