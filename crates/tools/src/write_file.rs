use std::path::Path;

use anyhow::Result;
use gitagent_provider::ToolCall;

use crate::{str_arg, ToolOutput};

pub(crate) fn run(call: &ToolCall, cwd: &Path) -> Result<ToolOutput> {
    let path = cwd.join(str_arg(call, "path")?);
    let content = str_arg(call, "content")?;
    let existed = path.exists();
    if path.is_dir() {
        return Ok(ToolOutput::error(format!(
            "{} is a directory.",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Ok(ToolOutput::error(format!(
                "Cannot create {}: {e}",
                parent.display()
            )));
        }
    }
    if let Err(e) = std::fs::write(&path, content) {
        return Ok(ToolOutput::error(format!(
            "Cannot write {}: {e}",
            path.display()
        )));
    }
    Ok(ToolOutput::ok(format!(
        "{} {} ({} bytes, {} lines).",
        if existed { "Overwrote" } else { "Created" },
        path.display(),
        content.len(),
        content.lines().count()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "write_file".into(),
            arguments: args.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn creates_and_overwrites_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &call(json!({"path": "a/b/c.txt", "content": "x\ny\n"})),
            dir.path(),
        )
        .unwrap();
        assert!(out.content.starts_with("Created "), "{}", out.content);
        assert!(out.content.ends_with("(4 bytes, 2 lines)."));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "x\ny\n"
        );
        let out = run(
            &call(json!({"path": "a/b/c.txt", "content": ""})),
            dir.path(),
        )
        .unwrap();
        assert!(out.content.starts_with("Overwrote "));
        let out = run(&call(json!({"path": "a", "content": "z"})), dir.path()).unwrap();
        assert!(out.is_error);
    }
}
