mod approval;
mod console;
mod init_cmd;
mod install_cmd;
mod ipc;
mod learn_cmd;
mod lineedit;
mod otel;
mod plugin_cmd;
mod repl;
mod repo_mode;
mod sandbox_exec;
mod schedule_cmd;
mod validate_cmd;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use gitagent_core::{Finished, Outcome, RunOptions};
use gitagent_loader::{AgentSpec, LoadOptions};
use gitagent_provider::Provider;
use gitagent_record::Events;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "gitagent",
    version,
    about = "A terminal agent whose behavior lives in version-controlled files.",
    after_help = "Without -p or --instruction-file, gitagent starts an interactive session in --dir.\n\
                  `gitagent learn --help` describes learning lessons from finished trials."
)]
pub(crate) struct Cli {
    /// Benchmark mode: built-in config only, no git commits, no memory.
    #[arg(long)]
    bench: bool,

    /// Conversation mode: a plain reply ends the run instead of
    /// `task_complete` (always on in the interactive session).
    #[arg(long, conflicts_with = "bench")]
    conversation: bool,

    /// The environment whose `config/<env>.yaml` in the agent directory
    /// applies (default: `GITAGENT_ENV`, else the defaults alone).
    #[arg(short = 'e', long, conflicts_with = "bench")]
    env: Option<String>,

    /// The task, given as plain words (the same as -p).
    #[arg(value_name = "PROMPT", conflicts_with_all = ["prompt", "instruction_file"])]
    text: Option<String>,

    /// Run this workflow of the agent (see `workflows/`) instead of a task.
    #[arg(long, conflicts_with_all = ["prompt", "instruction_file", "text", "bench", "repo", "ipc"])]
    workflow: Option<String>,

    /// An input for --workflow, as key=value (a JSON value when it parses
    /// as one); repeatable.
    #[arg(long, requires = "workflow")]
    input: Vec<String>,

    /// A remote sandbox is not available in this version.
    #[arg(long = "sandbox", hide = true)]
    remote_sandbox: bool,

    /// Voice mode is not available in this version.
    #[arg(long, hide = true, num_args = 0..=1, default_missing_value = "openai")]
    voice: Option<String>,

    /// Serve the IPC protocol on stdin and stdout (JSON Lines) for a client
    /// library; the session's settings come in its `initialize` message.
    #[arg(long, conflicts_with_all = ["bench", "prompt", "instruction_file", "repo"])]
    ipc: bool,

    /// Read the task from this file.
    #[arg(long, conflicts_with = "prompt")]
    instruction_file: Option<PathBuf>,

    /// The task, given inline.
    #[arg(short = 'p', long)]
    prompt: Option<String>,

    /// Model as provider:model (overrides gitagent.yaml).
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Profile: auto, a built-in name, or a path to a profile YAML file.
    #[arg(long)]
    profile: Option<String>,

    /// Path to gitagent.yaml; its prompts/ and profiles/ override the built-ins.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Directory the agent works in (default: current directory).
    #[arg(short = 'd', long)]
    dir: Option<PathBuf>,

    /// Where the trajectory, counters and logs are written.
    #[arg(long, default_value = "/tmp/gitagent")]
    out_dir: PathBuf,

    /// Wall-clock budget for the run in seconds (default: timeout_s from the config).
    #[arg(long)]
    timeout_s: Option<u64>,

    /// Session id recorded in the trajectory.
    #[arg(long)]
    session_id: Option<String>,

    /// Write events to stdout, one JSON object per line.
    #[arg(long, value_enum)]
    events: Option<EventFormat>,

    /// `text` prints the agent's summary; `stream-json` prints the SDK's
    /// messages (system, assistant, user, result), one per line.
    #[arg(long, value_enum, default_value = "text")]
    output_format: OutputFormat,

    /// Stop after this many model calls.
    #[arg(long)]
    max_turns: Option<u32>,

    /// Offer only these tools, comma-separated (`.claude/` or gitagent names).
    #[arg(long, value_delimiter = ',')]
    allowed_tools: Vec<String>,

    /// Never offer these tools, comma-separated (`.claude/` or gitagent names).
    #[arg(long, value_delimiter = ',')]
    disallowed_tools: Vec<String>,

