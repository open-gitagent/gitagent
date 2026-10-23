//! `gitagent init [dir] [--name] [--model]`: starts an agent directory with
//! the files gitagent reads (agent.yaml, SOUL.md, memory/MEMORY.md,
//! .gitignore) and a git repository, leaving anything already there alone.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "gitagent init",
    about = "Start an agent directory: agent.yaml, SOUL.md, memory/MEMORY.md and a git repository."
)]
struct InitCli {
    /// The directory (default: the current directory; created if missing).
    dir: Option<PathBuf>,

    /// The agent's name (default: the directory's name).
    #[arg(long)]
    name: Option<String>,

    /// The model to prefer, as provider:model or a model id.
    #[arg(short = 'm', long, default_value = "claude-sonnet-5-5")]
    model: String,

    /// One line on what the agent does.
    #[arg(long)]
    description: Option<String>,
}

pub(crate) fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = match InitCli::try_parse_from(
        std::iter::once("gitagent init".into()).chain(args.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let dir = cli.dir.unwrap_or_else(|| PathBuf::from("."));
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("gitagent init: {}: {e}", dir.display());
        return ExitCode::from(2);
    }
    let name = cli.name.unwrap_or_else(|| name_for(&dir));
    match scaffold(&dir, &name, &cli.model, cli.description.as_deref()) {
        Ok((created, note)) => {
            if created.is_empty() {
                println!(
                    "gitagent init: {} already has its files{note}",
                    dir.display()
                );
            } else {
                println!(
                    "gitagent init: created {} in {}{note}",
                    created.join(", "),
                    dir.display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gitagent init: {e}");
            ExitCode::from(2)
        }
    }
}

/// The agent's name from a directory's name.
pub(crate) fn name_for(dir: &std::path::Path) -> String {
    let raw = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "agent".into());
    let mut name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    name = name.trim_matches('-').to_string();
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name = format!("agent-{name}").trim_matches('-').to_string();
    }
    name
}

/// Writes the files an agent directory needs where they are missing, and
/// starts a git repository if the directory is not in one. Gives the files
/// created and a note on git.
pub(crate) fn scaffold(
    dir: &std::path::Path,
    name: &str,
    model: &str,
    description: Option<&str>,
) -> Result<(Vec<&'static str>, String), String> {
    let description = description
        .map(str::to_string)
        .unwrap_or_else(|| format!("The {name} agent."));
    let files = [
        (
            "agent.yaml",
            format!(
                "spec_version: \"0.1.0\"\nname: {name}\nversion: 0.1.0\ndescription: {description}\nmodel:\n  preferred: {model}\n  fallback: []\nruntime:\n  max_turns: 50\n"
            ),
        ),
        (
            "SOUL.md",
            "# Identity\n\nYou are a helpful agent working in this repository. You run commands, read and write files, and remember what matters for later sessions. Be concise and act rather than speculate.\n".to_string(),
        ),
        ("memory/MEMORY.md", "# Memory\n".to_string()),
        (".gitignore", ".gitagent/\n".to_string()),
    ];
    let mut created = Vec::new();
    for (rel, text) in &files {
        let path = dir.join(rel);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        created.push(*rel);
    }
    let mut note = start_git(dir).unwrap_or_default();
    if git(dir, &["rev-parse", "--is-inside-work-tree"])
        && !created.is_empty()
        && commit_all(dir, "Start the agent")
    {
        note.push_str("; committed");
    }
    Ok((created, note))
}

fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn commit_all(dir: &std::path::Path, message: &str) -> bool {
    git(dir, &["add", "-A"])
        && git(
            dir,
            &[
                "-c",
                "user.name=gitagent",
                "-c",
                "user.email=gitagent@localhost",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                message,
            ],
        )
}

/// Starts a git repository in `dir` if it is not inside one; gives a note
/// for the user when something happened.
fn start_git(dir: &std::path::Path) -> Option<String> {
    if git(dir, &["rev-parse", "--is-inside-work-tree"]) {
        return None;
    }
    Some(if git(dir, &["init", "--quiet"]) {
        "; git repository started".into()
    } else {
        "; git is not available, so no repository was started".into()
    })
}

/// As 1.x's ensureRepo: an agent directory outside git becomes a repository
/// with a first commit, so memory saves can be committed. Gives a line for
/// the user when it did something.
pub(crate) fn ensure_repo(dir: &std::path::Path) -> Option<String> {
    let note = start_git(dir)?;
    if git(dir, &["rev-parse", "--is-inside-work-tree"]) && commit_all(dir, "Initial commit") {
        return Some(format!("{note}; committed what was there"));
    }
    Some(note)
}
