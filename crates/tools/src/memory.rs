//! The `memory` tool: reads and adds to the project's memory files. Saves are
//! appended (earlier entries are never rewritten), lines beyond a layer's
//! limit move to `memory/archive/YYYY-MM.md`, and each save is committed to
//! git when the project is a repository.

use std::path::Path;

use anyhow::Result;
use gitagent_loader::memory::Layer;
use gitagent_loader::MemoryCfg;
use gitagent_provider::{ToolCall, ToolSpec};
use gitagent_record::Counters;
use serde_json::{json, Value};

use crate::{str_arg, ArgError, ToolOutput, MEMORY};

pub fn spec(cfg: &MemoryCfg) -> ToolSpec {
    let mut properties = json!({
        "action": {"type": "string", "enum": ["load", "save"], "description": "`load` reads the memory; `save` adds to it."},
        "content": {"type": "string", "description": "For save: what to remember, as one or more short lines. It is appended; earlier entries stay, so send only what is new."},
        "message": {"type": "string", "description": "For save: a short note on why, used as the commit message."}
    });
    if cfg.layers.len() > 1 {
        let names: Vec<&str> = cfg.layers.iter().map(|l| l.name.as_str()).collect();
        properties["layer"] = json!({
            "type": "string",
            "enum": names,
            "description": format!("Which memory file (default `{}`).", cfg.working.name)
        });
    }
    ToolSpec {
        name: MEMORY.into(),
        description: "Your memory across sessions, kept in the project's memory/ directory and \
                      shown in the system prompt at the start of each session. Save what a later \
                      session should know: the user's preferences, decisions and their reasons, \
                      where things are. Keep entries short."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": ["action"]
        }),
    }
}

pub(crate) async fn run(
    call: &ToolCall,
    cfg: &MemoryCfg,
    counters: &mut Counters,
) -> Result<ToolOutput> {
    let layer = match call.arguments.get("layer") {
        None | Some(Value::Null) => &cfg.working,
        Some(Value::String(name)) => match cfg.layer(name) {
            Some(l) => l,
            None => {
                let names: Vec<&str> = cfg.layers.iter().map(|l| l.name.as_str()).collect();
                return Ok(ToolOutput::error(format!(
                    "No memory layer named {name:?}. Layers: {}.",
                    names.join(", ")
                )));
            }
        },
        Some(_) => return Err(ArgError("`layer` must be a string".into()).into()),
    };
    match str_arg(call, "action")? {
        "load" => {
            counters.inc("memory_loads");
            let text = std::fs::read_to_string(cfg.dir.join(&layer.path)).unwrap_or_default();
            Ok(ToolOutput::ok(if has_entries(&text) {
                text.trim().to_string()
            } else {
                "No memories yet.".to_string()
            }))
        }
        "save" => {
            let content = str_arg(call, "content")?;
            let message = match call.arguments.get("message").and_then(Value::as_str) {
                Some(m) if !m.trim().is_empty() => m.trim(),
                _ => "Update memory",
            };
            save(cfg, layer, content, message, counters).await
        }
        other => Err(ArgError(format!("`action` must be load or save, not {other:?}")).into()),
    }
}

async fn save(
    cfg: &MemoryCfg,
    layer: &Layer,
    content: &str,
    message: &str,
    counters: &mut Counters,
) -> Result<ToolOutput> {
    let path = cfg.dir.join(&layer.path);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    // A model used to 1.x sends the whole memory back, with its addition or
    // its edits: an addition is appended; a text carrying most of the
    // existing lines (a rewrite) replaces the file, as 1.x's save did.
    let content = content.trim();
    if content.is_empty() {
        return Ok(ToolOutput::error("Nothing to save: `content` is empty."));
    }
    let old = existing.trim();
    let existing_lines: Vec<&str> = old
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let carried = existing_lines
        .iter()
        .filter(|l| content.contains(*l))
        .count();
    let rewrite = existing_lines.len() >= 2 && carried * 10 >= existing_lines.len() * 6;
    let (text, verb) = if let Some(rest) = content.strip_prefix(old).filter(|_| !old.is_empty()) {
        if rest.trim().is_empty() {
            return Ok(ToolOutput::error(
                "Nothing new to save: `content` is already in memory.",
            ));
        }
        (format!("{old}\n{}\n", rest.trim()), "Saved to")
    } else if rewrite {
        (format!("{content}\n"), "Rewrote")
    } else {
        let sep = if old.is_empty() { "" } else { "\n" };
        (format!("{old}{sep}{content}\n"), "Saved to")
    };

    let now = gitagent_record::utc_now_rfc3339();
    let archive_rel = format!("archive/{}.md", &now[..7]);
    let (kept, overflow) = split_overflow(&text, layer.max_lines);
    let mut changed = vec![layer.path.display().to_string()];
    if !overflow.is_empty() {
        let archive = cfg.dir.join(&archive_rel);
        let mut old = std::fs::read_to_string(&archive).unwrap_or_default();
        old.push_str(&format!(
            "\n---\n_Archived from {}: {now}_\n\n{}\n",
            layer.path.display(),
            overflow.join("\n")
        ));
        write(&archive, &old)?;
        counters.add("memory_archived_lines", overflow.len() as u64);
        changed.push(archive_rel.clone());
    }
    write(&path, &kept)?;
    counters.inc("memory_saves");

    let mut note = format!("{verb} memory/{}.", layer.path.display());
    if !overflow.is_empty() {
        note.push_str(&format!(
            " The oldest {} lines went to memory/{archive_rel} to stay within {} lines.",
            overflow.len(),
            layer.max_lines
        ));
    }
    if cfg.commit {
        match commit(&cfg.dir, &changed, message).await {
            Commit::Done => {
                counters.inc("memory_commits");
                note.push_str(&format!(" Committed: {message:?}."));
            }
            Commit::NotARepository => {}
            Commit::Failed(why) => {
                counters.inc("memory_commit_failures");
                note.push_str(&format!(" The git commit failed: {why}"));
            }
        }
    }
    Ok(ToolOutput::ok(note))
}

