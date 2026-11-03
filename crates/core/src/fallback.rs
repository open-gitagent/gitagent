//! Tool calls written as text, for models that do not reliably use native
//! tool calling (profile `tool_calling: native_with_text_fallback`).
//!
//! Accepted forms, anywhere in the reply:
//! - `<tool_call>{"name": ..., "arguments": {...}}</tool_call>`
//! - JSON objects, fenced or bare, with a tool name and its arguments.
//!
//! Argument keys may be `arguments`, `parameters`, `args` or `input` (an object
//! or a JSON string); the name may be `name`, `tool` or `function`, or nested
//! as `{"function": {"name", "arguments"}}`. Only names of offered tools count.

use gitagent_provider::ToolCall;
use serde_json::{Map, Value};

pub fn parse_text_calls(text: &str, tool_names: &[&str], turn: u32) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for obj in json_objects(text) {
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(obj) else {
            continue;
        };
        if let Some((name, arguments)) = as_call(&map) {
            if tool_names.contains(&name.as_str()) {
                calls.push(ToolCall {
                    id: format!("text_call_{turn}_{}", calls.len() + 1),
                    name,
                    arguments,
                });
            }
        }
    }
    calls
}

fn as_call(map: &Map<String, Value>) -> Option<(String, Map<String, Value>)> {
    if let Some(Value::Object(f)) = map.get("function") {
        return as_call(f);
    }
    let name = ["name", "tool", "tool_name", "function"]
        .iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))?
        .to_string();
    let args = ["arguments", "parameters", "args", "input"]
        .iter()
        .find_map(|k| map.get(*k));
    let arguments = match args {
        None => Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(o)) => o,
            _ => return None,
        },
        Some(_) => return None,
    };
    Some((name, arguments))
}

/// Top-level balanced `{...}` spans in `text`, skipping braces inside strings.
fn json_objects(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        match balanced_end(bytes, i) {
            Some(end) => {
                out.push(&text[i..=end]);
                i = end + 1;
            }
            None => i += 1,
        }
    }
    out
}

fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: &[&str] = &["exec", "task_complete"];

    #[test]
    fn parses_tool_call_tags_and_fenced_json() {
        let text = "I'll list files.\n<tool_call>\n{\"name\": \"exec\", \"arguments\": {\"cmd\": \"ls -la\"}}\n</tool_call>";
        let calls = parse_text_calls(text, TOOLS, 3);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "text_call_3_1");
        assert_eq!(calls[0].name, "exec");
        assert_eq!(calls[0].arguments["cmd"], "ls -la");

        let fenced = "```json\n{\"tool\": \"exec\", \"parameters\": {\"cmd\": \"echo '{x}'\"}}\n```\nthen\n{\"name\":\"task_complete\"}";
        let calls = parse_text_calls(fenced, TOOLS, 1);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["cmd"], "echo '{x}'");
        assert_eq!(calls[1].name, "task_complete");
    }

    #[test]
    fn accepts_nested_function_objects_and_string_arguments() {
        let text = r#"{"type": "function", "function": {"name": "exec", "arguments": "{\"cmd\": \"pwd\"}"}}"#;
        let calls = parse_text_calls(text, TOOLS, 1);
        assert_eq!(calls[0].arguments["cmd"], "pwd");
    }

    #[test]
    fn ignores_unknown_tools_and_ordinary_json() {
        let text = r#"The config is {"port": 8080, "name": "web"} and {"name": "browse", "arguments": {}}. Unbalanced { here."#;
        assert!(parse_text_calls(text, TOOLS, 1).is_empty());
    }
}
