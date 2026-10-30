use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use gitagent_provider::ToolCall;

use crate::{int_arg, str_arg, ToolOutput};

const DEFAULT_LIMIT: u64 = 2000;
const MAX_LINE_CHARS: usize = 2000;
const MAX_TOTAL_CHARS: usize = 100_000;

pub(crate) fn run(call: &ToolCall, cwd: &Path) -> Result<ToolOutput> {
    let path = cwd.join(str_arg(call, "path")?);
    let offset = int_arg(call, "offset")?.unwrap_or(1).max(1) as usize;
    let limit = int_arg(call, "limit")?.unwrap_or(DEFAULT_LIMIT).max(1) as usize;

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "Cannot read {}: {e}",
                path.display()
            )))
        }
    };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok(ToolOutput::error(format!(
            "{} looks like a binary file ({} bytes). Inspect it with `exec` (e.g. `xxd | head`).",
            path.display(),
            bytes.len()
        )));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok(ToolOutput::ok(format!("{} is empty.", path.display())));
    }
    if offset > lines.len() {
        return Ok(ToolOutput::error(format!(
            "offset {offset} is past the end of {} ({} lines).",
            path.display(),
            lines.len()
        )));
    }

    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        if out.len() > MAX_TOTAL_CHARS {
            break;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.chars().count() > MAX_LINE_CHARS {
            let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
            let _ = writeln!(out, "{:>6}\t{cut} [line truncated]", i + 1);
        } else {
            let _ = writeln!(out, "{:>6}\t{line}", i + 1);
        }
        last = i + 1;
    }
    if last < lines.len() {
        let _ = write!(
            out,
            "[showing lines {offset}-{last} of {}; use offset={} to continue]",
            lines.len(),
            last + 1
        );
    }
    Ok(ToolOutput::ok(out.trim_end().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "read_file".into(),
            arguments: args.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn reads_numbered_lines_with_paging() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\r\nthree\n").unwrap();
        let out = run(&call(json!({"path": "a.txt"})), dir.path()).unwrap();
        assert_eq!(out.content, "     1\tone\n     2\ttwo\n     3\tthree");
        let out = run(
            &call(json!({"path": "a.txt", "offset": 2, "limit": 1})),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            out.content,
            "     2\ttwo\n[showing lines 2-2 of 3; use offset=3 to continue]"
        );
    }

    #[test]
    fn reports_missing_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&call(json!({"path": "nope"})), dir.path()).unwrap();
        assert!(out.is_error);
        std::fs::write(dir.path().join("b"), [0u8, 1, 2]).unwrap();
        let out = run(&call(json!({"path": "b"})), dir.path()).unwrap();
        assert!(out.is_error && out.content.contains("binary"));
    }
}
