//! A checklist the model keeps for itself. Each call replaces the whole list
//! and shows it back, so the current plan is always in the latest tool result.

use std::fmt::Write as _;

use anyhow::Result;
use gitagent_provider::ToolCall;
use serde_json::Value;

use crate::{ArgError, ToolOutput};

pub(crate) fn run(call: &ToolCall) -> Result<ToolOutput> {
    let Some(Value::Array(items)) = call.arguments.get("items") else {
        return Err(ArgError("`items` must be a list of {text, status}".into()).into());
    };
    let mut out = String::new();
    let (mut done, mut total) = (0, 0);
    for item in items {
        let text = item["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let status = item["status"].as_str().unwrap_or("pending");
        let mark = match status {
            "done" => {
                done += 1;
                "[x]"
            }
            "in_progress" => "[~]",
            _ => "[ ]",
        };
        total += 1;
        let _ = writeln!(out, "{mark} {text}");
    }
    let _ = write!(out, "{done}/{total} done");
    Ok(ToolOutput::ok(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_the_checklist() {
        let call = ToolCall {
            id: "c".into(),
            name: "todo".into(),
            arguments: json!({"items": [
                {"text": "build", "status": "done"},
                {"text": "test", "status": "in_progress"},
                {"text": "ship"},
                {"text": ""}
            ]})
            .as_object()
            .unwrap()
            .clone(),
        };
        assert_eq!(
            run(&call).unwrap().content,
            "[x] build\n[~] test\n[ ] ship\n1/3 done"
        );
        let bad = ToolCall {
            arguments: json!({"items": "x"}).as_object().unwrap().clone(),
            ..call
        };
        assert!(run(&bad).is_err());
    }
}
