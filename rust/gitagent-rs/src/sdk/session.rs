//! Local-repo session mode: clone a git repo, work on a `gitagent/session-<hash>`
//! branch, and commit/push on finalize. Faithful to the TS `initLocalSession`,
//! but every git call is argv (`Command`) — never a shell string — so the
//! load-time RCE the TS version shipped with (extends/session injection) can't
//! happen here.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct RepoOptions {
    pub url: String,
    pub token: String,
    /// Where to clone / reuse the working copy.
    pub dir: PathBuf,
    /// Resume an existing session branch instead of creating a new one.
    pub session: Option<String>,
    /// Run code the repository supplies (mcp.yaml servers, hooks.yaml,
    /// tools/*.yaml, its .env). Off by default: a cloned repo is untrusted.
    pub trust_repo: bool,
}

pub struct LocalSession {
    pub dir: PathBuf,
    pub branch: String,
    pub session_id: String,
    auth: GitAuth,
}

/// How git authenticates for this session. The token is passed to git through
/// the environment (`GIT_CONFIG_*` → `http.<origin>/.extraHeader`), so it never
/// appears in argv, in a remote URL, or in `.git/config`.
#[derive(Clone, Default)]
struct GitAuth {
    env: Vec<(String, String)>,
    /// Secrets to redact from any error text.
    secrets: Vec<String>,
    /// Stop git from walking above the working copy's parent.
    ceiling: Option<PathBuf>,
}

impl GitAuth {
    fn new(url: &str, token: &str, dir: &Path) -> GitAuth {
        let (_, userinfo) = split_userinfo(url);
        // Explicit token wins; else credentials embedded in the URL.
        let creds = if !token.is_empty() {
            Some(("x-access-token".to_string(), token.to_string()))
        } else {
            userinfo.map(|ui| match ui.split_once(':') {
                Some((u, p)) => (u.to_string(), p.to_string()),
                None => ("x-access-token".to_string(), ui.to_string()),
            })
        };
        let mut auth = GitAuth {
            ceiling: {
                let abs = if dir.is_absolute() {
                    dir.to_path_buf()
                } else {
                    std::env::current_dir().unwrap_or_default().join(dir)
                };
                abs.parent().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
            },
            ..Default::default()
        };
        if let (Some((user, pass)), Some(origin)) = (creds, url_origin(url)) {
            let basic = base64_encode(format!("{user}:{pass}").as_bytes());
            auth.env = vec![
                ("GIT_CONFIG_COUNT".into(), "1".into()),
                ("GIT_CONFIG_KEY_0".into(), format!("http.{origin}/.extraHeader")),
                ("GIT_CONFIG_VALUE_0".into(), format!("Authorization: Basic {basic}")),
            ];
            auth.secrets = vec![pass, basic];
            auth.secrets.retain(|s| !s.is_empty());
        }
        auth
    }

    fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in &self.secrets {
            out = out.replace(s.as_str(), "***");
        }
        out
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(cwd).envs(self.env.iter().map(|(k, v)| (k, v)));
        // Never fall back to an interactive credential prompt.
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        if let Some(c) = &self.ceiling {
            cmd.env("GIT_CEILING_DIRECTORIES", c);
        }
        cmd
    }

    fn git(&self, args: &[&str], cwd: &Path) -> Result<String> {
        let out = self
            .command(cwd)
            .args(args)
            .output()
            .with_context(|| self.redact(&format!("running git {}", args.first().unwrap_or(&""))))?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                self.redact(String::from_utf8_lossy(&out.stderr).trim())
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// `scheme://host[:port]` of a URL, without credentials.
fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    (!host.is_empty() && matches!(scheme, "http" | "https")).then(|| format!("{scheme}://{host}"))
}

/// Split `scheme://user:pass@host/...` into (url without userinfo, userinfo).
fn split_userinfo(url: &str) -> (String, Option<String>) {
    if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if let Some((ui, host)) = authority.rsplit_once('@') {
            return (format!("{scheme}://{host}{path}"), (!ui.is_empty()).then(|| ui.to_string()));
        }
    }
    (url.to_string(), None)
}

fn clean_url(url: &str) -> String {
    split_userinfo(url).0
}

fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn default_branch(auth: &GitAuth, dir: &Path) -> String {
    if let Ok(r) = auth.git(&["symbolic-ref", "refs/remotes/origin/HEAD"], dir) {
        return r.replace("refs/remotes/origin/", "");
    }
    if auth.git(&["rev-parse", "--verify", "origin/main"], dir).is_ok() {
        "main".into()
    } else {
        "master".into()
    }
}

fn short_hex() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:08x}", (nanos as u32) ^ (std::process::id()))
}

pub fn repo_name(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("agent")
        .trim_end_matches(".git")
        .to_string()
}