    /// Text added after the system prompt.
    #[arg(long)]
    append_system_prompt: Option<String>,

    /// Clone this repository, work on a new branch and commit the result.
    #[arg(short = 'r', long)]
    repo: Option<String>,

    /// Token for the repository (default: GITAGENT_GIT_TOKEN, GITHUB_TOKEN
    /// or GIT_TOKEN).
    #[arg(long, requires = "repo")]
    pat: Option<String>,

    /// The session branch to work on in repo mode (the same as --branch).
    #[arg(long, requires = "repo", conflicts_with = "branch")]
    session: Option<String>,

    /// Branch to start from in repo mode (default: the remote's default branch).
    #[arg(long, requires = "repo")]
    base: Option<String>,

    /// Work on this branch in repo mode; an existing remote branch is resumed.
    #[arg(long, requires = "repo")]
    branch: Option<String>,

    /// Push the working branch after committing (never forced).
    #[arg(long, requires = "repo")]
    push: bool,

    /// After the run, keep the shells open (and anything still running in
    /// them) until they exit, instead of stopping them. For harnesses that
    /// check the environment after the agent ends; they can wait for the
    /// `done` file in the output directory.
    #[arg(long)]
    linger: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EventFormat {
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    StreamJson,
}

/// What every mode needs: the loaded configuration and a provider.
pub(crate) struct Setup {
    pub spec: AgentSpec,
    pub provider: Box<dyn Provider>,
    pub work_dir: PathBuf,
    pub timeout: Duration,
    pub events: Events,
    /// Directories the sandboxed shell may write to; `None` when the
    /// sandbox is off.
    pub sandbox: Option<Vec<PathBuf>>,
}

fn main() -> ExitCode {
    // The sandbox launcher runs before anything else and replaces itself
    // with the shell.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).and_then(|a| a.to_str()) == Some("__sandbox") {
        return sandbox_exec::run(&args[2..]);
    }
    if args.get(1).and_then(|a| a.to_str()) == Some("validate") {
        return validate_cmd::run(&args[2..]);
    }
    if args.get(1).and_then(|a| a.to_str()) == Some("install") {
        return install_cmd::run(&args[2..]);
    }
    if args.get(1).and_then(|a| a.to_str()) == Some("init") {
        return init_cmd::run(&args[2..]);
    }
    if args.get(1).and_then(|a| a.to_str()) == Some("plugin") {
        return plugin_cmd::run(&args[2..]);
    }
    let learn = args.get(1).and_then(|a| a.to_str()) == Some("learn");
    let schedule = args.get(1).and_then(|a| a.to_str()) == Some("schedule");
    let cli = (!learn && !schedule).then(Cli::parse);
    // `.env` files go into the environment before any thread starts;
    // never in benchmark runs.
    if let Some(cli) = cli.as_ref().filter(|c| !c.bench) {
        load_env_files(cli.dir.as_deref());
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) if learn => rt.block_on(learn_cmd::run(&args[2..])),
        Ok(rt) if schedule => rt.block_on(schedule_cmd::run(&args[2..])),
        Ok(rt) => rt.block_on(async_main(cli.expect("parsed above"))),
        Err(e) => usage_error(e.into()),
    }
}

/// Reads `~/.gitagent/.env`, then `<dir>/.env` (the working directory
/// without `--dir`), into the environment: `KEY=VALUE` lines, `#` comments,
/// quotes around a value removed, a later file winning. Values are never
/// printed.
pub(crate) fn load_env_files(dir: Option<&std::path::Path>) {
    let mut files = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        files.push(PathBuf::from(home).join(".gitagent/.env"));
    }
    let dir = dir
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    files.push(dir.join(".env"));
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            std::env::set_var(key, value);
        }
    }
}

