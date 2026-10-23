//! An agent's lifecycle hooks: `hooks/hooks.yaml`, scripts that see events
//! (session start, each tool call, the final response, …) and may allow,
//! block or change what happens.
//!
//! ```yaml
//! hooks:
//!   pre_tool_use:
//!     - script: check-command.sh
//!       description: Refuses destructive commands
//!       timeout: 10        # seconds, 30 by default
//!       fail_open: false   # a hook that crashes or times out blocks
//!                          # (the default is true, as in 1.x: a warning, then on;
//!                          # compliance hooks always block)
//!       compliance: true   # runs even when hooks are turned off
//!       applies_to: [cli]  # tool names (any naming); all tools when absent
//! ```
//!
//! Script paths are relative to `hooks/` and must stay inside the agent
//! directory. Problems are warnings and the hook is left out.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::AgentDir;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    /// The user's prompt, before the model sees it (1.x `pre_query`).
    PreQuery,
    PreToolUse,
    PostToolUse,
    /// After a tool call that failed (1.x).
    PostToolFailure,
    /// After a file tool changed a file (1.x).
    FileChanged,
    /// The agent's final response, before it is delivered.
    PreResponse,
    PostResponse,
    OnError,
}

impl HookEvent {
    pub const ALL: [HookEvent; 10] = [
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::PreQuery,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PostToolFailure,
        HookEvent::FileChanged,
        HookEvent::PreResponse,
        HookEvent::PostResponse,
        HookEvent::OnError,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "on_session_start",
            HookEvent::SessionEnd => "on_session_end",
            HookEvent::PreQuery => "pre_query",
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::PostToolFailure => "post_tool_failure",
            HookEvent::FileChanged => "file_changed",
            HookEvent::PreResponse => "pre_response",
            HookEvent::PostResponse => "post_response",
            HookEvent::OnError => "on_error",
        }
    }

    /// The event with this name (`user_prompt_submit` counts as
    /// `pre_query`).
    pub fn parse(name: &str) -> Option<HookEvent> {
        if name == "user_prompt_submit" {
            return Some(HookEvent::PreQuery);
        }
        HookEvent::ALL.into_iter().find(|e| e.name() == name)
    }

    /// Events about one tool call; the others concern the whole session.
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            HookEvent::PreToolUse
                | HookEvent::PostToolUse
                | HookEvent::PostToolFailure
                | HookEvent::FileChanged
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookEntry {
    pub event: HookEvent,
    /// The script, as written in hooks.yaml (for messages).
    pub script: String,
    /// The program, its arguments and the script's path.
    pub command: Vec<String>,
    pub description: String,
    pub timeout: Duration,
    /// Required for compliance: runs even when hooks are turned off.
    pub compliance: bool,
    /// A failure (crash, timeout, unreadable answer) lets the action go on.
    pub fail_open: bool,
    /// gitagent tool names this hook is for; empty means every tool.
    pub applies_to: Vec<String>,
}

/// The hooks in an agent directory's `hooks/hooks.yaml`, in file order, and
/// what was wrong with the rest.
pub fn load(agent: &AgentDir) -> (Vec<HookEntry>, Vec<String>) {
    // The child's hooks, else the nearest parent's.
    let base = agent
        .file_root("hooks/hooks.yaml")
        .unwrap_or_else(|| agent.root.clone());
    let path = base.join("hooks/hooks.yaml");
    let mut warnings = Vec::new();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Vec::new(), warnings);
    };
    let root: Value = match serde_saphyr::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("hooks/hooks.yaml: not valid YAML: {e}"));
            return (Vec::new(), warnings);
        }
    };
    let Some(events) = root.get("hooks").and_then(Value::as_object) else {
        warnings.push("hooks/hooks.yaml: no `hooks:` mapping".into());
        return (Vec::new(), warnings);
    };
    let mut entries = Vec::new();
    for (name, list) in events {
        let Some(event) = HookEvent::parse(name) else {
            warnings.push(format!("hooks/hooks.yaml: unknown event {name:?}"));
            continue;
        };
        for (i, v) in list.as_array().into_iter().flatten().enumerate() {
            match entry(event, v, &base) {
                Ok(e) => entries.push(e),
                Err(why) => warnings.push(format!("hooks/hooks.yaml: {name}[{i}]: {why}")),
            }
        }
    }
    (entries, warnings)
}

/// The event named `name`, if there is one.
pub fn parse_event(name: &str) -> Option<HookEvent> {
    HookEvent::parse(name)
}

