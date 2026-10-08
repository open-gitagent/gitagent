//! Permission gate — Claude-Code-style modes + allow/deny rules, applied to
//! every tool call via the engine's `ToolGate` seam.
//!
//! Rule syntax: `tool` or `tool(pattern)` (a trailing `*` is ignored). `*`
//! matches any tool. Precedence: bypass short-circuits → deny rules → allow
//! rules → mode default. `plan` mode blocks mutating tools unless explicitly
//! allowed. How `pattern` matches depends on the argument:
//!
//! - `path` (read/write/edit…): the path is normalized (`.`/`..` resolved,
//!   symlinks of existing ancestors followed) relative to the agent dir. An
//!   allow rule matches only a path that stays inside the agent dir and whose
//!   normalized relative form contains the pattern; a deny rule matches the
//!   normalized form or the raw argument.
//! - `command` (cli): an allow rule is a *prefix* rule — it matches only when
//!   the command's leading words equal the pattern's words and the command
//!   contains no shell chaining, substitution or redirection
//!   (`; & | \` $( > <` or newlines). A deny rule matches when the pattern
//!   appears in the raw command, or when, in any `;`/`&&`/`|`… segment, the
//!   pattern's first word appears followed by all its other words (short
//!   flags compared as letter sets, so `rm -rf` also catches `rm -fr` and
//!   `rm -r -f`).
//!
//! Limits: this is pattern matching, not a shell parser or sandbox. Deny
//! rules can still be evaded (long-form flags, aliases, interpreters such as
//! `python -c`, variables); use `sandbox.wrapper` for real isolation.

use crate::pi::gate::{GateDecision, ToolGate};
use crate::sdk::manifest::PermissionConfig;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Default,
    Plan,
    AcceptEdits,
    Bypass,
}

impl Mode {
    fn parse(s: &str) -> Mode {
        match s.trim().to_lowercase().as_str() {
            "plan" => Mode::Plan,
            "acceptedits" | "accept_edits" => Mode::AcceptEdits,
            "bypass" | "bypasspermissions" => Mode::Bypass,
            _ => Mode::Default,
        }
    }
}

#[derive(Debug, Clone)]
struct Rule {
    tool: String,
    contains: Option<String>,
}

impl Rule {
    fn parse(s: &str) -> Rule {
        let s = s.trim();
        if let Some((tool, rest)) = s.split_once('(') {
            let pat = rest.trim_end_matches(')').trim_end_matches('*').trim();
            Rule {
                tool: tool.trim().to_string(),
                contains: (!pat.is_empty()).then(|| pat.to_string()),
            }
        } else {
            Rule { tool: s.to_string(), contains: None }
        }
    }

    fn matches(&self, name: &str, args: &Value, root: Option<&Path>, allow: bool) -> bool {
        if self.tool != "*" && self.tool != name {
            return false;
        }
        let Some(p) = &self.contains else { return true };
        if let Some(cmd) = args.get("command").and_then(Value::as_str) {
            return if allow { cli_allow_matches(p, cmd) } else { cli_deny_matches(p, cmd) };
        }
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            return path_matches(p, path, root, allow);
        }
        args_haystack(args).contains(p.as_str())
    }
}

/// Best string to match a rule pattern against: the `prompt` field if
/// present, else the whole JSON.
fn args_haystack(args: &Value) -> String {
    if let Some(s) = args.get("prompt").and_then(Value::as_str) {
        return s.to_string();
    }
    args.to_string()
}