async fn async_main(mut cli: Cli) -> ExitCode {
    if cli.remote_sandbox {
        return usage_error(anyhow::anyhow!(
            "--sandbox (a remote sandbox) is not available in this version; local runs are sandboxed on Linux"
        ));
    }
    if cli.voice.is_some() {
        return usage_error(anyhow::anyhow!(
            "--voice is not available in this version; use the interactive session or --ipc"
        ));
    }
    if cli.session.is_some() {
        cli.branch = cli.session.take();
    }
    let task = match task_from(&cli) {
        Ok(t) => t,
        Err(e) => return usage_error(e),
    };
    if cli.repo.is_some() && cli.bench {
        return usage_error(anyhow::anyhow!("--repo cannot be used with --bench"));
    }
    if let Some(name) = cli.workflow.clone() {
        return run_workflow(cli, name).await;
    }
    if cli.ipc {
        ipc::run(cli).await;
    }
    match task {
        Some(instruction) if cli.repo.is_some() => {
            let url = cli.repo.clone().unwrap_or_default();
            repo_mode::run(cli, url, instruction).await
        }
        None if cli.repo.is_some() => usage_error(anyhow::anyhow!(
            "--repo needs a task: give -p/--prompt or --instruction-file"
        )),
        Some(instruction) => single(cli, instruction).await,
        None if cli.bench => usage_error(anyhow::anyhow!(
            "--bench needs a task: give --instruction-file or -p/--prompt"
        )),
        None => match setup(&cli, true) {
            Ok(s) => match repl::run(s, &cli.out_dir).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => usage_error(e),
            },
            Err(e) => usage_error(e),
        },
    }
}

pub(crate) fn usage_error(e: anyhow::Error) -> ExitCode {
    eprintln!("gitagent: {e:#}");
    ExitCode::from(2)
}

fn task_from(cli: &Cli) -> Result<Option<String>> {
    let prompt = cli.prompt.clone().or_else(|| cli.text.clone());
    let task = match (&cli.instruction_file, &prompt) {
        (Some(path), None) => Some(
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
        ),
        (None, Some(p)) => Some(p.clone()),
        _ => None,
    };
    if task.as_deref().is_some_and(|t| t.trim().is_empty()) {
        bail!("the task is empty");
    }
    Ok(task)
}

