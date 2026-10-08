//! Load an agent directory into a system prompt + manifest — including the
//! identity surface gitagent injects: SOUL/RULES/DUTIES, skills, knowledge,
//! examples, and sub-agents.

use crate::sdk::manifest::AgentManifest;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub struct LoadedAgent {
    pub manifest: AgentManifest,
    pub system_prompt: String,
    pub dir: PathBuf,
}

pub fn load_agent(dir: &Path) -> Result<LoadedAgent> {
    let manifest_path = dir.join("agent.yaml");
    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: AgentManifest = serde_yaml::from_str(&raw).context("parsing agent.yaml")?;

    let mut parts: Vec<String> = vec![format!("You are {}.", manifest.name)];
    if !manifest.description.is_empty() {
        parts.push(manifest.description.clone());
    }

    // extends: inherit a parent agent's identity. The parent may be a git URL
    // (cloned into .gitagent/deps/) or a local path. Injected before our own so
    // the child's sections can refine what it inherits.
    if let Some(ext) = &manifest.extends {
        if let Some(parent) = resolve_parent_dir(dir, ext) {
            for (header, content) in identity_sections(&parent) {
                parts.push(format!("# Inherited {header}\n{content}"));
            }
        }
    }

    // dependencies: clone each agent package from git (best-effort, non-fatal).
    resolve_dependencies(dir, &manifest.dependencies);

    // Identity files.
    for (header, content) in identity_sections(dir) {
        parts.push(format!("# {header}\n{content}"));
    }

    // Skills — the agent reads skills/<name>/SKILL.md on demand.
    let skills = discover_skills(dir);
    if !skills.is_empty() {
        let mut s = String::from("# Skills\nWhen a task matches a skill, read `skills/<name>/SKILL.md` and follow it:\n");
        for (name, desc) in &skills {
            s.push_str(&format!("- {name}: {desc}\n"));
        }
        parts.push(s);
    }

    // Knowledge — always_load docs are injected; the rest are listed.
    let (preloaded, available) = load_knowledge(dir);
    for (path, content) in preloaded {
        parts.push(format!("# Knowledge: {path}\n{content}"));
    }
    if !available.is_empty() {
        let mut s = String::from("# Available knowledge (read with the `read` tool when relevant):\n");
        for p in available {
            s.push_str(&format!("- knowledge/{p}\n"));
        }
        parts.push(s);
    }

    // Examples.
    for (name, content) in load_examples(dir) {
        parts.push(format!("# Example: {name}\n{content}"));
    }

    // Sub-agents — delegate via the run_agent tool.
    let subs = discover_sub_agents(dir);
    if !subs.is_empty() {
        let mut s = String::from("# Sub-agents\nDelegate specialised tasks via the `run_agent` tool:\n");
        for name in &subs {
            s.push_str(&format!("- {name}\n"));
        }
        parts.push(s);
    }

    // Workflows — multi-step procedures the agent reads on demand.
    let workflows = discover_workflows(dir);
    if !workflows.is_empty() {
        let mut s = String::from("# Workflows\nFor a matching task, read `workflows/<name>.md` and follow its steps:\n");
        for (name, desc) in &workflows {
            s.push_str(&format!("- {name}: {desc}\n"));
        }
        parts.push(s);
    }

    // Compliance guardrails.
    if let Some(c) = &manifest.compliance {
        if !c.standards.is_empty() || !c.rules.is_empty() {
            let mut s = String::from("# Compliance\nYou must operate within these guardrails:\n");
            for st in &c.standards {
                s.push_str(&format!("- Standard: {st}\n"));
            }
            for r in &c.rules {
                s.push_str(&format!("- {r}\n"));
            }
            parts.push(s);
        }
    }

    // Dependencies — list the cloned agent packages.
    if !manifest.dependencies.is_empty() {
        let names: Vec<&str> = manifest.dependencies.iter().map(|d| d.name.as_str()).collect();
        parts.push(format!("# Dependencies\n{}", names.join(", ")));
    }

    Ok(LoadedAgent {
        manifest,
        system_prompt: parts.join("\n\n"),
        dir: dir.to_path_buf(),
    })
}

/// Whether `name` is safe to use as a single path component (skill / sub-agent
/// / dependency names): `[A-Za-z0-9._-]`, not `.`/`..`, not starting with `-`.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether an `extends`/`source` value is a git URL (vs. a local path).
fn is_git_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("ssh://")
        || s.starts_with("git@")
        || s.starts_with("file://")
        || s.ends_with(".git")
}

/// The last path segment of a repo URL, without a trailing `.git`.
fn repo_basename(url: &str) -> String {
    url.trim_end_matches('/').rsplit('/').next().unwrap_or("dep").trim_end_matches(".git").to_string()
}

