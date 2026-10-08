//! The learning loop: a task tracker (decompose + check off subtasks) and a
//! skill learner (write new skills and reinforce them with a confidence score).
//! Both persist to the agent directory so learning survives across runs and is
//! committed with the session branch.

use crate::pi::message::ToolResult;
use crate::pi::tool::{AgentTool, ExecutionMode};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

// ── task_tracker ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Task {
    id: u64,
    text: String,
    done: bool,
}

pub struct TaskTracker {
    pub dir: PathBuf,
}

impl TaskTracker {
    fn path(&self) -> PathBuf {
        self.dir.join(".gitagent").join("tasks.json")
    }
    async fn load(&self) -> Vec<Task> {
        match tokio::fs::read_to_string(self.path()).await {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }
    async fn save(&self, tasks: &[Task]) -> anyhow::Result<()> {
        let p = self.path();
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(p, serde_json::to_string_pretty(tasks)?).await?;
        Ok(())
    }
    fn render(tasks: &[Task]) -> String {
        if tasks.is_empty() {
            return "No tasks.".into();
        }
        tasks
            .iter()
            .map(|t| format!("[{}] #{} {}", if t.done { "x" } else { " " }, t.id, t.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl AgentTool for TaskTracker {
    fn name(&self) -> &str { "task_tracker" }
    fn description(&self) -> &str {
        "Track subtasks. action=add (text), complete (id), or list. Returns the current task list."
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {
            "action": { "type": "string", "enum": ["add", "complete", "list"] },
            "text": { "type": "string" },
            "id": { "type": "integer" }
        }, "required": ["action"] })
    }
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    async fn execute(&self, _id: &str, args: Value, _cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("list");
        let mut tasks = self.load().await;
        match action {
            "add" => {
                let text = args.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                if text.is_empty() {
                    return Ok(ToolResult::text("Error: 'text' is required for add"));
                }
                let next = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
                tasks.push(Task { id: next, text, done: false });
                self.save(&tasks).await?;
            }
            "complete" => {
                let id = args.get("id").and_then(Value::as_u64).unwrap_or(0);
                if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                    t.done = true;
                }
                self.save(&tasks).await?;
            }
            "list" => {}
            other => return Ok(ToolResult::text(format!("Unknown action '{other}'"))),
        }
        Ok(ToolResult::text(TaskTracker::render(&tasks)))
    }
}

// ── skill_learner ────────────────────────────────────────────────────────

pub struct SkillLearner {
    pub dir: PathBuf,
}

impl SkillLearner {
    fn skill_path(&self, name: &str) -> PathBuf {
        self.dir.join("skills").join(name).join("SKILL.md")
    }
}

/// Parse a SKILL.md into (frontmatter yaml, body).
fn split_frontmatter(content: &str) -> (serde_yaml::Value, String) {
    let s = content.trim_start();
    if let Some(after) = s.strip_prefix("---") {
        if let Some(end) = after.find("\n---") {
            let fm = serde_yaml::from_str(after[..end].trim()).unwrap_or(serde_yaml::Value::Null);
            let body = after[end + 4..].trim_start().to_string();
            return (fm, body);
        }
    }
    (serde_yaml::Value::Null, content.to_string())
}

fn confidence_of(fm: &serde_yaml::Value) -> f64 {
    fm.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5)
}

#[async_trait]
impl AgentTool for SkillLearner {
    fn name(&self) -> &str { "skill_learner" }
    fn description(&self) -> &str {
        "Persist or reinforce a skill. action=save (name, description, body) writes skills/<name>/SKILL.md; \
         reinforce (name, delta) adjusts its confidence in [0,1]."
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {
            "action": { "type": "string", "enum": ["save", "reinforce"] },
            "name": { "type": "string" },
            "description": { "type": "string" },
            "body": { "type": "string" },
            "delta": { "type": "number" }
        }, "required": ["action", "name"] })
    }
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    async fn execute(&self, _id: &str, args: Value, _cancel: &CancellationToken) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or_default();
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        if name.is_empty() {
            return Ok(ToolResult::text("Error: 'name' is required"));
        }
        if !crate::sdk::loader::is_safe_name(&name) {
            return Ok(ToolResult::text(
                "Error: 'name' must be a simple skill name (letters, digits, '.', '_', '-')",
            ));
        }
        let path = self.skill_path(&name);
        match action {
            "save" => {
                let desc = args.get("description").and_then(Value::as_str).unwrap_or_default().replace(['\n', '\r'], " ");
                let body = args.get("body").and_then(Value::as_str).unwrap_or_default();
                let confidence = match tokio::fs::read_to_string(&path).await {
                    Ok(existing) => confidence_of(&split_frontmatter(&existing).0),
                    Err(_) => 0.5,
                };
                let out = format!(
                    "---\nname: {name}\ndescription: {desc}\nconfidence: {confidence}\n---\n{body}\n"
                );
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, out).await?;
                Ok(ToolResult::text(format!("Saved skill '{name}' (confidence {confidence:.2}).")))
            }
            "reinforce" => {
                let delta = args.get("delta").and_then(Value::as_f64).unwrap_or(0.1);
                let Ok(existing) = tokio::fs::read_to_string(&path).await else {
                    return Ok(ToolResult::text(format!("Error: skill '{name}' not found")));
                };
                let (fm, body) = split_frontmatter(&existing);
                let desc = fm.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let confidence = (confidence_of(&fm) + delta).clamp(0.0, 1.0);
                let out = format!(
                    "---\nname: {name}\ndescription: {desc}\nconfidence: {confidence}\n---\n{body}\n"
                );
                tokio::fs::write(&path, out).await?;
                Ok(ToolResult::text(format!("Skill '{name}' confidence now {confidence:.2}.")))
            }
            other => Ok(ToolResult::text(format!("Unknown action '{other}'"))),
        }
    }
}
