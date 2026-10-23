//! `gitagent schedule <list|add|remove|enable|disable|run|start> …`: the
//! agent's `schedules/*.yaml`, as gitagent 1.x ran them from its server.
//!
//! `start` keeps running and fires each enabled schedule when it is due,
//! one job at a time, in a fresh conversation each; `run <id>` fires one
//! now. Every job is logged to `.gitagent/schedule-logs/<id>.jsonl` and
//! recorded in its file (`lastRunAt`, `lastResult`; a `once` schedule is
//! then disabled). Cron times are local, as in 1.x.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use gitagent_core::{Outcome, RunOptions};
use gitagent_loader::schedules::{self, NewSchedule, Schedule};
use serde_json::{json, Value};

use crate::{Cli, Setup};

#[derive(Debug, Parser)]
#[command(name = "gitagent schedule", about = "Run prompts on a timetable.")]
struct ScheduleCli {
    /// The agent directory (default: the current directory).
    #[arg(short = 'd', long, global = true)]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Parser)]
struct RunArgs {
    /// The model, as provider:model or a model id.
    #[arg(short = 'm', long)]
    model: Option<String>,
    /// The environment (config/<env>.yaml).
    #[arg(short = 'e', long)]
    env: Option<String>,
    /// Where run records go (default: /tmp/gitagent/schedules).
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the schedules with their next run.
    #[command(alias = "ls")]
    List,
    /// Add a schedule: `add nightly --cron "0 2 * * *" --prompt "Tidy the notes."`
    Add {
        id: String,
        /// Five (or six, with seconds) cron fields, local time.
        #[arg(long, conflicts_with = "at")]
        cron: Option<String>,
        /// One run at this time, like 2026-11-01T09:00:00Z.
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        prompt: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Fire one time, then turn off.
        #[arg(long)]
        once: bool,
    },
    /// Delete a schedule's file.
    #[command(alias = "rm")]
    Remove {
        id: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    /// Run one schedule now.
    Run {
        id: String,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Keep running and fire the schedules when they are due (Ctrl-C stops).
    Start {
        #[command(flatten)]
        run: RunArgs,
        /// Stop after this many jobs (for tests and one-off use).
        #[arg(long, hide = true)]
        max_runs: Option<u32>,
    },
}

pub(crate) async fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = match ScheduleCli::try_parse_from(
        std::iter::once("gitagent schedule".into()).chain(args.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let dir = cli.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    let fail = |e: String| {
        eprintln!("gitagent schedule: {e}");
        ExitCode::from(1)
    };
    match cli.command {
        Command::List => {
            let (found, problems) = schedules::discover(&dir);
            for p in problems {
                eprintln!("gitagent schedule: warning: {p}");
            }
            if found.is_empty() {
                println!("No schedules in {}.", dir.join("schedules").display());
            }
            let now = now_unix();
            let offset = local_offset();
            for s in &found {
                let next = match s.next_after(now, offset) {
                    Some(t) => format!("next {}", schedules::format_local(t, offset)),
                    None if !s.enabled => "disabled".to_string(),
                    None => "never".to_string(),
                };
                let last = match (&s.last_run_at, &s.last_result) {
                    (Some(at), Some(r)) => format!(" · last {at} {r}"),
                    (Some(at), None) => format!(" · last {at}"),
                    _ => String::new(),
                };
                println!(
                    "{} — {} — {next}{last}{}",
                    s.id,
                    s.when(),
                    if s.description.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", s.description)
                    }
                );
            }
            ExitCode::SUCCESS
        }
        Command::Add {
            id,
            cron,
            at,
            prompt,
            description,
            once,
        } => {
            match schedules::save(
                &dir,
                &NewSchedule {
                    id,
                    description,
                    prompt,
                    cron,
                    run_at: at,
                    once,
                },
            ) {
                Ok(path) => {
                    println!("gitagent schedule: wrote {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        Command::Remove { id } => match find(&dir, &id) {
            Ok(s) => match std::fs::remove_file(&s.path) {
                Ok(()) => {
                    println!("gitagent schedule: removed {}", s.path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{}: {e}", s.path.display())),
            },
            Err(e) => fail(e),
        },
        Command::Enable { id } => {
            set_enabled(&dir, &id, true).map_or_else(fail, |_| ExitCode::SUCCESS)
        }
        Command::Disable { id } => {
            set_enabled(&dir, &id, false).map_or_else(fail, |_| ExitCode::SUCCESS)
        }
        Command::Run { id, run } => {
            let s = match find(&dir, &id) {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let mut runner = match Runner::new(&dir, &run) {
                Ok(r) => r,
                Err(e) => return fail(format!("{e:#}")),
            };
            let ok = runner.job(&s, s.once).await;
            runner.finish().await;
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Command::Start { run, max_runs } => {
            let mut runner = match Runner::new(&dir, &run) {
                Ok(r) => r,
                Err(e) => return fail(format!("{e:#}")),
            };
            let code = runner.serve(max_runs).await;
            runner.finish().await;
            code
        }
    }
}

fn find(dir: &Path, id: &str) -> Result<Schedule, String> {
    let (found, _) = schedules::discover(dir);
    found.into_iter().find(|s| s.id == id).ok_or_else(|| {
        format!(
            "there is no schedule {id:?} in {}",
            dir.join("schedules").display()
        )
    })
}

fn set_enabled(dir: &Path, id: &str, enabled: bool) -> Result<(), String> {
    let s = find(dir, id)?;
    schedules::update(&s.path, &[("enabled", Value::Bool(enabled))])?;
    println!(
        "gitagent schedule: {id} {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// The local zone's offset from UTC in seconds, from `date +%z` (so TZ and
/// daylight saving are the system's); UTC when that fails.
pub(crate) fn local_offset() -> i64 {
    let out = std::process::Command::new("date").arg("+%z").output();
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return 0,
    };
    let (sign, digits) = match text.split_at_checked(1) {
        Some(("-", d)) => (-1, d),
        Some(("+", d)) => (1, d),
        _ => return 0,
    };
    if digits.len() != 4 {
        return 0;
    }
    let h: i64 = digits[..2].parse().unwrap_or(0);
    let m: i64 = digits[2..].parse().unwrap_or(0);
    sign * (h * 3600 + m * 60)
}

struct Runner {
    dir: PathBuf,
    out_dir: PathBuf,
    setup: Setup,
    mcp: Option<std::sync::Arc<gitagent_mcp::Servers>>,
    session_id: String,
    runs: u32,
}

impl Runner {
    fn new(dir: &Path, args: &RunArgs) -> anyhow::Result<Runner> {
        crate::load_env_files(Some(dir));
        let mut argv: Vec<std::ffi::OsString> = vec![
            "gitagent".into(),
            "--conversation".into(),
            "--dir".into(),
            dir.into(),
        ];
        if let Some(m) = &args.model {
            argv.push("--model".into());
            argv.push(m.into());
        }
        if let Some(e) = &args.env {
            argv.push("--env".into());
            argv.push(e.into());
        }
        let out_dir = args
            .out_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("/tmp/gitagent/schedules"));
        argv.push("--out-dir".into());
        argv.push(out_dir.clone().into());
        let cli = Cli::try_parse_from(argv)?;
        let setup = crate::setup(&cli, true)?;
        Ok(Runner {
            dir: dir.to_path_buf(),
            out_dir,
            setup,
            mcp: None,
            session_id: format!("schedule-{}", std::process::id()),
            runs: 0,
        })
    }

    async fn connect(&mut self) {
        if self.mcp.is_none() {
            self.mcp = crate::connect_mcp(&self.setup.spec).await;
        }
    }

    /// Runs one job: the prompt as a fresh conversation. Returns whether it
    /// completed.
    async fn job(&mut self, s: &Schedule, disable_after: bool) -> bool {
        self.connect().await;
        self.runs += 1;
        let ts = schedules::utc_now_rfc3339();
        eprintln!("gitagent schedule: running {:?} at {ts}", s.id);
        self.setup.events.emit(
            "schedule",
            json!({"action": "start", "id": s.id, "prompt": s.prompt, "ts": ts}),
        );
        let out_dir = self
            .out_dir
            .join(&s.id)
            .join(ts.replace([':', '-'], "").trim_end_matches('Z'));
        let opts = RunOptions {
            instruction: s.prompt.clone(),
            work_dir: self.setup.work_dir.clone(),
            out_dir: out_dir.clone(),
            timeout: Some(self.setup.timeout),
            session_id: Some(format!("{}-{}", self.session_id, s.id)),
            events: self.setup.events.clone(),
            resume: None,
            sandbox: self.setup.sandbox.clone(),
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp: self.mcp.clone(),
            approver: Some(crate::approver()),
            kill: None,
            steer: None,
            external_tools: None,
        };
        let (success, result) =
            match gitagent_core::run(&self.setup.spec, self.setup.provider.as_ref(), opts).await {
                Ok(finished) => {
                    crate::report(&finished, false);
                    crate::export_telemetry(&finished, &self.setup.spec).await;
                    crate::learn_cmd::after_run(
                        &self.setup,
                        &finished.report,
                        &self.setup.work_dir,
                        false,
                    )
                    .await;
                    let ok = finished.report.outcome == Outcome::Completed;
                    let text = finished
                        .report
                        .summary
                        .clone()
                        .unwrap_or_else(|| finished.report.outcome.to_string());
                    (ok, text)
                }
                Err(e) => (false, format!("{e:#}")),
            };
        let log_dir = self.dir.join(".gitagent/schedule-logs");
        let line =
            json!({"ts": ts, "success": success, "result": cut(&result, 5000), "out_dir": out_dir})
                .to_string();
        if let Err(e) = std::fs::create_dir_all(&log_dir).and_then(|_| {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join(format!("{}.jsonl", s.id)))
                .and_then(|mut f| writeln!(f, "{line}"))
        }) {
            eprintln!("gitagent schedule: could not write the log: {e}");
        }
        let mut changes = vec![
            ("lastRunAt", Value::String(ts.clone())),
            (
                "lastResult",
                Value::String(if success { "success" } else { "error" }.into()),
            ),
        ];
        if disable_after {
            changes.push(("enabled", Value::Bool(false)));
        }
        if let Err(e) = schedules::update(&s.path, &changes) {
            eprintln!(
                "gitagent schedule: could not record the run in {}: {e}",
                s.path.display()
            );
        }
        self.setup.events.emit(
            "schedule",
            json!({"action": "result", "id": s.id, "success": success, "result": cut(&result, 2000), "ts": ts}),
        );
        eprintln!(
            "gitagent schedule: {:?} {}{}",
            s.id,
            if success { "completed" } else { "failed" },
            if disable_after {
                "; turned off (once)"
            } else {
                ""
            }
        );
        success
    }

    /// Fires schedules as they come due until Ctrl-C (or `max_runs` jobs).
    async fn serve(&mut self, max_runs: Option<u32>) -> ExitCode {
        let offset = local_offset();
        loop {
            let (found, problems) = schedules::discover(&self.dir);
            for p in problems {
                eprintln!("gitagent schedule: warning: {p}");
            }
            let now = now_unix();
            let mut due: Vec<(i64, Schedule)> = found
                .into_iter()
                .filter_map(|s| s.next_after(now - 1, offset).map(|t| (t, s)))
                .collect();
            due.sort_by_key(|(t, _)| *t);
            if self.runs == 0 {
                let active = due.len();
                eprintln!(
                    "gitagent schedule: watching {} ({active} active); Ctrl-C stops",
                    self.dir.join("schedules").display()
                );
                for (t, s) in &due {
                    eprintln!("  {} — {}", s.id, schedules::format_local(*t, offset));
                }
            }
            let Some((at, next)) = due.into_iter().next() else {
                if max_runs.is_some() {
                    eprintln!("gitagent schedule: nothing left to run");
                    return ExitCode::SUCCESS;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => continue,
                    _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
                }
            };
            let wait = (at - now_unix()).max(0) as u64;
            if wait > 0 {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                    _ = tokio::signal::ctrl_c() => {
                        eprintln!("\ngitagent schedule: stopped");
                        return ExitCode::SUCCESS;
                    }
                }
                // The files may have changed while waiting.
                let still = schedules::discover(&self.dir)
                    .0
                    .into_iter()
                    .find(|s| s.id == next.id);
                match still {
                    Some(s) if s.enabled && s.next_after(at - 1, offset) == Some(at) => {}
                    _ => continue,
                }
            }
            self.job(&next, next.once).await;
            if max_runs.is_some_and(|n| self.runs >= n) {
                return ExitCode::SUCCESS;
            }
        }
    }

    async fn finish(&mut self) {
        if self.runs > 0 {
            gitagent_core::end_session(&self.setup.spec, &self.setup.work_dir, &self.session_id)
                .await;
        }
    }
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}