/// Clone a git repo using argv (never a shell string) — the URL/branch come from
/// an untrusted agent.yaml, so shell interpolation here would be a load-time RCE
/// (`extends: "$(cmd)"`). Returns false on failure (continue-on-failure, like TS).
pub fn clone_git_repo(url: &str, dest: &Path, cwd: &Path, branch: Option<&str>) -> bool {
    use std::process::{Command, Stdio};
    let mut args: Vec<String> = vec!["clone".into(), "--depth".into(), "1".into()];
    if let Some(b) = branch {
        args.push("--branch".into());
        args.push(b.to_string());
    }
    args.push(url.to_string());
    args.push(dest.display().to_string());
    Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve `extends` to a directory: clone git URLs into `.gitagent/deps/<name>`
/// (reusing an existing clone), or resolve a local path. None if a clone fails.
fn resolve_parent_dir(dir: &Path, extends: &str) -> Option<PathBuf> {
    if is_git_url(extends) {
        let dest = dir.join(".gitagent").join("deps").join(repo_basename(extends));
        if dest.join("agent.yaml").exists() {
            return Some(dest); // already cloned
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        clone_git_repo(extends, &dest, dir, None).then_some(dest)
    } else if Path::new(extends).is_absolute() {
        Some(PathBuf::from(extends))
    } else {
        Some(dir.join(extends))
    }
}

/// Clone each dependency package into `.gitagent/deps/<name>` (best-effort).
fn resolve_dependencies(dir: &Path, deps: &[crate::sdk::manifest::Dependency]) {
    for dep in deps {
        if !is_git_url(&dep.source) {
            continue;
        }
        if !is_safe_name(&dep.name) {
            eprintln!("warning: skipping dependency with unsafe name {:?}", dep.name);
            continue;
        }
        let dest = dir.join(".gitagent").join("deps").join(&dep.name);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        clone_git_repo(&dep.source, &dest, dir, dep.version.as_deref());
    }
}

/// The identity markdown sections present in a directory, as (header, content).
fn identity_sections(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for file in ["SOUL.md", "RULES.md", "DUTIES.md", "AGENTS.md"] {
        if let Ok(content) = std::fs::read_to_string(dir.join(file)) {
            let t = content.trim();
            if !t.is_empty() {
                out.push((file.trim_end_matches(".md").to_string(), t.to_string()));
            }
        }
    }
    out
}

/// (name, description) for each workflows/<name>.md (description from frontmatter
/// or the first non-empty line).
pub fn discover_workflows(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("workflows")) {
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else { continue };
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("workflow").to_string();
            let desc = parse_frontmatter(&content)
                .map(|(_, d)| d)
                .filter(|d| !d.is_empty())
                .or_else(|| {
                    content
                        .lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with('#'))
                        .map(|l| l.to_string())
                })
                .unwrap_or_default();
            out.push((name, desc));
        }
    }
    out
}

/// Sub-agent directory names (agents/<name>/agent.yaml).
pub fn discover_sub_agents(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("agents")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("agent.yaml").exists() {
                if let Some(n) = e.file_name().to_str() {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// (name, description) for each skills/<name>/SKILL.md.
pub fn discover_skills(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("skills")) {
        for e in rd.flatten() {
            let skill_md = e.path().join("SKILL.md");
            if let Ok(content) = std::fs::read_to_string(&skill_md) {
                if let Some((name, desc)) = parse_frontmatter(&content) {
                    out.push((name, desc));
                }
            }
        }
    }
    out.sort();
    out
}

fn parse_frontmatter(content: &str) -> Option<(String, String)> {
    let s = content.trim_start();
    let after = s.strip_prefix("---")?;
    let end = after.find("\n---")?;
    let fm: serde_yaml::Value = serde_yaml::from_str(after[..end].trim()).ok()?;
    let name = fm.get("name")?.as_str()?.to_string();
    let desc = fm.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some((name, desc))
}

#[derive(Deserialize)]
struct KnowledgeIndex {
    #[serde(default)]
    entries: Vec<KnowledgeEntry>,
}

#[derive(Deserialize)]
struct KnowledgeEntry {
    path: String,
    #[serde(default)]
    always_load: bool,
}

/// Returns (preloaded [(path, content)], available [path]).
fn load_knowledge(dir: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let index_path = dir.join("knowledge").join("index.yaml");
    let Ok(raw) = std::fs::read_to_string(&index_path) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(index) = serde_yaml::from_str::<KnowledgeIndex>(&raw) else {
        return (Vec::new(), Vec::new());
    };
    let mut preloaded = Vec::new();
    let mut available = Vec::new();
    for e in index.entries {
        if e.always_load {
            if let Ok(content) = std::fs::read_to_string(dir.join("knowledge").join(&e.path)) {
                preloaded.push((e.path, content.trim().to_string()));
            }
        } else {
            available.push(e.path);
        }
    }
    (preloaded, available)
}

/// (name, content) for each examples/*.md.
fn load_examples(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("examples")) {
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("example").to_string();
                    out.push((name, content.trim().to_string()));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::is_safe_name;

    #[test]
    fn safe_names() {
        for ok in ["web-search", "a.b_c", "v2"] {
            assert!(is_safe_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "-x", "/abs", "a b", "x\0"] {
            assert!(!is_safe_name(bad), "{bad:?}");
        }
    }
}