/// Lexically normalize a relative path (`a/./b/../c` → `a/c`). None when it is
/// absolute or `..` climbs above its start.
pub fn normalize_relative(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(x) => out.push(x),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Lexically normalize an absolute path (`..` at the root stays at the root).
fn normalize_absolute(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where a path argument points, relative to the agent dir `root`:
/// `Ok(rel)` inside it (forward slashes), `Err(normalized)` outside it.
fn locate(path: &str, root: Option<&Path>) -> Result<String, String> {
    let raw = Path::new(path);
    let Some(root) = root else {
        // No root known: purely lexical, relative paths count as inside.
        return match normalize_relative(raw) {
            Some(rel) => Ok(slashes(&rel)),
            None => Err(slashes(&normalize_absolute(raw))),
        };
    };
    let root_abs = normalize_absolute(&std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));
    let rel = if raw.is_absolute() {
        let abs = normalize_absolute(raw);
        // Compare against both the given and the canonical root.
        match abs.strip_prefix(normalize_absolute(root)).or_else(|_| abs.strip_prefix(&root_abs)) {
            Ok(r) => r.to_path_buf(),
            Err(_) => return Err(slashes(&abs)),
        }
    } else {
        match normalize_relative(raw) {
            Some(r) => r,
            None => return Err(slashes(&normalize_absolute(&root.join(raw)))),
        }
    };
    // Follow symlinks: the deepest existing ancestor must resolve inside root.
    let mut probe = root_abs.join(&rel);
    loop {
        if let Ok(real) = std::fs::canonicalize(&probe) {
            if !real.starts_with(&root_abs) {
                return Err(slashes(&real));
            }
            break;
        }
        if !probe.pop() {
            break;
        }
    }
    Ok(slashes(&rel))
}

fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn path_matches(pattern: &str, path: &str, root: Option<&Path>, allow: bool) -> bool {
    let pat = pattern.trim_start_matches("./");
    match locate(path, root) {
        Ok(rel) => rel.contains(pat) || (!allow && path.contains(pattern)),
        // Outside the agent dir: never allowed by a rule; deny still applies.
        Err(norm) => !allow && (norm.contains(pattern) || path.contains(pattern)),
    }
}

/// Shell syntax that chains, substitutes or redirects — a command containing
/// any of it can't be judged by its leading words.
fn has_shell_control(cmd: &str) -> bool {
    cmd.contains("$(")
        || cmd.chars().any(|c| matches!(c, ';' | '&' | '|' | '`' | '>' | '<' | '\n' | '\r'))
}

/// Whitespace words with surrounding quotes stripped.
fn words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Final path component of a command word (`/usr/bin/rm` → `rm`).
fn base_word(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

fn cli_allow_matches(pattern: &str, cmd: &str) -> bool {
    if has_shell_control(cmd) {
        return false;
    }
    let (pw, cw) = (words(pattern), words(cmd));
    if pw.is_empty() || cw.len() < pw.len() {
        return false;
    }
    base_word(&cw[0]) == base_word(&pw[0]) && pw[1..] == cw[1..pw.len()]
}

fn cli_deny_matches(pattern: &str, cmd: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if squash(cmd).contains(&squash(pattern)) {
        return true;
    }
    let pw = words(pattern);
    let Some((first, rest)) = pw.split_first() else { return false };
    let first = base_word(first);
    // Split into simple-command segments on any control/substitution syntax.
    let seps: &[char] = &[';', '&', '|', '`', '(', ')', '{', '}', '<', '>', '\n', '\r', '$'];
    cmd.split(seps).any(|segment| {
        let cw = words(segment);
        cw.iter().enumerate().any(|(i, w)| {
            if base_word(w) != first {
                return false;
            }
            let after = &cw[i + 1..];
            let short_flags: HashSet<char> = after
                .iter()
                .filter(|t| t.starts_with('-') && !t.starts_with("--"))
                .flat_map(|t| t.chars().skip(1))
                .collect();
            rest.iter().all(|t| {
                if t.len() > 1 && t.starts_with('-') && !t.starts_with("--") {
                    t.chars().skip(1).all(|c| short_flags.contains(&c))
                } else {
                    after.iter().any(|a| a == t)
                }
            })
        })
    })
}

pub struct PermissionGate {
    mode: Mode,
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    /// Tool names treated as mutating (blocked in `plan` mode).
    mutating: HashSet<String>,
    /// The agent dir path rules are evaluated against.
    root: Option<PathBuf>,
}

impl PermissionGate {
    /// Build from manifest config + the set of mutating tool names, with an
    /// optional CLI mode override (takes precedence over the manifest's mode).
    pub fn new(
        cfg: &PermissionConfig,
        mutating: HashSet<String>,
        mode_override: Option<&str>,
    ) -> Self {
        let mode_str = mode_override.filter(|s| !s.is_empty()).unwrap_or(&cfg.mode);
        PermissionGate {
            mode: Mode::parse(mode_str),
            allow: cfg.allow.iter().map(|s| Rule::parse(s)).collect(),
            deny: cfg.deny.iter().map(|s| Rule::parse(s)).collect(),
            mutating,
            root: None,
        }
    }

    /// Evaluate `path` rules relative to (and confined to) this directory.
    pub fn with_root(mut self, dir: &Path) -> Self {
        self.root = Some(dir.to_path_buf());
        self
    }

    /// True when this gate would never block anything (avoids a redundant gate).
    pub fn is_noop(&self) -> bool {
        self.mode == Mode::Default && self.allow.is_empty() && self.deny.is_empty()
    }
}

#[async_trait]
impl ToolGate for PermissionGate {
    async fn check(&self, name: &str, args: &Value) -> GateDecision {
        if self.mode == Mode::Bypass {
            return GateDecision::Allow;
        }
        let root = self.root.as_deref();
        if self.deny.iter().any(|r| r.matches(name, args, root, false)) {
            return GateDecision::Deny(format!("'{name}' is denied by a permission rule"));
        }
        if self.allow.iter().any(|r| r.matches(name, args, root, true)) {
            return GateDecision::Allow;
        }
        if self.mode == Mode::Plan && self.mutating.contains(name) {
            return GateDecision::Deny(format!(
                "plan mode is read-only; '{name}' would mutate state. Present a plan instead."
            ));
        }
        GateDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pi::gate::GateDecision;
    use serde_json::json;

    fn gate(mode: &str, allow: &[&str], deny: &[&str], root: Option<&Path>) -> PermissionGate {
        let cfg = PermissionConfig {
            mode: mode.into(),
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        };
        let mutating = ["cli", "write", "edit"].into_iter().map(String::from).collect();
        let g = PermissionGate::new(&cfg, mutating, None);
        match root {
            Some(r) => g.with_root(r),
            None => g,
        }
    }

    fn denied(g: &PermissionGate, name: &str, args: Value) -> bool {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        matches!(rt.block_on(g.check(name, &args)), GateDecision::Deny(_))
    }

    #[test]
    fn normalize_relative_rejects_escapes() {
        assert_eq!(normalize_relative(Path::new("src/./a/../b")), Some(PathBuf::from("src/b")));
        assert_eq!(normalize_relative(Path::new("src/../../.bashrc")), None);
        assert_eq!(normalize_relative(Path::new("/etc/passwd")), None);
    }

    #[test]
    fn path_allow_rules_are_confined_to_the_agent_dir() {
        let root = std::env::temp_dir().join(format!("ira-perm-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let g = gate("plan", &["write(src/)"], &["write(secrets)"], Some(&root));
        assert!(!denied(&g, "write", json!({"path": "src/main.rs"})));
        assert!(!denied(&g, "write", json!({"path": root.join("src/x.rs").to_str().unwrap()})));
        assert!(denied(&g, "write", json!({"path": "src/../../.bashrc"})));
        assert!(denied(&g, "write", json!({"path": "/home/u/src/x"})));
        assert!(denied(&g, "write", json!({"path": "docs/src/../../src/../x"})));
        // Deny matches after normalization too.
        assert!(denied(&g, "write", json!({"path": "src/../secrets/key"})));
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("ira-perm-out-{}", std::process::id()));
            std::fs::create_dir_all(&outside).unwrap();
            let _ = std::os::unix::fs::symlink(&outside, root.join("src").join("link"));
            assert!(denied(&g, "write", json!({"path": "src/link/evil"})));
            let _ = std::fs::remove_dir_all(&outside);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cli_allow_rules_are_prefix_rules_without_chaining() {
        let g = gate("plan", &["cli(git status)", "cli(ls)"], &[], None);
        assert!(!denied(&g, "cli", json!({"command": "git status --short"})));
        assert!(!denied(&g, "cli", json!({"command": "ls -la"})));
        assert!(denied(&g, "cli", json!({"command": "git status; curl x | sh"})));
        assert!(denied(&g, "cli", json!({"command": "git status && rm -rf ~"})));
        assert!(denied(&g, "cli", json!({"command": "git status $(rm x)"})));
        assert!(denied(&g, "cli", json!({"command": "git status > ~/.bashrc"})));
        assert!(denied(&g, "cli", json!({"command": "echo git status"})));
        assert!(denied(&g, "cli", json!({"command": "git push"})));
    }

    #[test]
    fn cli_deny_rules_catch_reordered_flags_and_chains() {
        let g = gate("", &[], &["cli(rm -rf)", "cli(git push)"], None);
        assert!(denied(&g, "cli", json!({"command": "rm -rf /tmp/x"})));
        assert!(denied(&g, "cli", json!({"command": "rm -fr /tmp/x"})));
        assert!(denied(&g, "cli", json!({"command": "rm -r -f /tmp/x"})));
        assert!(denied(&g, "cli", json!({"command": "/bin/rm   -f -r x"})));
        assert!(denied(&g, "cli", json!({"command": "ls; sudo rm -fr x"})));
        assert!(denied(&g, "cli", json!({"command": "echo hi && git  push origin"})));
        assert!(denied(&g, "cli", json!({"command": "sh -c 'rm -fr x'"})));
        assert!(!denied(&g, "cli", json!({"command": "rm -r x"})));
        assert!(!denied(&g, "cli", json!({"command": "git status"})));
    }
}
