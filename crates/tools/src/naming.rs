//! Tool definitions under the `.claude/` tool names (`tool_naming: sdk`):
//! `Bash` with `command`, `Read` with `file_path`, `Edit` with `old_string`
//! and `new_string`, … The model sees these; gitagent still runs its own
//! tools underneath (the loop maps each call back with
//! `gitagent_loader::sdk::gitagent_tool_for_call`).

use gitagent_loader::sdk;
use gitagent_provider::ToolSpec;
use serde_json::{Map, Value};

/// The same tools, renamed for the model. `image_read` folds into `Read`,
/// which then also returns images; tools without a `.claude/` name keep theirs.
pub fn sdk_specs(specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let has_images = specs.iter().any(|t| t.name == crate::IMAGE_READ);
    let mut out = Vec::new();
    for spec in specs {
        if spec.name == crate::IMAGE_READ {
            continue;
        }
        let renames = sdk::renames(&spec.name);
        let mut description = spec.description.clone();
        for (ours, theirs) in renames {
            description = replace_word(&description, ours, theirs);
        }
        if spec.name == crate::READ_FILE && has_images {
            description.push_str(
                " An image file (png, jpg, gif or webp) is returned as an image instead of text.",
            );
        }
        out.push(ToolSpec {
            name: sdk::sdk_name(&spec.name).to_string(),
            description,
            input_schema: rename_schema(spec.input_schema, renames),
        });
    }
    out
}

fn rename_schema(schema: Value, renames: &[(&str, &str)]) -> Value {
    let Value::Object(mut obj) = schema else {
        return schema;
    };
    let to = |k: &str| {
        renames
            .iter()
            .find(|(ours, _)| *ours == k)
            .map(|(_, theirs)| theirs.to_string())
            .unwrap_or_else(|| k.to_string())
    };
    if let Some(Value::Object(props)) = obj.remove("properties") {
        let mut renamed = Map::new();
        for (k, mut v) in props {
            if let Some(d) = v.get("description").and_then(Value::as_str) {
                let mut d = d.to_string();
                for (ours, theirs) in renames {
                    d = replace_word(&d, ours, theirs);
                }
                v["description"] = Value::String(d);
            }
            renamed.insert(to(&k), v);
        }
        obj.insert("properties".into(), Value::Object(renamed));
    }
    if let Some(Value::Array(req)) = obj.remove("required") {
        obj.insert(
            "required".into(),
            Value::Array(
                req.into_iter()
                    .map(|r| match r {
                        Value::String(s) => Value::String(to(&s)),
                        other => other,
                    })
                    .collect(),
            ),
        );
    }
    Value::Object(obj)
}

/// Replaces whole-word occurrences of an argument name in prose.
fn replace_word(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + from.len()..].chars().next();
        let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..i]);
        if is_word(before) || is_word(after) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitagent_loader::{EditTool, ToolsCfg};

    #[test]
    fn specs_take_sdk_names_and_argument_names() {
        let cfg = ToolsCfg {
            write_stdin: true,
            image_read: true,
            todo: false,
            subagents: false,
            subagent_depth: 1,
            max_concurrent_subagents: 4,
            plan: false,
        };
        let specs = sdk_specs(crate::specs(&cfg, EditTool::StrReplace));
        let names: Vec<&str> = specs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Bash",
                "write_stdin",
                "Read",
                "Write",
                "Edit",
                "task_complete"
            ]
        );
        let bash = &specs[0];
        let props = bash.input_schema["properties"].as_object().unwrap();
        assert!(props.contains_key("command") && props.contains_key("timeout"));
        assert!(!props.contains_key("cmd"));
        assert_eq!(
            bash.input_schema["required"],
            serde_json::json!(["command"])
        );
        let read = &specs[2];
        assert!(read.input_schema["properties"].get("file_path").is_some());
        assert!(read.description.contains("returned as an image"));
        let edit = &specs[4];
        let p = edit.input_schema["properties"].as_object().unwrap();
        assert!(
            p.contains_key("old_string")
                && p.contains_key("new_string")
                && p.contains_key("file_path")
        );
        assert_eq!(
            replace_word("the cmd (not cmds) `cmd`", "cmd", "command"),
            "the command (not cmds) `command`"
        );
    }
}