/// Whether a memory file holds anything besides headings.
fn has_entries(text: &str) -> bool {
    text.lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
}

/// The text cut to `max_lines` lines, and the oldest lines that do not fit.
/// A heading on the first line stays.
fn split_overflow(text: &str, max_lines: usize) -> (String, Vec<&str>) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return (text.to_string(), Vec::new());
    }
    let keep_head = usize::from(lines[0].starts_with('#') && max_lines > 1);
    let excess = lines.len() - max_lines;
    let overflow = lines[keep_head..keep_head + excess].to_vec();
    let mut kept: Vec<&str> = lines[..keep_head].to_vec();
    kept.extend(&lines[keep_head + excess..]);
    (kept.join("\n") + "\n", overflow)
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

enum Commit {
    Done,
    NotARepository,
    Failed(String),
}

/// Commits just the memory files that changed, leaving anything else that
/// is staged alone.
async fn commit(dir: &Path, files: &[String], message: &str) -> Commit {
    let git = |args: Vec<String>| async move {
        tokio::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .await
    };
    match git(vec!["rev-parse".into(), "--is-inside-work-tree".into()]).await {
        Ok(o) if o.status.success() => {}
        _ => return Commit::NotARepository,
    }
    let mut add = vec!["add".to_string(), "--".to_string()];
    add.extend(files.iter().cloned());
    let mut commit = vec![
        "commit".to_string(),
        "--quiet".to_string(),
        "-m".to_string(),
        message.to_string(),
        "--".to_string(),
    ];
    commit.extend(files.iter().cloned());
    for args in [add, commit] {
        match git(args).await {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let out = String::from_utf8_lossy(&o.stdout);
                let why = if err.trim().is_empty() { out } else { err };
                return Commit::Failed(why.trim().to_string());
            }
            Err(e) => return Commit::Failed(e.to_string()),
        }
    }
    Commit::Done
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    /// The tool, run to completion.
    fn run(call: &ToolCall, cfg: &MemoryCfg, counters: &mut Counters) -> Result<ToolOutput> {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(super::run(call, cfg, counters))
    }

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: MEMORY.into(),
            arguments: args.as_object().unwrap().clone(),
        }
    }

    fn cfg(project: &Path, commit: bool) -> MemoryCfg {
        MemoryCfg::for_project(project, commit).unwrap()
    }

    #[test]
    fn saves_append_and_load_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let m = cfg(dir.path(), false);
        let mut c = Counters::new();
        let load = || call(json!({"action": "load"}));
        assert_eq!(
            run(&load(), &m, &mut c).unwrap().content,
            "No memories yet."
        );

        let out = run(
            &call(json!({"action": "save", "content": "- Prefers short answers."})),
            &m,
            &mut c,
        )
        .unwrap();
        assert!(!out.is_error, "{}", out.content);
        run(
            &call(json!({"action": "save", "content": "- Deploys on Fridays are off."})),
            &m,
            &mut c,
        )
        .unwrap();
        assert_eq!(
            run(&load(), &m, &mut c).unwrap().content,
            "- Prefers short answers.\n- Deploys on Fridays are off."
        );
        assert_eq!((c.get("memory_saves"), c.get("memory_loads")), (2, 2));
    }

    #[test]
    fn a_whole_memory_sent_back_only_adds_what_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let m = cfg(dir.path(), false);
        let mut c = Counters::new();
        let file = dir.path().join("memory/MEMORY.md");
        write(&file, "# Memory\n- a\n").unwrap();
        run(
            &call(json!({"action": "save", "content": "# Memory\n- a\n- b"})),
            &m,
            &mut c,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "# Memory\n- a\n- b\n"
        );
        let again = run(
            &call(json!({"action": "save", "content": "# Memory\n- a\n- b\n"})),
            &m,
            &mut c,
        )
        .unwrap();
        assert!(again.is_error);
        assert_eq!(c.get("memory_saves"), 1);
    }

    #[test]
    fn overflow_moves_to_the_archive_and_the_heading_stays() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("memory/memory.yaml"),
            "layers:\n  - {name: working, path: MEMORY.md, max_lines: 3}\n  - {name: decisions, path: decisions.md}\n",
        )
        .unwrap();
        let m = cfg(dir.path(), false);
        let mut c = Counters::new();
        write(
            &dir.path().join("memory/MEMORY.md"),
            "# Memory\n- one\n- two\n",
        )
        .unwrap();
        let out = run(
            &call(json!({"action": "save", "content": "- three\n- four"})),
            &m,
            &mut c,
        )
        .unwrap();
        assert!(out.content.contains("oldest 2 lines"), "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("memory/MEMORY.md")).unwrap(),
            "# Memory\n- three\n- four\n"
        );
        let archives: Vec<_> = std::fs::read_dir(dir.path().join("memory/archive"))
            .unwrap()
            .collect();
        assert_eq!(archives.len(), 1);
        let archived = std::fs::read_to_string(archives[0].as_ref().unwrap().path()).unwrap();
        assert!(archived.contains("_Archived from MEMORY.md: "));
        assert!(archived.ends_with("\n- one\n- two\n"));
        assert_eq!(c.get("memory_archived_lines"), 2);

        // Other layers by name; unknown names are refused.
        run(
            &call(json!({"action": "save", "layer": "decisions", "content": "- Use Postgres."})),
            &m,
            &mut c,
        )
        .unwrap();
        assert!(dir.path().join("memory/decisions.md").is_file());
        let bad = run(
            &call(json!({"action": "load", "layer": "nope"})),
            &m,
            &mut c,
        )
        .unwrap();
        assert!(bad.is_error && bad.content.contains("working, decisions"));
        assert!(spec(&m).input_schema["properties"]["layer"].is_object());
    }

    #[test]
    fn saves_are_committed_alone_in_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("other.txt"), "staged\n").unwrap();
        git(&["add", "other.txt"]);

        let m = cfg(dir.path(), true);
        let mut c = Counters::new();
        let out = run(
            &call(json!({"action": "save", "content": "- x", "message": "Remember x"})),
            &m,
            &mut c,
        )
        .unwrap();
        assert!(
            out.content.contains("Committed: \"Remember x\""),
            "{}",
            out.content
        );
        let log = Command::new("git")
            .current_dir(dir.path())
            .args(["show", "--name-only", "--format=%s", "HEAD"])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(log.starts_with("Remember x\n"));
        assert!(
            log.contains("memory/MEMORY.md") && !log.contains("other.txt"),
            "{log}"
        );
        assert_eq!(c.get("memory_commits"), 1);
    }

    /// A model used to 1.x sends the whole memory back: with an addition it
    /// is appended once; with edits or deletions it replaces the file; a
    /// short unrelated note is appended.
    #[test]
    fn a_whole_memory_sent_back_is_merged_as_1x_did() {
        let dir = tempfile::tempdir().unwrap();
        let m = cfg(dir.path(), false);
        let mut c = Counters::new();
        let path = dir.path().join("memory/MEMORY.md");
        let save = |content: &str, c: &mut Counters| {
            run(&call(json!({"action": "save", "content": content})), &m, c)
                .unwrap()
                .content
        };
        write(&path, "# Memory\n- one\n- two\n- three\n").unwrap();
        let out = save("# Memory\n- one\n- two\n- three\n- four", &mut c);
        assert_eq!(out, "Saved to memory/MEMORY.md.");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Memory\n- one\n- two\n- three\n- four\n"
        );
        let out = save("# Memory\n- one\n- three (not two)\n- four", &mut c);
        assert_eq!(out, "Rewrote memory/MEMORY.md.");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Memory\n- one\n- three (not two)\n- four\n"
        );
        save("- five", &mut c);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Memory\n- one\n- three (not two)\n- four\n- five\n"
        );
        let out = save("# Memory\n- one\n- three (not two)\n- four\n- five", &mut c);
        assert!(out.contains("already in memory"), "{out}");
    }

    #[test]
    fn outside_a_repository_the_save_still_happens() {
        let dir = tempfile::tempdir().unwrap();
        let m = cfg(dir.path(), true);
        let mut c = Counters::new();
        let out = run(
            &call(json!({"action": "save", "content": "- y"})),
            &m,
            &mut c,
        )
        .unwrap();
        assert_eq!(out.content, "Saved to memory/MEMORY.md.");
        assert_eq!(c.get("memory_commit_failures"), 0);
    }
}
