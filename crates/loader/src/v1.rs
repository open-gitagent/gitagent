//! The tool and argument names of gitagent 1.x (`cli`, `read`, `write`,
//! `edit`, `memory`, `task_tracker`), so that tool lists, hook scripts and
//! client code written for them keep working.
//!
//! As with the `.claude/` names, gitagent's own names stay what the model
//! sees; these tables translate where users write names: allow and deny
//! lists, hook payloads and hook answers.

use serde_json::{Map, Value};

/// gitagent tools that a 1.x tool name stands for. `capture_photo` and
/// `skill_learner` have no equivalent and give an empty list.
pub fn gitagent_tools_for(v1_name: &str) -> &'static [&'static str] {
    match v1_name {
        "cli" => &["exec", "write_stdin"],
        "read" => &["read_file", "image_read"],
        "write" => &["write_file"],
        "edit" => &["str_replace", "apply_patch"],
        "memory" => &["memory"],
        "task_tracker" => &["todo"],
        _ => &[],
    }
}

/// Whether `name` is a 1.x tool name (including those with no equivalent).
pub fn is_v1_tool(name: &str) -> bool {
    matches!(
        name,
        "cli"
            | "read"
            | "write"
            | "edit"
            | "memory"
            | "task_tracker"
            | "capture_photo"
            | "skill_learner"
    )
}

/// The 1.x name for a gitagent tool; tools without one keep their own.
pub fn v1_name(tool: &str) -> &str {
    match tool {
        "exec" | "write_stdin" => "cli",
        "read_file" | "image_read" => "read",
        "write_file" => "write",
        "str_replace" | "apply_patch" => "edit",
        "todo" => "task_tracker",
        other => other,
    }
}

/// A tool call's arguments with the 1.x argument names. `cli` takes its
/// timeout in seconds, `exec` its wait in milliseconds.
pub fn to_v1_input(tool: &str, args: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in args {
        let (key, value) = match (tool, k.as_str()) {
            ("exec", "cmd") => ("command", v.clone()),
            ("exec", "yield_ms") => ("timeout", millis_to_secs(v)),
            ("str_replace", "old_str") => ("old_string", v.clone()),
            ("str_replace", "new_str") => ("new_string", v.clone()),
            _ => (k.as_str(), v.clone()),
        };
        out.insert(key.to_string(), value);
    }
    out
}

/// Arguments written with the 1.x names, as gitagent's.
pub fn from_v1_input(tool: &str, args: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in args {
        let (key, value) = match (tool, k.as_str()) {
            ("exec", "command") => ("cmd", v.clone()),
            ("exec", "timeout") => ("yield_ms", secs_to_millis(v)),
            ("str_replace", "old_string") => ("old_str", v.clone()),
            ("str_replace", "new_string") => ("new_str", v.clone()),
            _ => (k.as_str(), v.clone()),
        };
        out.insert(key.to_string(), value);
    }
    out
}

fn millis_to_secs(v: &Value) -> Value {
    match v.as_f64() {
        Some(ms) => serde_json::json!((ms / 1000.0).round() as u64),
        None => v.clone(),
    }
}

fn secs_to_millis(v: &Value) -> Value {
    match v.as_f64() {
        Some(s) => serde_json::json!((s * 1000.0).round() as u64),
        None => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn names_map_both_ways() {
        assert_eq!(gitagent_tools_for("cli"), ["exec", "write_stdin"]);
        assert_eq!(gitagent_tools_for("edit"), ["str_replace", "apply_patch"]);
        assert!(gitagent_tools_for("capture_photo").is_empty());
        assert!(is_v1_tool("skill_learner") && !is_v1_tool("exec"));
        for (ours, theirs) in [
            ("exec", "cli"),
            ("write_stdin", "cli"),
            ("image_read", "read"),
            ("apply_patch", "edit"),
            ("todo", "task_tracker"),
            ("task_complete", "task_complete"),
        ] {
            assert_eq!(v1_name(ours), theirs);
        }
    }

    #[test]
    fn arguments_and_units_translate_and_round_trip() {
        let ours = map(json!({"cmd": "make test", "yield_ms": 120000, "background": false}));
        let theirs = to_v1_input("exec", &ours);
        assert_eq!(
            Value::Object(theirs.clone()),
            json!({"command": "make test", "timeout": 120, "background": false})
        );
        assert_eq!(from_v1_input("exec", &theirs), ours);

        let edit = map(json!({"path": "a.py", "old_string": "x", "new_string": "y"}));
        assert_eq!(
            Value::Object(from_v1_input("str_replace", &edit)),
            json!({"path": "a.py", "old_str": "x", "new_str": "y"})
        );
    }
}
