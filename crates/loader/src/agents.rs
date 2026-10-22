//! Sub-agent definitions: `agents/<name>.md` in a config directory (including
//! `.claude/agents/`, in the `.claude/` format), plus a built-in
//! `general` agent. Frontmatter: `name`, `description`, optional `tools` and
//! `disallowedTools` (a comma-separated string or a YAML list, in `.claude/`
//! or gitagent tool names), `maxTurns` (or `max_turns`), `model` and `effort`;
//! the body is the sub-agent's system prompt.

use std::path::PathBuf;

use crate::skills::split_frontmatter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// Tools the sub-agent may use (gitagent names); `None` means the parent's tools.
    pub tools: Option<Vec<String>>,
    /// Tools removed from whatever `tools` allows (gitagent names).
    pub disallowed_tools: Vec<String>,
    pub max_turns: u32,
    /// The agent file's `model` field (an alias such as `sonnet`, a model
    /// id, or `inherit`). Stored for the sub-agent features; not applied yet.
    pub model: Option<String>,
    /// The agent file's `effort` field: `low`, `medium`, `high`, `xhigh` or `max`.
    pub effort: Option<String>,
    pub instructions: String,
    /// A whole agent of its own (`agents/<name>/`), run with its own prompt,
    /// skills, tools, hooks and memory; `None` for a definition that runs
    /// on the parent's prompt with `instructions`.
    pub root: Option<PathBuf>,
    /// When to delegate to it (`delegation.triggers`), as written.
    pub triggers: Vec<String>,
    /// Its `delegation.mode`.
    pub delegation: Option<String>,
}

pub const GENERAL: &str = "general";
const DEFAULT_MAX_TURNS: u32 = 30;

pub fn general() -> AgentDef {
    AgentDef {
        name: GENERAL.into(),
        description: "A helper for a self-contained piece of work (investigating, building or \
            checking something) whose details you do not need to keep in your own context."
            .into(),
        tools: None,
        disallowed_tools: Vec::new(),
        max_turns: DEFAULT_MAX_TURNS,
        model: None,
        effort: None,
        instructions: String::new(),
        root: None,
        triggers: Vec::new(),
        delegation: None,
    }
}

/// A frontmatter field by key: `key: value`, `key: [a, b]`, or a YAML list on
/// the following lines. Lists come back joined with commas.
fn front_field(front: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = front.lines().collect();
    let i = lines.iter().position(|l| {
        l.strip_prefix(key)
            .is_some_and(|r| r.trim_start().starts_with(':') && !l.starts_with(' '))
    })?;
    let value = lines[i]
        .split_once(':')
        .map(|(_, v)| v.trim())
        .unwrap_or("");
    if !value.is_empty() {
        let v = value.trim_matches('"').trim_matches('\'');
        let v = v
            .strip_prefix('[')
            .and_then(|x| x.strip_suffix(']'))
            .unwrap_or(v);
        return Some(v.to_string());
    }
    let items: Vec<String> = lines[i + 1..]
        .iter()
        .take_while(|l| l.trim_start().starts_with('-'))
        .map(|l| {
            l.trim_start()
                .trim_start_matches('-')
                .trim()
                .trim_matches('"')
                .to_string()
        })
        .collect();
    Some(items.join(", "))
}

fn tool_list(value: Option<String>) -> Option<Vec<String>> {
    value.map(|t| {
        crate::sdk::normalize_tool_list(
            &t.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>(),
        )
    })
}

/// The built-in agent plus definitions from the config directories (the
/// first directory wins for a name), sorted by name.
pub fn discover(config_dirs: &[PathBuf]) -> Vec<AgentDef> {
    let mut agents = vec![general()];
    for dir in config_dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("agents")) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (front, body) = split_frontmatter(&text);
            let field = |key: &str| front_field(front, key);
            let name = field("name")
                .filter(|n| !n.is_empty())
                .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_default();
            if name.is_empty() || agents.iter().any(|a| a.name == name && a.name != GENERAL) {
                continue;
            }
            let def = AgentDef {
                description: field("description").unwrap_or_default(),
                tools: tool_list(field("tools")),
                disallowed_tools: tool_list(
                    field("disallowedTools").or_else(|| field("disallowed_tools")),
                )
                .unwrap_or_default(),
                max_turns: field("maxTurns")
                    .or_else(|| field("max_turns"))
                    .and_then(|m| m.parse().ok())
                    .unwrap_or(DEFAULT_MAX_TURNS),
                model: field("model").filter(|m| !m.is_empty() && m != "inherit"),
                effort: field("effort").filter(|e| !e.is_empty()),
                instructions: body.trim().to_string(),
                name,
                root: None,
                triggers: Vec::new(),
                delegation: None,
            };
            // A file can redefine the built-in agent.
            agents.retain(|a| a.name != def.name);
            agents.push(def);
        }
    }
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    agents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_and_file_agents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(
            dir.path().join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews a diff.\ntools: exec, read_file\nmax_turns: 8\n---\nOnly read; never edit.\n",
        )
        .unwrap();
        let agents = discover(&[dir.path().to_path_buf()]);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name, "general");
        let r = &agents[1];
        assert_eq!(r.description, "Reviews a diff.");
        assert_eq!(
            r.tools.as_deref(),
            Some(&["exec".to_string(), "read_file".to_string()][..])
        );
        assert_eq!(r.max_turns, 8);
        assert_eq!(r.instructions, "Only read; never edit.");
    }

    #[test]
    fn claude_agent_sdk_files_are_understood() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(
            dir.path().join("agents/code-reviewer.md"),
            "---\nname: code-reviewer\ndescription: Reviews code for quality.\ntools:\n  - Read\n  - Grep\n  - Bash\ndisallowedTools: Edit, Write\nmodel: sonnet\nmaxTurns: 12\neffort: low\ncolor: cyan\n---\nYou are a code reviewer.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("agents/writer.md"),
            "---\nname: writer\ndescription: Writes docs.\ntools: [Write, Read]\nmodel: inherit\n---\nWrite.\n",
        )
        .unwrap();
        let agents = discover(&[dir.path().to_path_buf()]);
        let r = agents.iter().find(|a| a.name == "code-reviewer").unwrap();
        // Grep has no gitagent equivalent and is dropped; Bash is exec.
        assert_eq!(
            r.tools.as_deref(),
            Some(
                &[
                    "read_file".to_string(),
                    "image_read".to_string(),
                    "exec".to_string()
                ][..]
            )
        );
        assert_eq!(
            r.disallowed_tools,
            vec!["str_replace", "apply_patch", "write_file"]
        );
        assert_eq!(
            (r.max_turns, r.model.as_deref(), r.effort.as_deref()),
            (12, Some("sonnet"), Some("low"))
        );
        assert_eq!(r.instructions, "You are a code reviewer.");
        let w = agents.iter().find(|a| a.name == "writer").unwrap();
        assert_eq!(
            w.tools.as_deref(),
            Some(
                &[
                    "write_file".to_string(),
                    "read_file".to_string(),
                    "image_read".to_string()
                ][..]
            )
        );
        assert_eq!(w.model, None, "inherit means the parent's model");
    }
}
