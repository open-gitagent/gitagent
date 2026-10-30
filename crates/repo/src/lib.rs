//! Repo mode (BLUEPRINT §5): clone a repository, work on a new branch,
//! commit when the task is done, and push only when asked.
//!
//! Rules: never commit to or push the base branch, never force-push, never
//! rewrite history. A token (from `GITAGENT_GIT_TOKEN` or `GITHUB_TOKEN`)
//! reaches git only through a `GIT_ASKPASS` helper: it is never put in a URL,
//! in `.git/config`, in a credential store, or in any log.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

/// Environment variable the askpass helper reads the token from. It is set
/// only on git commands that talk to the remote.
const TOKEN_VAR: &str = "GITAGENT_ASKPASS_TOKEN";

/// Pathspecs for the agent's own run records, which are never part of the
/// work: audit logs, state and stored sessions.
const NOT_WORK: [&str; 3] = [
    ":(exclude).gitagent/audit*",
    ":(exclude).gitagent/state.json",
    ":(exclude).gitagent/sessions",
];

#[derive(Debug, Clone)]
pub struct RepoOptions {
    pub url: String,
    /// Branch to start from; default: the remote's default branch.
    pub base: Option<String>,
    /// Continue an existing branch instead of creating one.
    pub branch: Option<String>,
    /// Where to clone; must not exist yet (or be empty).
    pub dir: PathBuf,
    pub branch_prefix: String,
    /// `Name <email>` for commits.
    pub author: String,
    pub token: Option<String>,
    /// A private directory for the askpass helper.
    pub helper_dir: PathBuf,
}

#[derive(Debug)]
pub struct Repo {
    pub dir: PathBuf,
    pub branch: String,
    pub base: String,
    /// True when an existing branch was checked out.
    pub resumed: bool,
    author_name: String,
    author_email: String,
    askpass: Option<PathBuf>,
    token: Option<String>,
}

impl Repo {
    /// Clones the repository and checks out the working branch.
    pub fn clone_for_task(opts: &RepoOptions, task: &str) -> Result<Repo> {
        // A clone of the same repository from an earlier session is reused,
        // as 1.x did, so a session branch can be resumed in place.
        let reuse = opts.dir.join(".git").exists();
        if !reuse
            && opts.dir.exists()
            && std::fs::read_dir(&opts.dir).map_or(true, |mut d| d.next().is_some())
        {
            bail!("{} already exists and is not empty", opts.dir.display());
        }
        let askpass = match &opts.token {
            Some(_) => Some(write_askpass(&opts.helper_dir)?),
            None => None,
        };
        let (name, email) = split_author(&opts.author);
        let mut repo = Repo {
            dir: opts.dir.clone(),
            branch: String::new(),
            base: String::new(),
            resumed: false,
            author_name: name,
            author_email: email,
            askpass,
            token: opts.token.clone(),
        };

        if reuse {
            let origin = repo
                .git(&["remote", "get-url", "origin"])
                .unwrap_or_default();
            if strip_credentials(origin.trim()) != strip_credentials(&opts.url) {
                bail!(
                    "{} is a clone of {}, not of {}",
                    opts.dir.display(),
                    strip_credentials(origin.trim()),
                    strip_credentials(&opts.url)
                );
            }
            let mut status = vec!["status", "--porcelain", "--", "."];
            status.extend(NOT_WORK);
            if !repo.git(&status)?.trim().is_empty() {
                bail!(
                    "{} has uncommitted changes; commit or remove them first",
                    opts.dir.display()
                );
            }
            repo.remote_git(&opts.dir, &["fetch", "--prune", "--no-tags", "origin"])
                .context("fetching the repository")?;
            // origin/HEAD may be missing in an old clone.
            let _ = repo.remote_git(&opts.dir, &["remote", "set-head", "origin", "--auto"]);
        } else {
            let parent = opts.dir.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent)?;
            repo.remote_git(
                parent,
                &[
                    "clone",
                    "--filter=blob:none",
                    "--no-tags",
                    &opts.url,
                    &opts.dir.to_string_lossy(),
                ],
            )
            .context("cloning the repository")?;
        }

        repo.base = match &opts.base {
            Some(b) => b.clone(),
            None => repo.default_branch()?,
        };
        let remote_branches = repo.remote_branches()?;
        if !remote_branches.contains(&repo.base) {
            bail!(
                "the base branch {:?} does not exist on the remote",
                repo.base
            );
        }