/// True when `dir` is itself the top level of a git working tree (not merely
/// somewhere inside an enclosing repository).
fn is_repo_root(auth: &GitAuth, dir: &Path) -> bool {
    let Ok(top) = auth.git(&["rev-parse", "--show-toplevel"], dir) else { return false };
    match (std::fs::canonicalize(&top), std::fs::canonicalize(dir)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn init_local_session(opts: RepoOptions) -> Result<LocalSession> {
    let dir = opts.dir;
    let auth = GitAuth::new(&opts.url, &opts.token, &dir);
    let url = clean_url(&opts.url);

    let empty_dir = dir.is_dir() && std::fs::read_dir(&dir).map(|mut r| r.next().is_none()).unwrap_or(false);
    if !dir.exists() || empty_dir {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // argv — the URL is never shell-interpreted; `--` ends option parsing.
        let out = auth
            .command(Path::new("."))
            .args(["clone", "--depth", "1", "--no-single-branch", "--", &url])
            .arg(&dir)
            .output()
            .context("git clone")?;
        if !out.status.success() {
            bail!("git clone failed: {}", auth.redact(String::from_utf8_lossy(&out.stderr).trim()));
        }
    } else {
        // Reuse only a working copy whose top level IS this directory — never
        // an enclosing repository git would otherwise walk up to.
        if !is_repo_root(&auth, &dir) {
            bail!(
                "{} exists but is not the root of a git clone; refusing to reuse it (choose another --dir)",
                dir.display()
            );
        }
        // Also scrubs any credentials an older version left in the remote URL.
        auth.git(&["remote", "set-url", "origin", &url], &dir)?;
        auth.git(&["fetch", "origin"], &dir).ok();
        let default = default_branch(&auth, &dir);
        auth.git(&["checkout", &default], &dir).ok();
        auth.git(&["reset", "--hard", &format!("origin/{default}")], &dir).ok();
    }

    let (branch, session_id) = match opts.session {
        Some(s) => {
            let id = s.strip_prefix("gitagent/session-").unwrap_or(&s).to_string();
            if auth.git(&["checkout", &s], &dir).is_err() {
                auth.git(&["checkout", "-b", &s, &format!("origin/{s}")], &dir).ok();
            }
            auth.git(&["pull", "origin", &s], &dir).ok();
            (s, id)
        }
        None => {
            let id = short_hex();
            let branch = format!("gitagent/session-{id}");
            auth.git(&["checkout", "-b", &branch], &dir)?;
            (branch, id)
        }
    };

    scaffold(&dir, &url);

    Ok(LocalSession { dir, branch, session_id, auth })
}

fn scaffold(dir: &Path, url: &str) {
    let agent_yaml = dir.join("agent.yaml");
    if !agent_yaml.exists() {
        let name = repo_name(url);
        let _ = std::fs::write(
            &agent_yaml,
            format!(
                "spec_version: \"0.1.0\"\nname: {name}\nversion: 0.1.0\ndescription: Gitagent agent for {name}\nmodel:\n  preferred: \"openai:gpt-4o-mini\"\n  fallback: []\ntools: [cli, read, write, memory]\nruntime:\n  max_turns: 50\n"
            ),
        );
    }
    let mem = dir.join("memory").join("MEMORY.md");
    if !mem.exists() {
        let _ = std::fs::create_dir_all(dir.join("memory"));
        let _ = std::fs::write(mem, "# Memory\n");
    }
}

impl LocalSession {
    /// Commit any changes and push the session branch. Best-effort (push may
    /// fail offline). Credentials only ever live in this process's memory.
    pub fn finalize(&self) -> Result<()> {
        let _ = self.auth.git(&["add", "-A"], &self.dir);
        // `git diff --cached --quiet` exits non-zero when there ARE staged changes.
        let has_staged = !self
            .auth
            .command(&self.dir)
            .args(["diff", "--cached", "--quiet"])
            .status()
            .map(|s| s.success())
            .unwrap_or(true);
        if has_staged {
            self.auth.git(&["commit", "-m", &format!("gitagent: session {}", self.branch)], &self.dir).ok();
        }
        self.auth.git(&["push", "origin", &self.branch], &self.dir).ok();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"x-access-token:abc"), "eC1hY2Nlc3MtdG9rZW46YWJj");
    }

    #[test]
    fn credentials_stay_out_of_urls_and_errors() {
        assert_eq!(clean_url("https://tok@github.com/o/r.git"), "https://github.com/o/r.git");
        assert_eq!(clean_url("https://u:p@github.com/o/r"), "https://github.com/o/r");
        assert_eq!(clean_url("https://github.com/o/r"), "https://github.com/o/r");
        assert_eq!(url_origin("https://u:p@github.com:8443/o/r").as_deref(), Some("https://github.com:8443"));
        assert_eq!(url_origin("file:///tmp/x"), None);

        let a = GitAuth::new("https://github.com/o/r", "sekret", Path::new("/tmp/x"));
        assert!(a.env.iter().all(|(_, v)| !v.contains("sekret")), "token is base64'd into the header only");
        assert!(a.env.iter().any(|(k, v)| k == "GIT_CONFIG_KEY_0" && v == "http.https://github.com/.extraHeader"));
        let msg = a.redact(&format!("fatal: auth failed for sekret / {}", base64_encode(b"x-access-token:sekret")));
        assert!(!msg.contains("sekret") && !msg.contains(&base64_encode(b"x-access-token:sekret")));

        // Credentials embedded in the URL are used, but moved out of it.
        let b = GitAuth::new("https://u:pw@github.com/o/r", "", Path::new("/tmp/x"));
        assert!(b.env.iter().any(|(_, v)| v == &format!("Authorization: Basic {}", base64_encode(b"u:pw"))));
        assert!(GitAuth::new("https://github.com/o/r", "", Path::new("/tmp/x")).env.is_empty());
    }
}