/// A hook from a script at `base/<script>` (a plugin's), with the default
/// settings.
pub fn entry_in(
    event: HookEvent,
    script: &str,
    description: &str,
    base: &Path,
) -> Result<HookEntry, String> {
    let script = script.trim();
    let path = base.join(script);
    if !path.is_file() {
        return Err(format!("the hook script {script} does not exist"));
    }
    let inside = match (path.canonicalize(), base.canonicalize()) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    };
    if !inside {
        return Err(format!("the hook script {script} is outside its directory"));
    }
    let mut command = crate::declared::interpreter(None, &path);
    command.push(path.display().to_string());
    Ok(HookEntry {
        event,
        script: script.to_string(),
        command,
        description: description.to_string(),
        timeout: DEFAULT_TIMEOUT,
        compliance: false,
        // As in 1.x, a failing plugin hook is a warning, not a block.
        fail_open: true,
        applies_to: Vec::new(),
    })
}

fn entry(event: HookEvent, v: &Value, root: &Path) -> Result<HookEntry, String> {
    let script = v
        .get("script")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("no `script`")?;
    let path = root.join("hooks").join(script);
    if !path.is_file() {
        return Err(format!("the script hooks/{script} does not exist"));
    }
    let inside = match (path.canonicalize(), root.canonicalize()) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    };
    if !inside {
        return Err(format!(
            "the script {script} is outside the agent directory"
        ));
    }
    let timeout = match v.get("timeout").or(v.get("timeout_seconds")) {
        None | Some(Value::Null) => DEFAULT_TIMEOUT,
        Some(t) => match t.as_u64() {
            Some(n) if n >= 1 => Duration::from_secs(n),
            _ => return Err("`timeout` must be a whole number of seconds".into()),
        },
    };
    let flag = |key: &str| v.get(key).and_then(Value::as_bool).unwrap_or(false);
    let applies_to: Vec<String> = v
        .get("applies_to")
        .and_then(Value::as_array)
        .map(|names| {
            let names: Vec<String> = names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            crate::sdk::normalize_tool_list(&names)
        })
        .unwrap_or_default();
    let mut command = crate::declared::interpreter(None, &path);
    command.push(path.display().to_string());
    Ok(HookEntry {
        event,
        script: script.to_string(),
        command,
        description: v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        timeout,
        compliance: flag("compliance"),
        // As in 1.x, a hook that fails is a warning, not a block, unless it
        // is a compliance hook or says otherwise.
        fail_open: v
            .get("fail_open")
            .and_then(Value::as_bool)
            .unwrap_or(!flag("compliance")),
        applies_to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_are_read_in_order_with_their_settings() {
        let dir = tempfile::tempdir().unwrap();
        let root = &dir.path().join("agent");
        std::fs::create_dir(root).unwrap();
        std::fs::write(dir.path().join("outside.sh"), "true\n").unwrap();
        std::fs::write(
            root.join("agent.yaml"),
            "name: t\nversion: 1.0.0\ndescription: d\n",
        )
        .unwrap();
        std::fs::write(root.join("SOUL.md"), "I test.\n").unwrap();
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        for f in ["a.sh", "b.py"] {
            std::fs::write(root.join("hooks").join(f), "true\n").unwrap();
        }
        std::fs::write(
            root.join("hooks/hooks.yaml"),
            "hooks:\n  pre_tool_use:\n    - {script: a.sh, description: A, timeout: 5, fail_open: true, applies_to: [cli, Write]}\n    - {script: b.py, description: B, compliance: true}\n  user_prompt_submit:\n    - {script: a.sh, description: Q}\n  on_lunch:\n    - {script: a.sh, description: X}\n  on_error:\n    - {script: ../../outside.sh, description: Escape}\n    - {script: missing.sh, description: M}\n",
        )
        .unwrap();
        let agent = AgentDir::load(root).unwrap().unwrap();
        let (hooks, warnings) = load(&agent);
        assert_eq!(hooks.len(), 3);
        let a = &hooks[0];
        assert_eq!(
            (a.event, a.script.as_str()),
            (HookEvent::PreToolUse, "a.sh")
        );
        assert_eq!(a.timeout, Duration::from_secs(5));
        assert!(a.fail_open && !a.compliance);
        assert_eq!(a.applies_to, vec!["exec", "write_stdin", "write_file"]);
        assert_eq!(a.command[0], "bash");
        let b = &hooks[1];
        assert_eq!(b.command[0], "python3");
        assert_eq!(b.timeout, DEFAULT_TIMEOUT);
        assert!(b.compliance && !b.fail_open && b.applies_to.is_empty());
        let q = &hooks[2];
        assert!(
            q.fail_open && !q.compliance,
            "plain hooks fail open as in 1.x"
        );
        assert_eq!(q.event, HookEvent::PreQuery);

        assert_eq!(warnings.len(), 3, "{warnings:?}");
        for expected in [
            "unknown event \"on_lunch\"",
            "outside the agent directory",
            "hooks/missing.sh does not exist",
        ] {
            assert!(
                warnings.iter().any(|w| w.contains(expected)),
                "{warnings:?}"
            );
        }
    }
}