        match &opts.branch {
            Some(b) if *b == repo.base => {
                bail!("refusing to work on the base branch {b:?}; pick another --branch")
            }
            Some(b) if repo.local_branch(b) => {
                // A session branch from an earlier run in this clone: keep
                // its commits and take the remote's if they follow on.
                repo.git(&["checkout", "-q", b])?;
                if remote_branches.contains(b) {
                    let _ = repo.git(&["merge", "-q", "--ff-only", &format!("origin/{b}")]);
                }
                repo.branch = b.clone();
                repo.resumed = true;
            }
            Some(b) if remote_branches.contains(b) => {
                repo.git(&["checkout", "-B", b, &format!("origin/{b}")])?;
                repo.branch = b.clone();
                repo.resumed = true;
            }
            Some(b) => {
                repo.git(&["checkout", "-b", b, &format!("origin/{}", repo.base)])?;
                repo.branch = b.clone();
            }
            None => {
                let name = format!("{}{}-{}", opts.branch_prefix, slug(task), short_id());
                repo.git(&["checkout", "-b", &name, &format!("origin/{}", repo.base)])?;
                repo.branch = name;
            }
        }
        if repo.branch == repo.base {
            bail!("refusing to work on the base branch {:?}", repo.base);
        }
        Ok(repo)
    }

    /// Commits every change except the agent's own run records (audit
    /// logs, state, stored sessions). Returns the commit id, or `None` when
    /// nothing changed.
    pub fn commit(&self, message: &str) -> Result<Option<String>> {
        self.check_branch()?;
        let mut add = vec!["add", "-A", "--", "."];
        add.extend(NOT_WORK);
        self.git(&add)?;
        if self.git_status(&["diff", "--cached", "--quiet"])? {
            return Ok(None);
        }
        let name = format!("user.name={}", self.author_name);
        let email = format!("user.email={}", self.author_email);
        self.git(&["-c", &name, "-c", &email, "commit", "-q", "-m", message])?;
        Ok(Some(self.git(&["rev-parse", "HEAD"])?.trim().to_string()))
    }

    /// Pushes the working branch. Never forces and never pushes the base.
    pub fn push(&self) -> Result<()> {
        self.check_branch()?;
        let refspec = format!("HEAD:refs/heads/{}", self.branch);
        self.remote_git(&self.dir, &["push", "-u", "origin", &refspec])
            .context("pushing the branch (it was not forced)")?;
        Ok(())
    }

    /// `git status --short --branch`, for the agent's first message.
    pub fn status(&self) -> Result<String> {
        self.git(&["status", "--short", "--branch"])
    }

    fn check_branch(&self) -> Result<()> {
        let current = self.git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        let current = current.trim();
        if current == self.base {
            bail!("refusing to commit or push on the base branch {current:?}");
        }
        if current != self.branch {
            bail!(
                "the working branch changed to {current:?} (expected {:?}); not committing",
                self.branch
            );
        }
        Ok(())
    }

    fn local_branch(&self, name: &str) -> bool {
        self.git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .is_ok()
    }

    fn default_branch(&self) -> Result<String> {
        let head = self.git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])?;
        Ok(head.trim().trim_start_matches("origin/").to_string())
    }

    fn remote_branches(&self) -> Result<Vec<String>> {
        let out = self.git(&[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/remotes/origin",
        ])?;
        Ok(out
            .lines()
            .filter_map(|l| l.strip_prefix("origin/"))
            .filter(|b| *b != "HEAD")
            .map(str::to_string)
            .collect())
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let out = self.command(&self.dir, args, false).output()?;
        check(out, args)
    }

    /// Runs git and reports whether it exited 0 (for `--quiet` checks).
    fn git_status(&self, args: &[&str]) -> Result<bool> {
        Ok(self.command(&self.dir, args, false).status()?.success())
    }

    /// A git command that talks to the remote: gets the token through the
    /// askpass helper, and no credential helper may store it.
    fn remote_git(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let out = self.command(cwd, args, true).output()?;
        check(out, args)
    }

    fn command(&self, cwd: &Path, args: &[&str], remote: bool) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove(TOKEN_VAR)
            .env_remove("GITAGENT_GIT_TOKEN")
            .env_remove("GITHUB_TOKEN");
        if remote {
            cmd.args(["-c", "credential.helper="]);
            if let (Some(helper), Some(token)) = (&self.askpass, &self.token) {
                cmd.env("GIT_ASKPASS", helper).env(TOKEN_VAR, token);
            }
        }
        cmd.args(args);
        cmd
    }
}

fn check(out: Output, args: &[&str]) -> Result<String> {
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    bail!(
        "git {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// The helper prints a user name or the token from the environment. The
/// script itself holds no secret.
fn write_askpass(dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("git-askpass.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  Username*) echo x-access-token ;;\n  *) printf '%s\\n' \"${TOKEN_VAR}\" ;;\nesac\n"
        ),
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn split_author(author: &str) -> (String, String) {
    match author.split_once('<') {
        Some((name, rest)) => (
            name.trim().to_string(),
            rest.trim_end_matches('>').trim().to_string(),
        ),
        None => (author.trim().to_string(), "gitagent@localhost".to_string()),
    }
}

/// Lowercase letters, digits and dashes from the start of the task.
/// `url` without a user or token in it.
pub fn strip_credentials(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme), Some(at)) if at > scheme => {
            format!("{}{}", &url[..scheme + 3], &url[at + 1..])
        }
        _ => url.to_string(),
    }
}

pub fn slug(task: &str) -> String {
    let mut out = String::new();
    for c in task.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

fn short_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{:06x}",
        (nanos ^ u128::from(std::process::id())) & 0xff_ffff
    )
}

#[cfg(test)]
mod tests;
