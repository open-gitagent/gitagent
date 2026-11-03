//! Continuous learning: distils general lessons from gitagent's own finished
//! trajectories and their pass/fail outcomes into `prompts/lessons.md`.
//!
//! The learner reads only each trial's `agent/trajectory.json` and the reward
//! in its `result.json`; it never reads tests, verifier output or solutions. With a
//! split file it learns from dev tasks only. Every proposed lesson passes a
//! leakage audit before it is stored, and the rendered file is reviewed and
//! committed like any other prompt change.

mod audit;
mod distill;
mod store;
mod trace;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gitagent_loader::AgentSpec;
use gitagent_provider::Provider;
use serde::Serialize;

pub use audit::check as audit_lesson;
pub use store::{Applied, Lesson, Op, Store};
pub use trace::{read_job, read_trial, Trial};

/// Most operations taken from one reply.
const MAX_OPS_PER_CALL: usize = 3;

pub struct LearnOptions {
    /// Harbor job directories to learn from.
    pub jobs: Vec<PathBuf>,
    /// The lesson store (lessons with importance counts).
    pub store_path: PathBuf,
    /// The rendered lessons file the agent loads.
    pub lessons_path: PathBuf,
    /// Dev task names; trials of any other task are skipped. `None` learns
    /// from every trial (only when no split exists yet).
    pub dev_tasks: Option<BTreeSet<String>>,
    /// Size limit of the rendered lessons.
    pub max_chars: usize,
    /// Lessons are rendered only once attempts at this many different tasks
    /// added or upvoted them.
    pub min_tasks: usize,
    /// Propose and audit, but write nothing.
    pub dry_run: bool,
}

/// What a learning pass did (also its counters).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct LearnReport {
    pub trials_read: usize,
    pub trials_not_dev: usize,
    pub trials_already_learned: usize,
    pub trials_learned: usize,
    pub model_calls: usize,
    pub model_errors: usize,
    pub ops_proposed: usize,
    pub ops_rejected: usize,
    pub lessons_added: usize,
    pub lessons_edited: usize,
    pub lessons_upvoted: usize,
    pub lessons_downvoted: usize,
    pub lessons_deleted: usize,
    pub duplicates: usize,
    pub unknown_ids: usize,
    pub lessons_total: usize,
    /// Lessons with enough evidence to be in the lessons file.
    pub lessons_shown: usize,
    /// The rendered lessons (written to the lessons file unless dry-run).
    #[serde(skip)]
    pub rendered: String,
}

/// Reads the dev task names from a split file (`{"dev": [...], ...}`).
pub fn read_split(path: &Path) -> Result<BTreeSet<String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let dev = v["dev"]
        .as_array()
        .with_context(|| format!("{} has no `dev` list", path.display()))?;
    Ok(dev
        .iter()
        .filter_map(|t| t.as_str())
        .map(|t| t.rsplit('/').next().unwrap_or(t).to_string())
        .collect())
}

pub async fn learn(
    spec: &AgentSpec,
    provider: &dyn Provider,
    opts: &LearnOptions,
) -> Result<LearnReport> {
    let mut store = Store::load(&opts.store_path)?;
    let mut report = LearnReport::default();

    let mut by_task: BTreeMap<String, Vec<Trial>> = BTreeMap::new();
    for job in &opts.jobs {
        for trial in read_job(job)? {
            report.trials_read += 1;
            if opts
                .dev_tasks
                .as_ref()
                .is_some_and(|dev| !dev.contains(&trial.task))
            {
                report.trials_not_dev += 1;
                continue;
            }
            if store.learned.contains(&trial.id) {
                report.trials_already_learned += 1;
                continue;
            }
            by_task.entry(trial.task.clone()).or_default().push(trial);
        }
    }

    for trials in by_task.values() {
        for unit in units(trials) {
            learn_unit(spec, provider, &mut store, &unit, &mut report).await;
        }
    }
    report.lessons_total = store.lessons.len();
    report.rendered = store.render(opts.max_chars, opts.min_tasks);
    report.lessons_shown = report.rendered.lines().count();

    if !opts.dry_run {
        store.save(&opts.store_path)?;
        write_lessons(&opts.lessons_path, &report.rendered)?;
    }
    Ok(report)
}

