//! The tool and argument names used in `.claude/` agent files, hook settings
//! and hook scripts, so that those files work with gitagent unchanged.
//!
//! gitagent's own names (`exec`, `read_file`, …) stay what the model sees;
//! these tables translate at the edges: when reading `.claude/agents/*.md`,
//! when matching and calling hooks from `.claude/settings.json`, and when
//! applying a hook's `updatedInput`.

use serde_json::{Map, Value};

/// gitagent tools that a `.claude/` tool name stands for. Names with no
/// equivalent (Glob, Grep, WebFetch, …) give an empty list.
pub fn gitagent_tools_for(sdk_name: &str) -> &'static [&'static str] {
    match sdk_name {
        "Bash" => &["exec"],
        "Read" => &["read_file", "image_read"],
        "Write" => &["write_file"],
        "Edit" | "MultiEdit" => &["str_replace", "apply_patch"],
        "Agent" | "Task" => &["subagent"],
        "Skill" => &["skill"],
        "TodoWrite" => &["todo"],
        _ => &[],
    }
}

/// The `.claude/` name for a gitagent tool; tools without one keep their own.
pub fn sdk_name(tool: &str) -> &str {
    match tool {
        "exec" => "Bash",
        "read_file" | "image_read" => "Read",
        "write_file" => "Write",
        "str_replace" | "apply_patch" => "Edit",
        "subagent" => "Agent",
        "skill" => "Skill",
        "todo" => "TodoWrite",
        other => other,
    }
}

/// The model id an alias stands for (`sonnet`, `opus`, `haiku`,
/// `fable`); anything else is already an id.
pub fn model_alias(name: &str) -> &str {
    match name {
        "sonnet" => "claude-sonnet-5-5",
        "opus" => "claude-opus-5-5",
        "haiku" => "claude-haiku-5-5",
        "fable" => "claude-fable-5-1",
        other => other,
    }
}

/// A tool list as gitagent tool names: `.claude/` and 1.x names
/// (`cli`, `read`, `edit`, …) are expanded, gitagent names pass through,
/// names with no equivalent are dropped.
pub fn normalize_tool_list(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let mut mapped = gitagent_tools_for(name);
        if mapped.is_empty() && crate::v1::is_v1_tool(name) {
            mapped = crate::v1::gitagent_tools_for(name);
        }
        let expanded: Vec<String> = if mapped.is_empty() {
            if is_gitagent_tool(name) {
                vec![name.to_string()]
            } else {
                Vec::new()
            }
        } else {
            mapped.iter().map(|s| s.to_string()).collect()
        };
        for t in expanded {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

fn is_gitagent_tool(name: &str) -> bool {
    matches!(
        name,
        "exec"
            | "write_stdin"
            | "read_file"
            | "write_file"
            | "str_replace"
            | "apply_patch"
            | "image_read"
            | "todo"
            | "task_complete"
            | "skill"
            | "subagent"
            | "memory"
    )
}

/// The gitagent tool behind a call the model made under `.claude/` names, with
/// its arguments translated. `Read` of an image file is `image_read`;
/// `Edit` is the profile's edit tool. Names that are already gitagent's pass
/// through; unknown names come back unchanged.
pub fn gitagent_tool_for_call(
    name: &str,
    args: &Map<String, Value>,
) -> (String, Map<String, Value>) {
    let target: &str = match name {
        "Bash" => "exec",
        "Read" => {
            let path = args.get("file_path").and_then(Value::as_str).unwrap_or("");
            let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                "image_read"
            } else {
                "read_file"
            }
        }
        "Write" => "write_file",
        "Edit" | "MultiEdit" => "str_replace",
        "Agent" | "Task" => "subagent",
        "Skill" => "skill",
        "TodoWrite" => "todo",
        other => return (other.to_string(), args.clone()),
    };
    (target.to_string(), from_sdk_input(target, args))
}

/// (gitagent argument, `.claude/` argument) pairs for a gitagent tool.
pub fn renames(tool: &str) -> &'static [(&'static str, &'static str)] {
    match tool {
        "exec" => &[
            ("cmd", "command"),
            ("yield_ms", "timeout"),
            ("background", "run_in_background"),
        ],
        "read_file" | "image_read" | "write_file" => &[("path", "file_path")],
        "str_replace" => &[
            ("path", "file_path"),
            ("old_str", "old_string"),
            ("new_str", "new_string"),
        ],
        "subagent" => &[("agent", "subagent_type")],
        "skill" => &[("name", "skill")],
        _ => &[],
    }
}

