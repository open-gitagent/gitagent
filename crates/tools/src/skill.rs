use anyhow::Result;
use gitagent_loader::{split_frontmatter, Skill};
use gitagent_provider::ToolCall;

use crate::{str_arg, ToolOutput};

pub(crate) fn run(call: &ToolCall, skills: &[Skill]) -> Result<ToolOutput> {
    let name = str_arg(call, "name")?;
    let Some(skill) = skills.iter().find(|s| s.name == name) else {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        return Ok(ToolOutput::error(format!(
            "No skill named {name:?}. Skills: {}.",
            names.join(", ")
        )));
    };
    match std::fs::read_to_string(&skill.path) {
        Ok(text) => {
            let dir = skill
                .path
                .parent()
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            Ok(ToolOutput::ok(format!(
                "Skill {name} (files next to it are in {dir}):\n\n{}",
                split_frontmatter(&text).1.trim()
            )))
        }
        Err(e) => Ok(ToolOutput::error(format!("Cannot read skill {name}: {e}"))),
    }
}