/// Loads everything a run needs; `conversation` is conversation mode
/// (`--conversation`, and always in the interactive session).
pub(crate) fn setup(cli: &Cli, conversation: bool) -> Result<Setup> {
    // Absolute, because hooks, tools and MCP servers run in other
    // directories than gitagent itself.
    let work_dir = match &cli.dir {
        Some(d) => std::path::absolute(d).with_context(|| format!("resolving {}", d.display()))?,
        None => std::env::current_dir().context("reading the current directory")?,
    };
    // As in 1.x, a directory given with --dir that is not an agent yet is
    // started as one (the current directory only when it is empty).
    let is_agent =
        work_dir.join("agent.yaml").is_file() || work_dir.join("gitagent.yaml").is_file();
    let empty = std::fs::read_dir(&work_dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);
    if !cli.bench && !is_agent && (cli.dir.is_some() || empty) {
        // A scripted model is a test fixture, not a model to keep.
        let model = cli
            .model
            .as_deref()
            .filter(|m| !m.starts_with("scripted:"))
            .unwrap_or("claude-sonnet-5-5");
        match init_cmd::scaffold(&work_dir, &init_cmd::name_for(&work_dir), model, None) {
            Ok((created, note)) if !created.is_empty() => eprintln!(
                "gitagent: started an agent in {}: created {}{note}",
                work_dir.display(),
                created.join(", ")
            ),
            Ok(_) => {}
            Err(e) => eprintln!(
                "gitagent: could not start an agent in {}: {e}",
                work_dir.display()
            ),
        }
    } else if !cli.bench && !cli.ipc && is_agent {
        if let Some(note) = init_cmd::ensure_repo(&work_dir) {
            eprintln!(
                "gitagent: {} was not in a git repository{note}",
                work_dir.display()
            );
        }
    }
    // Benchmark runs use only the built-in files and explicit flags, so the
    // task's directory or the machine's home cannot change the agent.
    let (project_dir, home_dir) = if cli.bench {
        (None, None)
    } else {
        (
            Some(work_dir.clone()),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    };
    let spec = gitagent_loader::load(&LoadOptions {
        config_path: cli.config.clone(),
        model: cli.model.clone(),
        profile: cli.profile.clone(),
        project_dir,
        home_dir,
        conversation: conversation && !cli.bench,
        env: cli.env.clone(),
    })?;
    if !matches!(spec.config.learning.as_str(), "off" | "online") {
        bail!(
            "learning must be off or online, got {:?}",
            spec.config.learning
        );
    }
    let mut spec = spec;
    let agent_warnings = spec.agent.iter().flat_map(|a| a.warnings.iter());
    for w in agent_warnings.chain(spec.warnings.iter()) {
        eprintln!("gitagent: warning: {w}");
    }
    if let Some(n) = cli.max_turns {
        spec.config.max_turns = n;
    }
    if let Some(extra) = &cli.append_system_prompt {
        spec.system_prompt.push('\n');
        spec.system_prompt.push_str(extra.trim_end());
        spec.system_prompt.push('\n');
    }
    let provider = make_provider(&spec)?;
    let timeout = Duration::from_secs(cli.timeout_s.unwrap_or(spec.config.timeout_s));
    let events = match cli.events {
        Some(EventFormat::Jsonl) => Events::stdout(),
        None => Events::disabled(),
    };
    let sandbox = sandbox_dirs(&spec.config.sandbox, cli.bench, &work_dir, &cli.out_dir)?;
    eprintln!(
        "gitagent {}: model {} · config {} · profile {} · {}{}{} · sandbox {}",
        env!("CARGO_PKG_VERSION"),
        spec.model,
        spec.config_source,
        spec.profile_source,
        work_dir.display(),
        if cli.bench { " · bench" } else { "" },
        spec.env_config
            .name
            .as_ref()
            .map(|e| format!(" · env {e}"))
            .unwrap_or_default(),
        if sandbox.is_some() { "on" } else { "off" }
    );
    Ok(Setup {
        spec,
        provider,
        work_dir,
        timeout,
        events,
        sandbox,
    })
}

/// One task (`-p`, `--instruction-file`, and `--bench`).
async fn single(cli: Cli, instruction: String) -> ExitCode {
    let out_dir = cli.out_dir.clone();
    let result = async {
        let s = setup(&cli, cli.conversation)?;
        let stream_json = cli.output_format == OutputFormat::StreamJson;
        let session_id = cli
            .session_id
            .clone()
            .unwrap_or_else(|| format!("gitagent-{}", std::process::id()));
        let tool_allowlist = (!cli.allowed_tools.is_empty())
            .then(|| gitagent_loader::sdk::normalize_tool_list(&cli.allowed_tools));
        let tool_denylist = gitagent_loader::sdk::normalize_tool_list(&cli.disallowed_tools);
        let mcp = connect_mcp(&s.spec).await;
        // Stream output: gitagent's events are converted as they happen.
        let converter = stream_json.then(|| {
            std::sync::Arc::new(std::sync::Mutex::new(gitagent_sdk::Converter::new(
                &session_id,
                &s.spec.model.to_string(),
                u64::from(s.spec.profile.context_window),
                None,
            )))
        });
        let events = match &converter {
            Some(c) => {
                let init = c.lock().unwrap().init(
                    &s.work_dir,
                    &gitagent_core::offered_tools(
                        &s.spec,
                        mcp.as_deref(),
                        None,
                        tool_allowlist.as_deref(),
                        &tool_denylist,
                    )
                    .into_iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>(),
                    &s.spec
                        .agents
                        .iter()
                        .map(|a| a.name.clone())
                        .collect::<Vec<_>>(),
                    &s.spec
                        .skills
                        .iter()
                        .map(|k| k.name.clone())
                        .collect::<Vec<_>>(),
                );
                println!("{}", serde_json::to_string(&init)?);
                let c = c.clone();
                Events::to_callback(move |event| {
                    for m in c.lock().unwrap().feed(&event) {
                        if let Ok(line) = serde_json::to_string(&m) {
                            println!("{line}");
                        }
                    }
                })
            }
            None => s.events.clone(),
        };
        let finished = gitagent_core::run(
            &s.spec,
            s.provider.as_ref(),
            RunOptions {
                instruction,
                work_dir: s.work_dir.clone(),
                out_dir: out_dir.clone(),
                timeout: Some(s.timeout),
                session_id: Some(session_id.clone()),
                events,
                resume: None,
                sandbox: s.sandbox.clone(),
                tool_allowlist,
                tool_denylist,
                hook_callbacks: None,
                mcp: mcp.clone(),
                approver: (!cli.bench).then(approver),
                kill: None,
                steer: None,
                external_tools: None,
            },
        )
        .await?;
        let finished = if cli.bench {
            finished
        } else {
            let finished = teardown(&s, finished, &out_dir, &session_id, mcp.clone()).await?;
            gitagent_core::end_session(&s.spec, &s.work_dir, &session_id).await;
            finished
        };
        report(&finished, !s.events.enabled() && !stream_json);
        if !cli.bench {
            export_telemetry(&finished, &s.spec).await;
        }
        learn_cmd::after_run(&s, &finished.report, &s.work_dir, cli.bench).await;
        anyhow::Ok(finished)
    }
    .await;

    let (code, finished) = match result {
        // Running out of turns or time is a normal end of a run; only a
        // provider failure gets its own exit code so harnesses can tell it apart.
        Ok(f) => (
            match f.report.outcome {
                Outcome::ProviderFailed(_) => 3,
                Outcome::Killed(_) => 130,
                _ => 0,
            },
            Some(f),
        ),
        Err(e) => {
            eprintln!("gitagent: {e:#}");
            (2, None)
        }
    };
    // Written last, after the trajectory and counters: a harness waiting for
    // this file can read everything else.
    if let Err(e) = std::fs::create_dir_all(&out_dir)
        .and_then(|_| std::fs::write(out_dir.join("done"), format!("{code}\n")))
    {
        eprintln!("gitagent: writing {}: {e}", out_dir.join("done").display());
    }
    if let Some(mut f) = finished.filter(|_| cli.linger) {
        eprintln!("gitagent: done; keeping the shells open until they exit");
        f.terminal.linger().await;
    }
    ExitCode::from(code)
}

/// `--workflow <name>`: runs one of the agent's workflows and prints its
/// report; exit code 1 when a step failed or the run stopped.
async fn run_workflow(cli: Cli, name: String) -> ExitCode {
    let result = async {
        let s = setup(&cli, true)?;
        let inputs =
            gitagent_core::workflow::parse_inputs(&cli.input).map_err(|e| anyhow::anyhow!(e))?;
        let session_id = cli
            .session_id
            .clone()
            .unwrap_or_else(|| format!("gitagent-{}", std::process::id()));
        let mcp = connect_mcp(&s.spec).await;
        let opts = RunOptions {
            instruction: String::new(),
            work_dir: s.work_dir.clone(),
            out_dir: cli.out_dir.clone(),
            timeout: Some(s.timeout),
            session_id: Some(session_id.clone()),
            events: s.events.clone(),
            resume: None,
            sandbox: s.sandbox.clone(),
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
            mcp,
            approver: Some(approver()),
            kill: None,
            steer: None,
            external_tools: None,
        };
        let report =
            gitagent_core::run_workflow(&s.spec, s.provider.as_ref(), &name, &inputs, opts).await?;
        gitagent_core::end_session(&s.spec, &s.work_dir, &session_id).await;
        anyhow::Ok(report)
    }
    .await;
    match result {
        Ok(report) => {
            println!("{}", report.text());
            ExitCode::from(u8::from(!report.succeeded()))
        }
        Err(e) => usage_error(e),
    }
}

/// Runs the agent's teardown turn, when it has one, on the finished run's
/// conversation and shell; the run's own result is kept.
pub(crate) async fn teardown(
    s: &Setup,
    finished: gitagent_core::Finished,
    out_dir: &std::path::Path,
    session_id: &str,
    mcp: Option<std::sync::Arc<gitagent_mcp::Servers>>,
) -> Result<gitagent_core::Finished> {
    if s.spec.teardown.is_none() {
        return Ok(finished);
    }
    let report = finished.report.clone();
    let opts = RunOptions {
        instruction: String::new(),
        work_dir: s.work_dir.clone(),
        out_dir: out_dir.join("teardown"),
        timeout: Some(s.timeout),
        session_id: Some(session_id.to_string()),
        events: s.events.clone(),
        resume: Some(finished.into_resume()),
        sandbox: s.sandbox.clone(),
        tool_allowlist: None,
        tool_denylist: Vec::new(),
        hook_callbacks: None,
        mcp,
        approver: Some(approver()),
        kill: None,
        steer: None,
        external_tools: None,
    };
    match gitagent_core::teardown(&s.spec, s.provider.as_ref(), opts).await {
        Some(Ok(mut done)) => {
            eprintln!(
                "gitagent: teardown: {}",
                done.report.summary.as_deref().unwrap_or("(no reply)")
            );
            done.report = report;
            Ok(done)
        }
        Some(Err(e)) => {
            // The shell went with the failed turn; the run's result stands.
            eprintln!("gitagent: teardown failed: {e:#}");
            let terminal = gitagent_terminal::Terminal::start(gitagent_terminal::ShellConfig::new(
                &s.work_dir,
                out_dir.join("teardown/commands"),
            ))
            .await?;
            Ok(gitagent_core::Finished {
                report,
                terminal,
                messages: Vec::new(),
            })
        }
        None => unreachable!("the agent has a teardown"),
    }
}

/// Asks the person at the terminal when a call needs approval.
pub(crate) fn approver() -> std::sync::Arc<dyn gitagent_core::Approver> {
    std::sync::Arc::new(approval::TtyApprover)
}

/// Connects the MCP servers for a session (none in benchmark runs, whose
/// spec has none), printing what could not be connected.
pub(crate) async fn connect_mcp(spec: &AgentSpec) -> Option<std::sync::Arc<gitagent_mcp::Servers>> {
    let (servers, warnings) = gitagent_core::connect_mcp(spec).await;
    for w in warnings {
        eprintln!("gitagent: warning: {w}");
    }
    servers
}

/// Sends traces and metrics when `OTEL_EXPORTER_OTLP_ENDPOINT` is set. A
/// failure is reported but does not change the run's result.
pub(crate) async fn export_telemetry(f: &Finished, spec: &AgentSpec) {
    if let Some(exporter) = otel::Exporter::from_env() {
        if let Err(e) = exporter.export(&f.report, &spec.model.to_string()).await {
            eprintln!("gitagent: telemetry export failed: {e:#}");
        }
    }
}

/// Prints the outcome: a status line on stderr and, unless events go to
/// stdout, the agent's summary on stdout.
pub(crate) fn report(f: &Finished, summary_to_stdout: bool) {
    let r = &f.report;
    eprintln!(
        "gitagent: {} after {} turns · tokens in {} (cached {}) out {} · {}",
        r.outcome,
        r.turns,
        r.usage.input_tokens,
        r.usage.cache_read_tokens,
        r.usage.output_tokens,
        r.trajectory_path.display()
    );
    if summary_to_stdout {
        match &r.summary {
            Some(s) => println!("{s}"),
            None => println!("(no summary: the run ended with {})", r.outcome),
        }
    }
}

/// `sandbox: auto` means on outside `--bench` (the benchmark container is the
/// isolation there) wherever the sandbox is available; `on` and `off` force it.
fn sandbox_dirs(
    setting: &str,
    bench: bool,
    work_dir: &std::path::Path,
    out_dir: &std::path::Path,
) -> Result<Option<Vec<PathBuf>>> {
    let on = match setting {
        "auto" => !bench && gitagent_sandbox::available(),
        "on" => {
            if !gitagent_sandbox::available() {
                bail!("sandbox: on needs Linux (Landlock and seccomp)");
            }
            true
        }
        "off" => false,
        other => bail!("sandbox must be auto, on or off, got {other:?}"),
    };
    if !on {
        return Ok(None);
    }
    let mut dirs = vec![
        work_dir.to_path_buf(),
        out_dir.to_path_buf(),
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/dev"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".cache"));
    }
    std::fs::create_dir_all(out_dir)?;
    Ok(Some(dirs))
}

pub(crate) fn make_provider(spec: &AgentSpec) -> Result<Box<dyn Provider>> {
    gitagent_provider::for_spec(spec)
}
