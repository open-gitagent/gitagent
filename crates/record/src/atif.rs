//! Agent Trajectory Interchange Format (ATIF) as validated by Harbor.
//!
//! Field names and shapes follow Harbor's `harbor.models.trajectories` models
//! (schema `ATIF-v1.8`). Harbor rejects unknown keys, so anything of our own
//! goes into an `extra` object, never a new field.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SCHEMA_VERSION: &str = "ATIF-v1.8";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Trajectory {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub agent: Agent,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Agent {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    System,
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub step_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    /// Must be a JSON object.
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Observation {
    pub results: Vec<ObservationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Trajectories of sub-agents this call started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_trajectory_ref: Option<Vec<SubagentTrajectoryRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Harbor requires `trajectory_id` or `trajectory_path`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubagentTrajectoryRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_path: Option<String>,
}

/// Per-step token usage. `prompt_tokens` includes cached tokens;
/// `cached_tokens` is the cached subset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FinalMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

impl Step {
    fn new(source: Source, message: impl Into<String>) -> Self {
        Step {
            step_id: 0,
            timestamp: Some(crate::utc_now_rfc3339()),
            source,
            model_name: None,
            reasoning_effort: None,
            message: message.into(),
            reasoning_content: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            extra: None,
        }
    }

    pub fn system(message: impl Into<String>) -> Self {
        Self::new(Source::System, message)
    }

    pub fn user(message: impl Into<String>) -> Self {
        Self::new(Source::User, message)
    }

    pub fn agent(message: impl Into<String>) -> Self {
        Self::new(Source::Agent, message)
    }
}

/// Holds the trajectory in memory and rewrites it to disk on every `flush`.
pub struct TrajectoryWriter {
    path: PathBuf,
    trajectory: Trajectory,
}

impl TrajectoryWriter {
    pub fn new(path: impl Into<PathBuf>, session_id: Option<String>, agent: Agent) -> Self {
        TrajectoryWriter {
            path: path.into(),
            trajectory: Trajectory {
                schema_version: SCHEMA_VERSION.to_string(),
                session_id,
                agent,
                steps: Vec::new(),
                notes: None,
                final_metrics: None,
                extra: None,
            },
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn trajectory(&self) -> &Trajectory {
        &self.trajectory
    }

    /// Appends a step, assigning the next sequential `step_id`.
    pub fn push(&mut self, mut step: Step) -> u32 {
        step.step_id = self.trajectory.steps.len() as u32 + 1;
        let id = step.step_id;
        self.trajectory.steps.push(step);
        id
    }

    pub fn set_final_metrics(&mut self, metrics: FinalMetrics) {
        self.trajectory.final_metrics = Some(metrics);
    }

    pub fn set_extra(&mut self, key: &str, value: Value) {
        self.trajectory
            .extra
            .get_or_insert_with(Map::new)
            .insert(key.to_string(), value);
    }

    /// Validates and writes the whole trajectory. Harbor needs at least one step,
    /// so an empty trajectory is not written.
    pub fn flush(&self) -> Result<()> {
        if self.trajectory.steps.is_empty() {
            return Ok(());
        }
        validate(&self.trajectory)?;
        let json = serde_json::to_vec_pretty(&self.trajectory)?;
        crate::write_atomic(&self.path, &json)
    }
}

/// Checks the structural rules Harbor enforces when it loads a trajectory.
pub fn validate(t: &Trajectory) -> Result<()> {
    if t.steps.is_empty() {
        bail!("trajectory has no steps");
    }
    for (i, step) in t.steps.iter().enumerate() {
        if step.step_id as usize != i + 1 {
            bail!("step {} has step_id {}", i + 1, step.step_id);
        }
        let agent_only = step.model_name.is_some()
            || step.reasoning_effort.is_some()
            || step.reasoning_content.is_some()
            || step.tool_calls.is_some()
            || step.metrics.is_some();
        if agent_only && step.source != Source::Agent {
            bail!(
                "step {} has agent-only fields on a non-agent step",
                step.step_id
            );
        }
        if let Some(obs) = &step.observation {
            let calls = step.tool_calls.as_deref().unwrap_or_default();
            for r in &obs.results {
                if let Some(id) = &r.source_call_id {
                    if !calls.iter().any(|c| &c.tool_call_id == id) {
                        bail!(
                            "step {} observation refers to unknown tool call {id}",
                            step.step_id
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent() -> Agent {
        Agent {
            name: "gitagent".into(),
            version: "0.1.0".into(),
            model_name: Some("test/model".into()),
            tool_definitions: None,
            extra: None,
        }
    }

    #[test]
    fn writes_sequential_steps_with_matching_observations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trajectory.json");
        let mut w = TrajectoryWriter::new(&path, Some("s1".into()), agent());
        w.push(Step::user("do the task"));
        let mut step = Step::agent("running it");
        let mut args = Map::new();
        args.insert("cmd".into(), json!("echo hi"));
        step.tool_calls = Some(vec![ToolCall {
            tool_call_id: "c1".into(),
            function_name: "exec".into(),
            arguments: args,
        }]);
        step.observation = Some(Observation {
            results: vec![ObservationResult {
                source_call_id: Some("c1".into()),
                content: Some("hi".into()),
                subagent_trajectory_ref: None,
                extra: None,
            }],
        });
        step.metrics = Some(Metrics {
            prompt_tokens: Some(100),
            completion_tokens: Some(10),
            cached_tokens: Some(80),
            ..Default::default()
        });
        assert_eq!(w.push(step), 2);
        w.flush().unwrap();

        let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["schema_version"], "ATIF-v1.8");
        assert_eq!(back["steps"][0]["source"], "user");
        assert_eq!(back["steps"][1]["step_id"], 2);
        assert_eq!(
            back["steps"][1]["tool_calls"][0]["arguments"]["cmd"],
            "echo hi"
        );
        // Unset optional fields are omitted, not written as null.
        assert!(back["steps"][0].get("metrics").is_none());
    }

    #[test]
    fn rejects_observation_for_unknown_call() {
        let mut w = TrajectoryWriter::new("/nonexistent/t.json", None, agent());
        let mut step = Step::agent("");
        step.observation = Some(Observation {
            results: vec![ObservationResult {
                source_call_id: Some("missing".into()),
                content: None,
                subagent_trajectory_ref: None,
                extra: None,
            }],
        });
        w.push(step);
        assert!(w.flush().is_err());
    }

    #[test]
    fn rejects_agent_fields_on_user_step() {
        let mut w = TrajectoryWriter::new("/nonexistent/t.json", None, agent());
        let mut step = Step::user("hi");
        step.metrics = Some(Metrics::default());
        w.push(step);
        assert!(w.flush().is_err());
    }
}