/// Asks the model about one group of attempts, audits its operations and
/// applies the rest. On a model error the attempts are left unlearned.
async fn learn_unit(
    spec: &AgentSpec,
    provider: &dyn Provider,
    store: &mut Store,
    unit: &[&Trial],
    report: &mut LearnReport,
) {
    report.model_calls += 1;
    let ops = match distill::propose(spec, provider, store, unit).await {
        Ok(ops) => ops,
        Err(e) => {
            report.model_errors += 1;
            eprintln!("gitagent learn: {}: {e:#}", ids(unit));
            return;
        }
    };
    report.ops_proposed += ops.len();
    let mut kept = Vec::new();
    for op in ops.into_iter().take(MAX_OPS_PER_CALL) {
        let text = match &op {
            Op::Add { text } | Op::Edit { text, .. } => Some(text.as_str()),
            _ => None,
        };
        let rejected = text.and_then(|text| {
            unit.iter()
                .find_map(|t| audit::check(text, &t.task, &t.instruction))
                .map(|why| (text, why))
        });
        match rejected {
            Some((text, why)) => {
                report.ops_rejected += 1;
                eprintln!("gitagent learn: rejected {text:?}: {why}");
            }
            None => kept.push(op),
        }
    }
    let a = store.apply(&kept, &unit[0].task, &ids(unit));
    report.lessons_added += a.added;
    report.lessons_edited += a.edited;
    report.lessons_upvoted += a.upvoted;
    report.lessons_downvoted += a.downvoted;
    report.lessons_deleted += a.deleted;
    report.duplicates += a.duplicates;
    report.unknown_ids += a.unknown_ids;
    for t in unit {
        store.learned.insert(t.id.clone());
        report.trials_learned += 1;
    }
}

/// Header of a project's lessons file when online learning creates it.
const PROJECT_LESSONS_HEADER: &str = "<!--
Lessons gitagent learned while working in this project (`learning: online`).
A lesson appears here once two different tasks support it. Review them like
code: edit or delete lines freely. This comment is not sent to the model.
-->";

/// Online learning for everyday use (never in `--bench`): learns from one
/// finished run into `<project>/.gitagent/lessons.json` and renders
/// `<project>/.gitagent/prompts/lessons.md`, which later runs load on top of
/// the built-in lessons. Each run counts as its own task for the evidence
/// threshold, and nothing checked the result, so the learner is told so.
pub async fn learn_from_run(
    spec: &AgentSpec,
    provider: &dyn Provider,
    trajectory_path: &Path,
    project_dir: &Path,
) -> Result<LearnReport> {
    let text = std::fs::read_to_string(trajectory_path)
        .with_context(|| format!("reading {}", trajectory_path.display()))?;
    let trajectory: gitagent_record::atif::Trajectory = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", trajectory_path.display()))?;
    let (instruction, trace) = trace::render(&trajectory);
    // Each run is its own task; the counter keeps two runs in the same
    // millisecond apart.
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "{}@{}#{}",
        trajectory.session_id.as_deref().unwrap_or("run"),
        gitagent_record::utc_now_rfc3339(),
        RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let trial = Trial {
        id: id.clone(),
        task: id,
        reward: None,
        instruction,
        trace,
    };

    let dir = project_dir.join(".gitagent");
    let store_path = dir.join("lessons.json");
    let lessons_path = dir.join("prompts/lessons.md");
    let mut store = Store::load(&store_path)?;
    let mut report = LearnReport {
        trials_read: 1,
        ..Default::default()
    };
    learn_unit(spec, provider, &mut store, &[&trial], &mut report).await;
    report.lessons_total = store.lessons.len();
    report.rendered = store.render(PROJECT_MAX_CHARS, 2);
    report.lessons_shown = report.rendered.lines().count();
    store.save(&store_path)?;
    if !lessons_path.exists() {
        if let Some(d) = lessons_path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&lessons_path, format!("{PROJECT_LESSONS_HEADER}\n"))?;
    }
    write_lessons(&lessons_path, &report.rendered)?;
    Ok(report)
}

/// Size limit of a project's lessons file.
const PROJECT_MAX_CHARS: usize = 3000;

/// What to show the model at once: an accepted and a rejected attempt at the
/// same task together (so it can learn from the difference), the rest alone.
fn units(trials: &[Trial]) -> Vec<Vec<&Trial>> {
    let passed: Vec<&Trial> = trials.iter().filter(|t| t.passed()).collect();
    let failed: Vec<&Trial> = trials.iter().filter(|t| !t.passed()).collect();
    let mut out = Vec::new();
    let pairs = passed.len().min(failed.len()).min(1);
    for i in 0..pairs {
        out.push(vec![passed[i], failed[i]]);
    }
    out.extend(passed[pairs..].iter().map(|t| vec![*t]));
    out.extend(failed[pairs..].iter().map(|t| vec![*t]));
    out
}

fn ids(unit: &[&Trial]) -> String {
    unit.iter()
        .map(|t| t.id.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Rewrites the lessons file, keeping its header comment.
pub fn write_lessons(path: &Path, lessons: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let trimmed = existing.trim_start();
    let header = match trimmed.strip_prefix("<!--").and_then(|t| t.find("-->")) {
        Some(end) => &trimmed[..end + "<!--".len() + "-->".len()],
        None => "",
    };
    let mut out = String::new();
    if !header.is_empty() {
        out.push_str(header);
        out.push('\n');
    }
    out.push_str(lessons);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests;