/// A tool call's arguments with the `.claude/` argument names.
pub fn to_sdk_input(tool: &str, args: &Map<String, Value>) -> Map<String, Value> {
    rename(args, renames(tool).iter().map(|(ours, sdk)| (*ours, *sdk)))
}

/// Arguments written with the `.claude/` names, as gitagent's.
pub fn from_sdk_input(tool: &str, args: &Map<String, Value>) -> Map<String, Value> {
    rename(args, renames(tool).iter().map(|(ours, sdk)| (*sdk, *ours)))
}

fn rename<'a>(
    args: &Map<String, Value>,
    pairs: impl Iterator<Item = (&'a str, &'a str)>,
) -> Map<String, Value> {
    let pairs: Vec<(&str, &str)> = pairs.collect();
    let mut out = Map::new();
    for (k, v) in args {
        let key = pairs
            .iter()
            .find(|(from, _)| *from == k)
            .map(|(_, to)| to.to_string())
            .unwrap_or_else(|| k.clone());
        out.insert(key, v.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_lists_translate() {
        let list = |s: &str| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            normalize_tool_list(&list("Read, Grep, Bash, exec, Nope")),
            vec!["read_file", "image_read", "exec"]
        );
        assert_eq!(
            normalize_tool_list(&list("Edit")),
            vec!["str_replace", "apply_patch"]
        );
        // 1.x names, mixed with the others; duplicates collapse.
        assert_eq!(
            normalize_tool_list(&list("cli, read, edit, capture_photo, memory, Bash")),
            vec![
                "exec",
                "write_stdin",
                "read_file",
                "image_read",
                "str_replace",
                "apply_patch",
                "memory"
            ]
        );
        assert_eq!(sdk_name("exec"), "Bash");
        assert_eq!(sdk_name("task_complete"), "task_complete");
    }

    #[test]
    fn calls_under_sdk_names_map_back() {
        let args = |v: Value| v.as_object().unwrap().clone();
        let (t, a) =
            gitagent_tool_for_call("Bash", &args(json!({"command": "ls", "timeout": 100})));
        assert_eq!(
            (t.as_str(), Value::Object(a)),
            ("exec", json!({"cmd": "ls", "yield_ms": 100}))
        );
        let (t, a) = gitagent_tool_for_call("Read", &args(json!({"file_path": "/tmp/shot.PNG"})));
        assert_eq!(
            (t.as_str(), Value::Object(a)),
            ("image_read", json!({"path": "/tmp/shot.PNG"}))
        );
        let (t, _) =
            gitagent_tool_for_call("Read", &args(json!({"file_path": "notes.md", "offset": 3})));
        assert_eq!(t, "read_file");
        let (t, a) = gitagent_tool_for_call(
            "Edit",
            &args(json!({"file_path": "a", "old_string": "x", "new_string": "y"})),
        );
        assert_eq!(
            (t.as_str(), Value::Object(a)),
            (
                "str_replace",
                json!({"path": "a", "old_str": "x", "new_str": "y"})
            )
        );
        let (t, a) = gitagent_tool_for_call("task_complete", &args(json!({"summary": "s"})));
        assert_eq!(
            (t.as_str(), Value::Object(a)),
            ("task_complete", json!({"summary": "s"}))
        );
        let (t, _) = gitagent_tool_for_call("Glob", &Map::new());
        assert_eq!(t, "Glob");
    }

    #[test]
    fn arguments_translate_both_ways() {
        let ours = json!({"cmd": "ls", "yield_ms": 5000})
            .as_object()
            .unwrap()
            .clone();
        let sdk = to_sdk_input("exec", &ours);
        assert_eq!(
            Value::Object(sdk.clone()),
            json!({"command": "ls", "timeout": 5000})
        );
        assert_eq!(from_sdk_input("exec", &sdk), ours);
        let edit = json!({"file_path": "a.py", "old_string": "x", "new_string": "y"});
        assert_eq!(
            Value::Object(from_sdk_input("str_replace", edit.as_object().unwrap())),
            json!({"path": "a.py", "old_str": "x", "new_str": "y"})
        );
        // Unknown keys pass through untouched.
        let other = json!({"summary": "done"}).as_object().unwrap().clone();
        assert_eq!(to_sdk_input("task_complete", &other), other);
    }
}
