//! `--repo <url>`: clone, work on a new branch, commit when the task is
//! accepted, push only with `--push`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use gitagent_core::{Outcome, RunOptions};
use gitagent_repo::{Repo, RepoOptions};
use serde_json::json;

use crate::{report, setup, Cli};

pub(crate) async fn run(mut cli: Cli, url: String, task: String) -> ExitCode {
    match run_inner(&mut cli, url, task).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gitagent: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run_inner(cli: &mut Cli, url: String, task: String) -> Result<ExitCode> {
    let token = ["GITAGENT_GIT_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    let dir = match &cli.dir {
        Some(d) => d.clone(),
        None => std::env::current_dir()?.join(repo_name(&url)),
    };
    // Branch prefix, author and push default come from the user's config.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let home_cfg = gitagent_loader::load_config(cli.config.as_deref(), home.as_deref())?.repo;

    let opts = RepoOptions {
        url: url.clone(),
        base: cli.base.clone(),
        branch: cli.branch.clone(),
        dir: dir.clone(),
        branch_prefix: home_cfg.branch_prefix.clone(),
        author: home_cfg.author.clone(),
        token,
        helper_dir: cli.out_dir.join("askpass"),
    };
    let repo = Repo::clone_for_task(&opts, &task)?;

    cli.dir = Some(repo.dir.clone());
    let s = setup(cli)?;
    let events = s.events.clone();
    events.emit(
        "repo",
        json!({"action": "clone", "url": without_credentials(&url), "dir": repo.dir}),
    );
    events.emit(
        "repo",
        json!({"action": "branch", "branch": repo.branch, "base": repo.base, "resumed": repo.resumed}),
    );
    eprintln!(
        "gitagent: working on branch {} (from {}) in {}",
        repo.branch,
        repo.base,
        repo.dir.display()
    );

    let instruction = format!(
        "{}\n\nYou are working in a git clone on branch `{}` (based on `{}`). Your changes are committed for you when you call task_complete; do not commit, push or switch branches yourself.\n\nCurrent `git status`:\n{}",
        task.trim(),
        repo.branch,
        repo.base,
        repo.status().unwrap_or_default().trim_end()
    );
    let finished = gitagent_core::run(
        &s.spec,
        s.provider.as_ref(),
        RunOptions {
            instruction,
            work_dir: repo.dir.clone(),
            out_dir: cli.out_dir.clone(),
            timeout: Some(s.timeout),
            session_id: cli.session_id.clone(),
            events: events.clone(),
            resume: None,
            sandbox: s.sandbox.clone(),
            tool_allowlist: None,
            tool_denylist: Vec::new(),
            hook_callbacks: None,
        },
    )
    .await?;
    report(&finished, !events.enabled());
    crate::export_telemetry(&finished, &s.spec).await;
    drop(finished.terminal);

    let mut commit = None;
    let mut pushed = false;
    if finished.report.outcome == Outcome::Completed {
        let subject = finished
            .report
            .summary
            .as_deref()
            .unwrap_or(&task)
            .lines()
            .next()
            .unwrap_or("changes")
            .to_string();
        let subject = shorten(&subject, 72);
        commit = repo
            .commit(&format!("gitagent: {subject}"))
            .context("committing the changes")?;
        events.emit("repo", json!({"action": "commit", "sha": commit}));
        // Lessons go in their own commit so they are reviewed with the work.
        if crate::learn_cmd::after_run(&s, &finished.report, &repo.dir, false).await {
            if let Some(sha) = repo
                .commit("gitagent: update learned lessons")
                .context("committing the lessons")?
            {
                events.emit("repo", json!({"action": "commit", "sha": sha}));
                commit = Some(sha);
            }
        }
        if (cli.push || home_cfg.push) && (commit.is_some() || repo.resumed) {
            repo.push()?;
            pushed = true;
            events.emit("repo", json!({"action": "push", "branch": repo.branch}));
        }
    } else {
        eprintln!(
            "gitagent: the task did not complete ({}); nothing was committed. The work is in {}",
            finished.report.outcome,
            repo.dir.display()
        );
    }
    eprintln!(
        "gitagent: branch {} · commit {} · {}",
        repo.branch,
        commit
            .as_deref()
            .unwrap_or("none (no changes or not completed)"),
        if pushed { "pushed" } else { "not pushed" }
    );
    Ok(
        if matches!(finished.report.outcome, Outcome::ProviderFailed(_)) {
            ExitCode::from(3)
        } else {
            ExitCode::SUCCESS
        },
    )
}

/// Cuts `text` to at most `max` characters at a word boundary.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    match cut.rfind(' ') {
        Some(i) if i > max / 2 => format!("{}…", cut[..i].trim_end_matches([',', ';', ':'])),
        _ => format!("{cut}…"),
    }
}

/// The last path part of the URL without `.git`.
fn repo_name(url: &str) -> String {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo");
    let name = last.trim_end_matches(".git");
    if name.is_empty() {
        "repo".into()
    } else {
        name.into()
    }
}

/// Removes `user:password@` from a URL before it is shown or logged.
fn without_credentials(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme), Some(at)) if at > scheme => {
            format!("{}{}", &url[..scheme + 3], &url[at + 1..])
        }
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_credentials() {
        assert_eq!(repo_name("https://github.com/org/app.git"), "app");
        assert_eq!(repo_name("git@github.com:org/tool"), "tool");
        assert_eq!(repo_name("/tmp/origin.git/"), "origin");
        assert_eq!(
            without_credentials("https://user:tok@github.com/org/app.git"),
            "https://github.com/org/app.git"
        );
        assert_eq!(
            without_credentials("git@github.com:org/app.git"),
            "git@github.com:org/app.git"
        );
    }
}
