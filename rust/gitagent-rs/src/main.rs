//! Ira CLI — a thin front-end over the SDK. All agent behavior lives in the
//! library (`ira::sdk` / `::pi`); this binary only parses flags and hands
//! off to the `cli` renderer.

mod cli;
mod tui;


use clap::Parser;
use ira::sdk::env::{load_env, load_user_env};
use ira::sdk::goal::Goal;
use ira::sdk::query::QueryOptions;
use ira::sdk::session::{repo_name, RepoOptions};
use std::path::PathBuf;

/// Ira — a git-native AI agent (Rust core, faithful pi-core engine).
#[derive(Parser)]
#[command(name = "ira", version, about)]
struct Args {
    /// Agent directory (contains agent.yaml). With --repo, the clone target.
    #[arg(short, long, default_value = ".")]
    dir: String,
    /// Override model, e.g. "openai:gpt-4o-mini@http://localhost:8090/v1".
    #[arg(short, long)]
    model: Option<String>,
    /// Clone a GitHub repo and run the agent from it on a session branch.
    #[arg(short, long)]
    repo: Option<String>,
    /// GitHub PAT (or set GITHUB_TOKEN / GIT_TOKEN).
    #[arg(long)]
    pat: Option<String>,
    /// With --repo: also run code the repository supplies (mcp.yaml servers,
    /// hooks.yaml, tools/*.yaml) and load its .env. Off by default. Same as
    /// GITAGENT_TRUST_REPO=1.
    #[arg(long)]
    trust_repo: bool,
    /// Resume an existing session branch (e.g. gitagent/session-abc12345).
    #[arg(long)]
    session: Option<String>,
    /// Permission mode: plan | acceptEdits | bypass | default.
    #[arg(long)]
    permission_mode: Option<String>,
    /// Test-driven goal mode: a shell command that must exit 0 for success.
    /// The agent retries (self-correcting) until it passes or attempts run out.
    #[arg(long)]
    verify: Option<String>,
    /// Max self-correcting attempts in goal mode (default 3).
    #[arg(long)]
    max_attempts: Option<u32>,
    /// Launch the full-screen chat TUI (interactive mode).
    #[arg(long)]
    tui: bool,
    /// Launch the desktop web UI (Heritage design) in your browser.
    #[arg(long)]
    ui: bool,
    /// The prompt to run. Omit for interactive mode.
    prompt: Option<String>,
}

#[tokio::main]
async fn main() {
    let mut args = Args::parse();

    // Resolve the agent dir to an absolute path and fail FAST with a helpful
    // message if it isn't an agent — not at the first message. (Repo mode is
    // exempt: the clone target may not exist yet.)
    if args.repo.is_none() {
        let dir = PathBuf::from(&args.dir);
        let abs = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !abs.join("agent.yaml").exists() {
            eprintln!("error: no agent.yaml in '{}'", abs.display());
            eprintln!("hint:  pass the agent directory explicitly, e.g.");
            eprintln!("         ira -d /path/to/agent {}", if args.ui { "--ui" } else { "\"your prompt\"" });
            eprintln!("       or cd into a directory that contains agent.yaml first.");
            std::process::exit(2);
        }
        args.dir = abs.display().to_string();
    }

    // Load ~/.gitagent/.env then <dir>/.env (API keys, GITAGENT_MODEL_BASE_URL).
    // A --repo working copy is untrusted: only the user's own .env is loaded.
    if args.repo.is_some() && !trust_repo(&args) {
        load_user_env();
    } else {
        load_env(&PathBuf::from(&args.dir));
    }

    let code = match (args.verify.clone(), args.prompt.clone()) {
        // Test-driven goal loop (needs a prompt + a verify command).
        (Some(verify), Some(prompt)) => {
            cli::run_goal_cli(
                Goal {
                    prompt,
                    verify: Some(verify),
                    max_attempts: args.max_attempts.unwrap_or(3),
                    model: args.model.clone(),
                    reflect: true,
                },
                PathBuf::from(&args.dir),
            )
            .await
        }
        // One-shot streaming run.
        (None, Some(prompt)) => {
            let repo = build_repo(&args);
            cli::run_stream(QueryOptions {
                dir: PathBuf::from(&args.dir),
                model: args.model,
                prompt,
                repo,
                permission_mode: args.permission_mode,
            })
            .await
        }
        // No prompt → interactive: web UI, TUI, or plain REPL.
        (_, None) => {
            if args.ui {
                ira::ui::run_ui(PathBuf::from(&args.dir), args.model, args.permission_mode).await
            } else if args.tui {
                let repo = build_repo(&args);
                tui::run_tui(PathBuf::from(&args.dir), args.model, repo, args.permission_mode).await
            } else {
                let repo = build_repo(&args);
                cli::run_interactive(PathBuf::from(&args.dir), args.model, repo, args.permission_mode).await
            }
        }
    };

    std::process::exit(code);
}

/// Whether the user opted in to running repository-supplied code.
fn trust_repo(args: &Args) -> bool {
    args.trust_repo
        || std::env::var("GITAGENT_TRUST_REPO").map(|v| matches!(v.trim(), "1" | "true" | "yes")).unwrap_or(false)
}

/// Build repo options when --repo is given.
fn build_repo(args: &Args) -> Option<RepoOptions> {
    let url = args.repo.as_ref()?;
    let token = args
        .pat
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GIT_TOKEN").ok())
        .unwrap_or_default();
    // Clone into --dir if given, else ./<repo-name>.
    let clone_dir = if args.dir == "." { PathBuf::from(repo_name(url)) } else { PathBuf::from(&args.dir) };
    Some(RepoOptions {
        url: url.clone(),
        token,
        dir: clone_dir,
        session: args.session.clone(),
        trust_repo: trust_repo(args),
    })
}
