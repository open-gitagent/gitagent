//! Reads finished trials: gitagent's own trajectory and the trial's reward.
//! Nothing else in a trial directory is opened: it never reads verifier output or tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gitagent_record::atif::{Source, Trajectory};
use serde_json::Value;

/// Longest rendered trace sent to the model; the middle is cut beyond this.
const MAX_TRACE_CHARS: usize = 60_000;
/// Longest single message, argument or observation in the rendered trace.
const MAX_ITEM_CHARS: usize = 2_000;

#[derive(Debug, Clone)]
pub struct Trial {
    /// `<job>/<trial>`, used to skip trials already learned from.
    pub id: String,
    /// The task name, used only for the split check and the leakage audit;
    /// it is never shown to the model.
    pub task: String,
    /// The trial's reward, when the run got as far as being checked.
    pub reward: Option<f64>,
    /// The task instruction (the first user message).
    pub instruction: String,
    /// The trajectory as plain text for the model.
    pub trace: String,
}

impl Trial {
    pub fn passed(&self) -> bool {
        self.reward.is_some_and(|r| r >= 1.0)
    }
}

/// Every trial in a Harbor job directory (sub-directories with a
/// `result.json` and an `agent/trajectory.json`), sorted by name.
pub fn read_job(job_dir: &Path) -> Result<Vec<Trial>> {
    let job = job_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(job_dir)
        .with_context(|| format!("reading {}", job_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("result.json").is_file() && p.join("agent/trajectory.json").is_file())
        .collect();
    dirs.sort();
    let mut trials = Vec::new();
    for dir in dirs {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        trials.push(read_trial(&dir, &format!("{job}/{name}"))?);
    }
    Ok(trials)
}

pub fn read_trial(dir: &Path, id: &str) -> Result<Trial> {
    let result: Value = read_json(&dir.join("result.json"))?;
    let task = result["task_name"]
        .as_str()
        .or_else(|| result["config"]["task"]["name"].as_str())
        .unwrap_or_default();
    let task = task.rsplit('/').next().unwrap_or(task).to_string();
    let reward = result["verifier_result"]["rewards"]["reward"].as_f64();
    let trajectory: Trajectory = read_json(&dir.join("agent/trajectory.json"))?;
    let (instruction, trace) = render(&trajectory);
    Ok(Trial {
        id: id.to_string(),
        task,
        reward,
        instruction,
        trace,
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The instruction and the trajectory as text: each agent step's message,
/// tool calls and observations, long items cut in the middle. The system
/// prompt is left out (the learner sees the lessons separately).
pub fn render(t: &Trajectory) -> (String, String) {
    let mut instruction = String::new();
    let mut out = String::new();
    for step in &t.steps {
        match step.source {
            Source::System => {}
            Source::User => {
                if instruction.is_empty() {
                    instruction = step.message.clone();
                }
                out.push_str(&format!(
                    "## user\n{}\n\n",
                    cut(&step.message, MAX_ITEM_CHARS)
                ));
            }
            Source::Agent => {
                out.push_str(&format!("## agent (step {})\n", step.step_id));
                if !step.message.trim().is_empty() {
                    out.push_str(&cut(&step.message, MAX_ITEM_CHARS));
                    out.push('\n');
                }
                for call in step.tool_calls.iter().flatten() {
                    let args = serde_json::to_string(&call.arguments).unwrap_or_default();
                    out.push_str(&format!(
                        "-> {}({})\n",
                        call.function_name,
                        cut(&args, MAX_ITEM_CHARS)
                    ));
                }
                for r in step.observation.iter().flat_map(|o| &o.results) {
                    if let Some(c) = &r.content {
                        out.push_str(&format!("<- {}\n", cut(c, MAX_ITEM_CHARS)));
                    }
                }
                out.push('\n');
            }
        }
    }
    (instruction, cut(&out, MAX_TRACE_CHARS))
}

/// Keeps the head and tail of long text.
fn cut(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head: String = chars[..max / 2].iter().collect();
    let tail: String = chars[chars.len() - max / 2..].iter().collect();
    format!(
        "{head}\n[... {} characters cut ...]\n{tail}",
        chars.len() - max
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::write_trial;

    #[test]
    fn reads_the_trajectory_and_reward_only() {
        let job = tempfile::tempdir().unwrap();
        write_trial(job.path(), "a__1", "serve-thing", Some(1.0));
        write_trial(job.path(), "b__2", "other-thing", None);
        std::fs::create_dir_all(job.path().join("not-a-trial")).unwrap();
        let trials = read_job(job.path()).unwrap();
        assert_eq!(trials.len(), 2);
        let t = &trials[0];
        assert_eq!(t.task, "serve-thing");
        assert!(t.passed());
        assert!(!trials[1].passed());
        assert!(t.instruction.contains("port 5328"));
        assert!(!t.trace.contains("SYSTEM PROMPT"));
        assert!(t.trace.contains("-> exec("));
        assert!(t.trace.contains("characters cut"));
        assert!(!t.trace.contains("SECRET"));
        assert!(t.id.ends_with("/a__1"));
    }
}
