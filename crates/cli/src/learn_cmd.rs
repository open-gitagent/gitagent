//! `gitagent learn`: distils lessons from finished benchmark trials.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::Parser;
use gitagent_core::{Outcome, RunReport};
use gitagent_learn::LearnOptions;
use gitagent_loader::LoadOptions;

use crate::Setup;

#[derive(Debug, Parser)]
#[command(
    name = "gitagent learn",
    about = "Distil general lessons from gitagent's own finished trials into the lessons file.",
    long_about = "Reads each trial's agent/trajectory.json and the reward in its result.json \
                  (it never reads tests or verifier output), asks the model for lesson \
                  changes, rejects any that carry task details, and rewrites the lessons \
                  file. Review the diff before committing it. Learn from the dev split only."
)]
struct LearnCli {
    /// A Harbor job directory to learn from (repeatable).
    #[arg(long = "from", required = true)]
    jobs: Vec<PathBuf>,

    /// Split file whose `dev` list names the tasks that may be learned from.
    #[arg(long, default_value = "bench/split.json")]
    split: PathBuf,

    /// Learn from every trial because no split exists yet (only for tasks
    /// that will go in the dev split).
    #[arg(long)]
    no_split: bool,

    /// The lesson store with importance counts.
    #[arg(long, default_value = "bench/lessons/lessons.json")]
    store: PathBuf,

    /// The lessons file to rewrite.
    #[arg(long, default_value = "prompts/lessons.md")]
    lessons: PathBuf,

    /// Size limit of the lessons file in characters.
    #[arg(long, default_value_t = 3000)]
    max_chars: usize,

    /// Show a lesson to the agent only once attempts at this many different
    /// tasks support it.
    #[arg(long, default_value_t = 2)]
    min_tasks: usize,

    /// Model as provider:model.
    #[arg(long)]
    model: Option<String>,

    /// Profile: auto, a built-in name, or a path to a profile YAML file.
    #[arg(long)]
    profile: Option<String>,

    /// Show what would change without writing anything.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) async fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = LearnCli::parse_from(
        std::iter::once(std::ffi::OsString::from("gitagent learn")).chain(args.iter().cloned()),
    );
    match learn(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => crate::usage_error(e),
    }
}

async fn learn(cli: LearnCli) -> Result<()> {
    let dev_tasks = match (cli.split.is_file(), cli.no_split) {
        (true, _) => Some(gitagent_learn::read_split(&cli.split)?),
        (false, true) => None,
        (false, false) => bail!(
            "{} not found: learning needs the dev split; pass --no-split only for tasks that will be dev",
            cli.split.display()
        ),
    };
    // Built-in configuration only, as in --bench.
    let spec = gitagent_loader::load(&LoadOptions {
        model: cli.model.clone(),
        profile: cli.profile.clone(),
        ..Default::default()
    })?;
    let provider = crate::make_provider(&spec)?;
    eprintln!(
        "gitagent learn: model {} · {} job(s)",
        spec.model,
        cli.jobs.len()
    );
    let report = gitagent_learn::learn(
        &spec,
        provider.as_ref(),
        &LearnOptions {
            jobs: cli.jobs,
            store_path: cli.store,
            lessons_path: cli.lessons.clone(),
            dev_tasks,
            max_chars: cli.max_chars,
            min_tasks: cli.min_tasks,
            dry_run: cli.dry_run,
        },
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if cli.dry_run {
        println!("\nLessons after this pass:\n{}", report.rendered);
    } else {
        eprintln!(
            "gitagent learn: {} lessons stored, {} shown; review the diff of {} before committing",
            report.lessons_total,
            report.lessons_shown,
            cli.lessons.display()
        );
    }
    Ok(())
}

/// Online learning after a finished task: with `learning: online`, outside
/// `--bench`, and only when the agent completed the task. Returns whether
/// lessons were learned. A failure is reported but does not change the run.
pub(crate) async fn after_run(s: &Setup, run: &RunReport, project_dir: &Path, bench: bool) -> bool {
    if bench || s.spec.config.learning != "online" || run.outcome != Outcome::Completed {
        return false;
    }
    let result = gitagent_learn::learn_from_run(
        &s.spec,
        s.provider.as_ref(),
        &run.trajectory_path,
        project_dir,
    )
    .await;
    match result {
        Ok(r) => {
            s.events
                .emit("learn", serde_json::to_value(&r).unwrap_or_default());
            eprintln!(
                "gitagent: learned {} new, {} upvoted, {} rejected · {} lessons stored, {} in .gitagent/prompts/lessons.md",
                r.lessons_added, r.lessons_upvoted, r.ops_rejected, r.lessons_total, r.lessons_shown
            );
            r.trials_learned > 0
        }
        Err(e) => {
            eprintln!("gitagent: learning from this run failed: {e:#}");
            false
        }
    }
}
