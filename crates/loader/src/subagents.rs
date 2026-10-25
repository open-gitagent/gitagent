//! Sub-agents of an agent directory, in `agents/`:
//!
//! - `agents/<name>/`: a whole agent of its own (agent.yaml, SOUL.md, its
//!   skills, tools, hooks and memory), run with its own prompt and context;
//! - `agents/<name>.md`: frontmatter (`name`, `description`, `model`,
//!   `delegation`, and gitagent's `tools`, `disallowedTools`, `maxTurns`,
//!   `effort`) and a body of instructions, run on the parent's prompt.
//!
//! When agent.yaml has an `agents:` map, only its keys count, with their
//! `description` and `delegation` (`mode`, `triggers`); without one, every
//! entry of `agents/` is offered. The parent's `delegation.mode` says how
//! they are used: `auto` (whenever their purpose fits), `explicit` (only
//! when asked by name) or `router` (every request goes to the router first).

use std::path::Path;

use serde_json::Value;

use crate::agents::AgentDef;
use crate::AgentDir;

const DEFAULT_MAX_TURNS: u32 = 30;

/// The sub-agents of `agent`, sorted by name, and what was wrong.
pub fn discover(agent: &AgentDir) -> (Vec<AgentDef>, Vec<String>) {
    let mut warnings = Vec::new();
    let Some(base) = agent.file_root("agents") else {
        for name in agent.manifest.agents.keys() {
            warnings.push(format!(
                "agent.yaml names the sub-agent {name:?}, but there is no agents/{name}/ or agents/{name}.md"
            ));
        }
        return (Vec::new(), warnings);
    };
    let dir = base.join("agents");
    let declared = &agent.manifest.agents;
    let mut defs = Vec::new();
    if declared.is_empty() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let is_full = path.is_dir() && path.join("agent.yaml").is_file();
            let is_light = path.is_file() && path.extension().is_some_and(|e| e == "md");
            if !(is_full || is_light) || name.is_empty() {
                continue;
            }
            match load(&path, &name, None) {
                Ok(def) => defs.push(def),
                Err(why) => warnings.push(why),
            }
        }
    } else {
        for (name, settings) in declared {
            let full = dir.join(name);
            let light = dir.join(format!("{name}.md"));
            let path = if full.join("agent.yaml").is_file() {
                full
            } else if light.is_file() {
                light
            } else {
                warnings.push(format!(
                    "agent.yaml names the sub-agent {name:?}, but there is no agents/{name}/ or agents/{name}.md"
                ));
                continue;
            };
            match load(&path, name, Some(settings)) {
                Ok(def) => defs.push(def),
                Err(why) => warnings.push(why),
            }
        }
    }
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    (defs, warnings)
}

/// One sub-agent, from its directory or its file; `settings` is its entry
/// in the parent's `agents:` map.
fn load(path: &Path, name: &str, settings: Option<&Value>) -> Result<AgentDef, String> {
    let s = |key: &str| settings.and_then(|v| v[key].as_str()).map(str::to_string);
    let triggers: Vec<String> = settings
        .and_then(|v| v["delegation"]["triggers"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let mode = settings.and_then(|v| v["delegation"]["mode"].as_str().map(str::to_string));
    if path.is_dir() {
        let sub = AgentDir::load(path)
            .map_err(|e| format!("agents/{name}: {e:#}"))?
            .ok_or_else(|| format!("agents/{name}/ has no agent.yaml"))?;
        let m = &sub.manifest;
        return Ok(AgentDef {
            name: name.to_string(),
            description: s("description")
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| m.description.clone()),
            tools: None,
            disallowed_tools: Vec::new(),
            max_turns: m
                .runtime
                .as_ref()
                .and_then(|r| r.max_turns)
                .unwrap_or(DEFAULT_MAX_TURNS),
            model: m.model.as_ref().and_then(|m| m.preferred.clone()),
            effort: None,
            instructions: String::new(),
            root: Some(sub.root.clone()),
            triggers,
            delegation: mode,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("agents/{name}.md: {e}"))?;
    let (front, body) = crate::split_frontmatter(&text);
    let fm: Value = if front.trim().is_empty() {
        Value::Null
    } else {
        serde_saphyr::from_str(front)
            .map_err(|e| format!("agents/{name}.md: the frontmatter is not valid YAML: {e}"))?
    };
    let text_field = |key: &str| fm[key].as_str().map(|v| v.trim().to_string());
    let list = |key: &str| -> Option<Vec<String>> {
        match &fm[key] {
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
            Value::String(s) => Some(s.split(',').map(|x| x.trim().to_string()).collect()),
            _ => None,
        }
    };
    let model = match &fm["model"] {
        Value::String(m) if m != "inherit" => Some(m.clone()),
        Value::Object(o) => o
            .get("preferred")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    };
    let mut triggers = triggers;
    if triggers.is_empty() {
        triggers = list("triggers")
            .or_else(|| {
                fm["delegation"]["triggers"].as_array().map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
            })
            .unwrap_or_default();
    }
    Ok(AgentDef {
        name: name.to_string(),
        description: s("description")
            .or_else(|| text_field("description"))
            .unwrap_or_default(),
        tools: list("tools").map(|t| crate::sdk::normalize_tool_list(&t)),
        disallowed_tools: list("disallowedTools")
            .or_else(|| list("disallowed_tools"))
            .map(|t| crate::sdk::normalize_tool_list(&t))
            .unwrap_or_default(),
        max_turns: fm["maxTurns"]
            .as_u64()
            .or(fm["max_turns"].as_u64())
            .map_or(DEFAULT_MAX_TURNS, |n| n as u32),
        model,
        effort: text_field("effort"),
        instructions: body.trim().to_string(),
        root: None,
        triggers,
        delegation: mode.or_else(|| fm["delegation"]["mode"].as_str().map(str::to_string)),
    })
}

/// The `# Sub-agents` section of the system prompt for the agent
/// directory's sub-agents, with the parent's `delegation` setting.
pub fn prompt_section(defs: &[AgentDef], delegation: Option<&Value>) -> String {
    let own: Vec<&AgentDef> = defs
        .iter()
        .filter(|d| d.root.is_some() || !d.triggers.is_empty() || d.delegation.is_some())
        .collect();
    if own.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "# Sub-agents\n\nAgents you can start with the `subagent` tool, each with its own instructions and context:\n",
    );
    for d in &own {
        let mut line = format!("- {}: {}", d.name, d.description.trim());
        if !d.triggers.is_empty() {
            line.push_str(&format!(" Use it when: {}.", d.triggers.join(", ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    let mode = delegation
        .and_then(|d| d["mode"].as_str())
        .unwrap_or("auto");
    let router = delegation.and_then(|d| d["router"].as_str());
    out.push('\n');
    out.push_str(match (mode, router) {
        ("explicit", _) => "Delegate only when the person asks for a sub-agent by name.\n",
        ("router", Some(r)) => {
            return format!(
                "{out}Send every request to the `{r}` sub-agent first and follow its answer about who does the work.\n"
            )
        }
        _ => "Delegate when a sub-agent's purpose or triggers fit the work.\n",
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(p: std::path::PathBuf, t: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }

    #[test]
    fn both_forms_are_found_and_the_manifest_picks_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root.join("agent.yaml"),
            "name: lead\nversion: 1.0.0\ndescription: Lead.\nagents:\n  fact-checker: {description: Checks facts., delegation: {mode: auto, triggers: [factual_claim_detected]}}\n  writer: {}\n  ghost: {}\n",
        );
        write(root.join("SOUL.md"), "Lead soul.\n");
        write(
            root.join("agents/fact-checker/agent.yaml"),
            "name: fact-checker\nversion: 1.0.0\ndescription: From its own manifest.\nmodel: {preferred: claude-haiku-5-5}\nruntime: {max_turns: 12}\n",
        );
        write(root.join("agents/fact-checker/SOUL.md"), "I check.\n");
        write(
            root.join("agents/writer.md"),
            "---\nname: writer\ndescription: Writes.\nmodel: {preferred: claude-sonnet-5-5}\ntools: [cli, read]\nmaxTurns: 8\ndelegation: {mode: explicit, triggers: [draft_requested]}\n---\nWrite plainly.\n",
        );
        write(
            root.join("agents/extra.md"),
            "---\ndescription: Not declared.\n---\nx\n",
        );
        let agent = AgentDir::load(root).unwrap().unwrap();
        let (defs, warnings) = discover(&agent);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["fact-checker", "writer"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("\"ghost\""));
        let fc = &defs[0];
        assert_eq!(fc.description, "Checks facts.");
        assert_eq!(fc.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(fc.max_turns, 12);
        assert!(fc.root.as_ref().unwrap().ends_with("agents/fact-checker"));
        assert_eq!(fc.triggers, vec!["factual_claim_detected"]);
        assert_eq!(fc.delegation.as_deref(), Some("auto"));
        let w = &defs[1];
        assert_eq!(w.description, "Writes.");
        assert_eq!(w.instructions, "Write plainly.");
        assert_eq!(w.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            w.tools.as_deref(),
            Some(
                &[
                    "exec".to_string(),
                    "write_stdin".into(),
                    "read_file".into(),
                    "image_read".into()
                ][..]
            )
        );
        assert_eq!(w.max_turns, 8);
        assert_eq!(w.triggers, vec!["draft_requested"]);
        assert!(w.root.is_none());

        let section = prompt_section(&defs, Some(&json!({"mode": "router", "router": "writer"})));
        assert!(section
            .contains("- fact-checker: Checks facts. Use it when: factual_claim_detected.\n"));
        assert!(section.ends_with("Send every request to the `writer` sub-agent first and follow its answer about who does the work.\n"));
        assert!(prompt_section(&defs, None)
            .ends_with("Delegate when a sub-agent's purpose or triggers fit the work.\n"));

        // Without an `agents:` map, everything in agents/ is offered.
        write(
            root.join("agent.yaml"),
            "name: lead\nversion: 1.0.0\ndescription: Lead.\n",
        );
        let agent = AgentDir::load(root).unwrap().unwrap();
        let (defs, warnings) = discover(&agent);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["extra", "fact-checker", "writer"]);
        assert!(warnings.is_empty());
        assert_eq!(defs[1].description, "From its own manifest.");
    }
}
