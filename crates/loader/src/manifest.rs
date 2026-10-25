//! Agent directories in the git-native agent format: an `agent.yaml` manifest
//! next to `SOUL.md` (identity), `RULES.md` (hard constraints), `DUTIES.md`
//! (segregation of duties) and, as a fallback for agents without `SOUL.md`,
//! `AGENTS.md`.
//!
//! Loading is lenient, so existing agent repositories keep working: schema
//! problems become warnings here and errors only in `validate`. The identity
//! files are placed ahead of gitagent's own working instructions in the system
//! prompt, in the order header, SOUL, RULES, DUTIES.
//!
//! `extends: <path or git URL>` names a parent agent. The child's manifest
//! is merged over the parent's (mappings key by key, lists replaced, except
//! `skills` and `tools`, which are joined); SOUL.md is the child's unless it
//! has none; RULES.md is the parent's followed by the child's; DUTIES.md and
//! the other folders (hooks, knowledge, examples, config files) are the
//! child's when it has them, else the parent's; skills and tools are joined
//! with the child's shadowing the parent's by name; memory is never shared.
//! A git parent is cloned into `.gitagent/parent/` on first use. Parents can
//! have parents of their own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::ModelRef;

/// Top-level keys of the manifest schema, plus `plugins` (used by existing
/// agent repositories for plugin settings).
const KNOWN_KEYS: &[&str] = &[
    "spec_version",
    "name",
    "version",
    "description",
    "author",
    "license",
    "model",
    "extends",
    "dependencies",
    "skills",
    "tools",
    "agents",
    "delegation",
    "runtime",
    "a2a",
    "compliance",
    "registries",
    "tags",
    "mcp_servers",
    "metadata",
    "plugins",
];

/// `agent.yaml`. Sections that later features act on (compliance, MCP
/// servers, sub-agents, plugins, …) are kept as raw values.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub spec_version: Option<String>,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub license: Option<String>,
    pub model: Option<ModelSection>,
    pub extends: Option<String>,
    pub dependencies: Vec<Value>,
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub agents: BTreeMap<String, Value>,
    pub delegation: Option<Value>,
    pub runtime: Option<Runtime>,
    pub a2a: Option<Value>,
    pub compliance: Option<Value>,
    pub registries: Vec<Value>,
    pub tags: Vec<String>,
    pub mcp_servers: BTreeMap<String, Value>,
    pub metadata: BTreeMap<String, Value>,
    pub plugins: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModelSection {
    /// `provider:model`, `provider/model`, or a bare model id whose provider
    /// is recognised from its name.
    pub preferred: Option<String>,
    pub fallback: Vec<String>,
    pub constraints: Option<Constraints>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Constraints {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop_sequences: Vec<String>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Runtime {
    pub max_turns: Option<u32>,
    pub temperature: Option<f32>,
    /// Seconds.
    pub timeout: Option<u64>,
    /// The agent may plan work as phases of tasks that run at the same
    /// time as sub-agents, then reconcile the results (the `plan` tool).
    pub parallel_tasks: Option<bool>,
    /// How many tasks run at once.
    pub max_parallel: Option<u32>,
}

/// Most generations of `extends` followed.
const MAX_PARENTS: usize = 8;

/// An agent directory: the manifest and its identity files.
#[derive(Debug, Clone)]
pub struct AgentDir {
    pub root: PathBuf,
    /// The roots of the agents this one extends, nearest first.
    pub parents: Vec<PathBuf>,
    pub manifest: Manifest,
    pub soul: Option<String>,
    pub rules: Option<String>,
    pub duties: Option<String>,
    pub agents_md: Option<String>,
    /// Schema problems found while loading; `validate` reports them as errors.
    pub warnings: Vec<String>,
}

impl AgentDir {
    /// The directory's agent, if it has an `agent.yaml`.
    pub fn load(root: &Path) -> Result<Option<AgentDir>> {
        let Some((mut agent, mut raw)) = Self::load_own(root)? else {
            return Ok(None);
        };
        // Parents, nearest first, each merged under what is already there.
        let mut seen: Vec<PathBuf> = vec![agent.root.clone()];
        let mut extends = agent.manifest.extends.clone();
        let mut from = agent.root.clone();
        while let Some(source) = extends.take() {
            if seen.len() > MAX_PARENTS {
                agent.warnings.push(format!(
                    "extends: more than {MAX_PARENTS} generations of parents; the rest are ignored"
                ));
                break;
            }
            let parent_root = match resolve_parent(&from, &source) {
                Ok(p) => p,
                Err(why) => {
                    agent
                        .warnings
                        .push(format!("extends: {why}; running without the parent"));
                    break;
                }
            };
            let canonical = parent_root.canonicalize().unwrap_or(parent_root.clone());
            if seen.contains(&canonical) {
                agent.warnings.push(format!(
                    "extends: {} is already in the chain of parents; stopping there",
                    parent_root.display()
                ));
                break;
            }
            seen.push(canonical);
            let Some((parent, parent_raw)) = Self::load_own(&parent_root)? else {
                agent.warnings.push(format!(
                    "extends: {} has no agent.yaml; running without it",
                    parent_root.display()
                ));
                break;
            };
            for w in &parent.warnings {
                agent
                    .warnings
                    .push(format!("parent {}: {w}", parent.root.display()));
            }
            // The child wins; the parent fills in what the child left out.
            let mut merged = parent_raw;
            merge_manifests(&mut merged, raw);
            raw = merged;
            agent.manifest = match serde_json::from_value::<Manifest>(lenient(&raw)) {
                Ok(m) => m,
                Err(e) => {
                    agent.warnings.push(format!(
                        "extends: the merged agent.yaml cannot be read ({e}); the parent's settings are ignored"
                    ));
                    agent.manifest
                }
            };
            if agent.soul.is_none() {
                agent.soul = parent.soul;
            }
            if agent.agents_md.is_none() {
                agent.agents_md = parent.agents_md;
            }
            agent.rules = match (parent.rules, agent.rules.take()) {
                (Some(p), Some(c)) => Some(format!("{}\n\n{}", p.trim_end(), c.trim_start())),
                (p, c) => c.or(p),
            };
            if agent.duties.is_none() {
                agent.duties = parent.duties;
            }
            extends = parent.manifest.extends.clone();
            from = parent.root.clone();
            agent.parents.push(parent.root);
        }
        // SOUL.md is required somewhere in the chain.
        if agent.soul.is_none() && agent.parents.is_empty() {
            agent
                .warnings
                .push("SOUL.md is missing (it is required)".into());
        } else if agent.soul.is_none() {
            agent
                .warnings
                .push("SOUL.md is missing here and in the parents (it is required)".into());
        }
        dependencies_present(&agent.root, &agent.manifest, &mut agent.warnings);
        Ok(Some(agent))
    }

    /// This directory's own files, without its parents; the raw manifest
    /// comes back for merging.
    fn load_own(root: &Path) -> Result<Option<(AgentDir, serde_json::Map<String, Value>)>> {
        let path = root.join("agent.yaml");
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let raw: Value =
            serde_saphyr::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let Value::Object(map) = &raw else {
            bail!("{}: expected a mapping at the top level", path.display());
        };
        let mut warnings = check(map);
        let manifest = match serde_json::from_value::<Manifest>(lenient(map)) {
            Ok(m) => m,
            Err(e) => {
                // The agent still runs on its identity files; the other
                // settings wait until the file is fixed.
                warnings.push(format!(
                    "agent.yaml: {e}; only name, version and description are used until this is fixed"
                ));
                let text = |key: &str| match map.get(key) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                Manifest {
                    name: text("name"),
                    version: text("version"),
                    description: text("description"),
                    ..Manifest::default()
                }
            }
        };

        let file = |name: &str| -> Option<String> {
            std::fs::read_to_string(root.join(name))
                .ok()
                .filter(|t| !t.trim().is_empty())
        };
        let soul = file("SOUL.md");
        if let Some(text) = &soul {
            if !has_paragraph(text) {
                warnings.push("SOUL.md has headings but no text".into());
            }
        }
        let agent = AgentDir {
            // Scripts of hooks and tools run in other directories, so their
            // paths are built from an absolute root.
            root: std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()),
            parents: Vec::new(),
            manifest,
            soul,
            rules: file("RULES.md"),
            duties: file("DUTIES.md"),
            agents_md: file("AGENTS.md"),
            warnings,
        };
        Ok(Some((agent, map.clone())))
    }

    /// This root and its parents', nearest first.
    pub fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.root.clone())
            .chain(self.parents.iter().cloned())
            .collect()
    }

    /// The nearest root (this one, then the parents) that has `relative`.
    pub fn file_root(&self, relative: &str) -> Option<PathBuf> {
        self.roots().into_iter().find(|r| r.join(relative).exists())
    }

    /// The identity part of the system prompt: a `# name vX` header with the
    /// description, then SOUL, RULES and DUTIES as written. `AGENTS.md` stands
    /// in for SOUL only when there is no `SOUL.md` (it repeats SOUL and RULES
    /// in agents that have both).
    pub fn identity_prompt(&self) -> String {
        let m = &self.manifest;
        let name = if m.name.is_empty() {
            self.root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "agent".into())
        } else {
            m.name.clone()
        };
        let mut header = format!("# {name}");
        if !m.version.is_empty() {
            header.push_str(&format!(" v{}", m.version));
        }
        if !m.description.trim().is_empty() {
            header.push('\n');
            header.push_str(m.description.trim());
        }
        let identity = self.soul.as_ref().or(self.agents_md.as_ref());
        [
            Some(&header),
            identity,
            self.rules.as_ref(),
            self.duties.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n\n")
    }

    /// `model.preferred` as a model reference.
    pub fn preferred_model(&self) -> Option<Result<ModelRef>> {
        let preferred = self.manifest.model.as_ref()?.preferred.as_deref()?;
        Some(model_ref(preferred))
    }
}

/// A model id from an agent manifest. `provider:model` and `provider/model`
/// name the provider; a bare id is recognised by its prefix. Any of them may
/// end in `@https://…` for an OpenAI-compatible endpoint.
pub fn model_ref(id: &str) -> Result<ModelRef> {
    let (id, base_url) = crate::split_base_url(id.trim());
    let mut m = model_id(id)?;
    m.base_url = base_url;
    Ok(m)
}

fn model_id(id: &str) -> Result<ModelRef> {
    // `provider/model` (the model id may itself contain `:`, e.g. a tag),
    // then `provider:model`.
    let slash = id.find('/');
    if slash.is_some_and(|s| id.find(':').is_none_or(|c| s < c)) {
        if let Some((provider, model)) = id.split_once('/') {
            if !provider.is_empty() && !model.is_empty() {
                return Ok(ModelRef::new(provider, model));
            }
        }
    }
    if id.contains(':') {
        return ModelRef::parse(id);
    }
    let provider = if id.starts_with("claude") {
        "anthropic"
    } else if ["gpt", "o1", "o3", "o4"].iter().any(|p| id.starts_with(p)) {
        "openai"
    } else if id.starts_with("gemini") {
        "google"
    } else {
        bail!("model {id:?}: cannot tell its provider; write it as provider:model")
    };
    Ok(ModelRef::new(provider, id))
}

/// Schema checks on the raw manifest. Only messages; the caller decides
/// whether they are warnings or errors.
fn check(map: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    for key in map.keys() {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            out.push(format!("agent.yaml: unknown top-level key `{key}`"));
        }
    }
    let text = |key: &str| map.get(key).and_then(Value::as_str).unwrap_or("");
    for key in ["name", "version", "description"] {
        if text(key).trim().is_empty() {
            out.push(format!("agent.yaml: `{key}` is required"));
        }
    }
    let name = text("name");
    if !name.is_empty() && !is_kebab(name) {
        out.push(format!(
            "agent.yaml: `name` must be lowercase letters, digits and dashes, starting with a letter (got {name:?})"
        ));
    }
    let version = text("version");
    if !version.is_empty() && !is_semver(version) {
        out.push(format!(
            "agent.yaml: `version` must be a semantic version like 1.0.0 (got {version:?})"
        ));
    }
    if let Some(v) = map.get("spec_version").and_then(Value::as_str) {
        if !is_semver(v) {
            out.push(format!(
                "agent.yaml: `spec_version` must look like 0.1.0 (got {v:?})"
            ));
        }
    }
    out
}

/// `child` merged over `base`: mappings key by key, other values replaced,
/// except `skills` and `tools`, which are joined (the parent's first).
fn merge_manifests(
    base: &mut serde_json::Map<String, Value>,
    child: serde_json::Map<String, Value>,
) {
    let names = |v: Option<&Value>| -> Vec<Value> {
        match v {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::String(s)) => vec![Value::String(s.clone())],
            _ => Vec::new(),
        }
    };
    let mut joined = serde_json::Map::new();
    for key in ["skills", "tools"] {
        let mut all = names(base.get(key));
        for n in names(child.get(key)) {
            if !all.contains(&n) {
                all.push(n);
            }
        }
        if !all.is_empty() {
            joined.insert(key.to_string(), Value::Array(all));
        }
    }
    // The child's `extends` names the parent being merged, not a parent of
    // the result; the parent's own `extends` is followed next.
    let mut child = child;
    child.remove("extends");
    crate::env_config::merge(base, child);
    for (k, v) in joined {
        base.insert(k, v);
    }
}

/// Where the parent named by `extends` is: a path relative to `root`, an
/// absolute path, or a git URL cloned into `root/.gitagent/parent/`.
fn resolve_parent(root: &Path, source: &str) -> Result<PathBuf, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("`extends` is empty".into());
    }
    if is_git_source(source) {
        let dest = root.join(".gitagent/parent");
        if !dest.join("agent.yaml").is_file() {
            if dest.exists() {
                return Err(format!(
                    "{} exists but has no agent.yaml; remove it and run `gitagent install`",
                    dest.display()
                ));
            }
            git_clone(source, &dest, None)
                .map_err(|e| format!("cloning the parent {source}: {e}"))?;
        }
        return Ok(dest);
    }
    let path = if Path::new(source).is_absolute() {
        PathBuf::from(source)
    } else {
        root.join(source)
    };
    if path.join("agent.yaml").is_file() {
        Ok(path)
    } else {
        Err(format!("the parent {} has no agent.yaml", path.display()))
    }
}

/// A git URL or SSH address, as opposed to a path.
pub fn is_git_source(s: &str) -> bool {
    s.contains("://") || s.starts_with("git@") || (s.ends_with(".git") && !Path::new(s).exists())
}

/// A shallow clone of `url` (at `branch`, a branch or tag, when given).
pub fn git_clone(url: &str, dest: &Path, branch: Option<&str>) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet", "--depth", "1"]);
    if let Some(b) = branch {
        cmd.args(["--branch", b]);
    }
    cmd.arg(url).arg(dest);
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_dir_all(dest);
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// A dependency's mount point: `mount`, else `.gitagent/deps/<name>`.
pub fn dependency_mount(root: &Path, dep: &Value) -> Option<(String, PathBuf)> {
    let name = dep["name"].as_str()?.trim().to_string();
    let mount = dep["mount"]
        .as_str()
        .map(|m| root.join(m.trim()))
        .unwrap_or_else(|| root.join(".gitagent/deps").join(&name));
    Some((name, mount))
}

/// Warns about dependencies that are not installed.
fn dependencies_present(root: &Path, manifest: &Manifest, warnings: &mut Vec<String>) {
    for dep in &manifest.dependencies {
        let Some((name, mount)) = dependency_mount(root, dep) else {
            continue;
        };
        if !mount.join("agent.yaml").is_file() {
            warnings.push(format!(
                "the dependency {name:?} is not installed at {}; run `gitagent install`",
                mount.display()
            ));
        }
    }
}

/// The manifest with the slips YAML invites put right: `version: 1.0`
/// read as a number, and `skills: refunds` where a list was meant.
fn lenient(map: &serde_json::Map<String, Value>) -> Value {
    let mut map = map.clone();
    for key in ["version", "spec_version"] {
        if let Some(Value::Number(n)) = map.get(key) {
            map.insert(key.into(), Value::String(n.to_string()));
        }
    }
    for key in ["skills", "tools", "tags"] {
        if let Some(Value::String(s)) = map.get(key) {
            map.insert(key.into(), Value::Array(vec![Value::String(s.clone())]));
        }
    }
    Value::Object(map)
}

/// `^[a-z][a-z0-9-]*$`
fn is_kebab(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `MAJOR.MINOR.PATCH` with optional `-pre` and `+build` parts.
fn is_semver(s: &str) -> bool {
    let (core, _build) = s.split_once('+').unwrap_or((s, ""));
    let (core, _pre) = core.split_once('-').unwrap_or((core, ""));
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// At least one line that is not empty and not a heading.
fn has_paragraph(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        !l.is_empty() && !l.starts_with('#')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    const MANIFEST: &str = "spec_version: 0.1.0\nname: support-bot\nversion: 1.2.0\ndescription: Answers billing questions.\nmodel:\n  preferred: claude-opus-5-5\n  constraints:\n    temperature: 0.2\n    max_tokens: 4000\nruntime:\n  max_turns: 40\nskills: [refunds]\n";

    #[test]
    fn a_directory_without_a_manifest_is_not_an_agent() {
        let dir = agent(&[("SOUL.md", "I help.")]);
        assert!(AgentDir::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn identity_files_go_in_order_after_the_header() {
        let dir = agent(&[
            ("agent.yaml", MANIFEST),
            ("SOUL.md", "# Identity\nI am calm and precise.\n"),
            ("RULES.md", "- Never share card numbers.\n"),
            ("DUTIES.md", "Role: support.\n"),
            ("AGENTS.md", "Duplicate of soul and rules.\n"),
        ]);
        let a = AgentDir::load(dir.path()).unwrap().unwrap();
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        assert_eq!(
            a.identity_prompt(),
            "# support-bot v1.2.0\nAnswers billing questions.\n\n# Identity\nI am calm and precise.\n\n- Never share card numbers.\n\nRole: support."
        );
        let m = &a.manifest;
        assert_eq!(m.runtime.as_ref().unwrap().max_turns, Some(40));
        assert_eq!(m.skills, vec!["refunds"]);
        let c = m.model.as_ref().unwrap().constraints.as_ref().unwrap();
        assert_eq!((c.temperature, c.max_tokens), (Some(0.2), Some(4000)));
        assert_eq!(
            a.preferred_model().unwrap().unwrap().to_string(),
            "anthropic:claude-opus-5-5"
        );
    }

    #[test]
    fn agents_md_stands_in_only_without_a_soul() {
        let dir = agent(&[
            ("agent.yaml", MANIFEST),
            ("AGENTS.md", "Fallback instructions.\n"),
        ]);
        let a = AgentDir::load(dir.path()).unwrap().unwrap();
        assert!(a.identity_prompt().ends_with("\n\nFallback instructions."));
        assert!(a.warnings.iter().any(|w| w.contains("SOUL.md is missing")));
    }

    #[test]
    fn schema_problems_are_warnings_not_load_errors() {
        let dir = agent(&[
            (
                "agent.yaml",
                "name: Support_Bot\nversion: one\nplugins: {}\nserve: {port: 3000}\n",
            ),
            ("SOUL.md", "# Only a heading\n"),
        ]);
        let a = AgentDir::load(dir.path()).unwrap().unwrap();
        let w = a.warnings.join("\n");
        for expected in [
            "unknown top-level key `serve`",
            "`description` is required",
            "`name` must be lowercase",
            "`version` must be a semantic version",
            "SOUL.md has headings but no text",
        ] {
            assert!(w.contains(expected), "missing {expected:?} in:\n{w}");
        }
        assert!(!w.contains("`plugins`"), "{w}");
    }

    #[test]
    fn a_child_inherits_from_its_parents() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |name: &str, files: &[(&str, &str)]| {
            let d = dir.path().join(name);
            std::fs::create_dir_all(&d).unwrap();
            for (f, t) in files {
                std::fs::write(d.join(f), t).unwrap();
            }
            d
        };
        mk(
            "grand",
            &[
                (
                    "agent.yaml",
                    "name: grand\nversion: 1.0.0\ndescription: Grand.\nskills: [g]\n",
                ),
                ("SOUL.md", "Grand soul.\n"),
                ("RULES.md", "- grand rule\n"),
            ],
        );
        mk(
            "parent",
            &[
                (
                    "agent.yaml",
                    "name: base\nversion: 1.0.0\ndescription: Base.\nextends: ../grand\nmodel: {preferred: claude-sonnet-5-5, constraints: {max_tokens: 500}}\nskills: [a]\ntools: [t1]\ncompliance: {risk_tier: low}\n",
                ),
                ("SOUL.md", "Base soul.\n"),
                ("RULES.md", "- parent rule\n"),
                ("DUTIES.md", "Parent duties.\n"),
            ],
        );
        let child = mk(
            "child",
            &[
                (
                    "agent.yaml",
                    "name: kid\nversion: 2.0.0\ndescription: Kid.\nextends: ../parent\nmodel: {constraints: {max_tokens: 900}}\nskills: [b, a]\ntools: [t2]\ncompliance: {supervision: {kill_switch: true}}\n",
                ),
                ("RULES.md", "- child rule\n"),
            ],
        );
        let a = AgentDir::load(&child).unwrap().unwrap();
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        let m = &a.manifest;
        assert_eq!((m.name.as_str(), m.version.as_str()), ("kid", "2.0.0"));
        // Mappings merge; the child's values win.
        let model = m.model.as_ref().unwrap();
        assert_eq!(model.preferred.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(model.constraints.as_ref().unwrap().max_tokens, Some(900));
        assert_eq!(m.compliance.as_ref().unwrap()["risk_tier"], "low");
        assert_eq!(
            m.compliance.as_ref().unwrap()["supervision"]["kill_switch"],
            true
        );
        // Lists of skills and tools join, parents first.
        assert_eq!(m.skills, vec!["g", "a", "b"]);
        assert_eq!(m.tools, vec!["t1", "t2"]);
        // The identity: nearest SOUL, rules from the oldest down, nearest duties.
        assert_eq!(a.soul.as_deref(), Some("Base soul.\n"));
        assert_eq!(
            a.rules.as_deref(),
            Some("- grand rule\n\n- parent rule\n\n- child rule\n")
        );
        assert_eq!(a.duties.as_deref(), Some("Parent duties.\n"));
        assert_eq!(a.parents.len(), 2);
        assert!(a.parents[0].ends_with("parent") && a.parents[1].ends_with("grand"));
        assert_eq!(a.file_root("DUTIES.md").unwrap(), a.parents[0]);

        // A cycle stops with a warning; a missing parent too.
        std::fs::write(
            dir.path().join("grand/agent.yaml"),
            "name: grand\nversion: 1.0.0\ndescription: Grand.\nextends: ../child\n",
        )
        .unwrap();
        let a = AgentDir::load(&child).unwrap().unwrap();
        assert!(
            a.warnings
                .iter()
                .any(|w| w.contains("already in the chain")),
            "{:?}",
            a.warnings
        );
        std::fs::write(
            dir.path().join("child/agent.yaml"),
            "name: kid\nversion: 2.0.0\ndescription: Kid.\nextends: ../nope\n",
        )
        .unwrap();
        let a = AgentDir::load(&child).unwrap().unwrap();
        assert!(a
            .warnings
            .iter()
            .any(|w| w.contains("has no agent.yaml; running without the parent")));
        assert!(a.warnings.iter().any(|w| w.contains("SOUL.md is missing")));
    }

    #[test]
    fn yaml_slips_are_put_right_and_type_errors_keep_the_identity() {
        // `version: 1.0` is a number to YAML; `skills: refunds` is one name.
        let dir = agent(&[
            (
                "agent.yaml",
                "name: bot\nversion: 1.0\ndescription: d\nskills: refunds\nruntime: {timeout: 120}\n",
            ),
            ("SOUL.md", "I help.\n"),
        ]);
        let a = AgentDir::load(dir.path()).unwrap().unwrap();
        assert_eq!(a.manifest.version, "1.0");
        assert_eq!(a.manifest.skills, vec!["refunds"]);
        assert_eq!(a.manifest.runtime.as_ref().unwrap().timeout, Some(120));

        // A setting of the wrong shape: a warning, and the agent still has
        // its name and identity.
        let dir = agent(&[
            (
                "agent.yaml",
                "name: bot\nversion: 1.0.0\ndescription: d\nmodel: claude-opus-5-5\n",
            ),
            ("SOUL.md", "I help.\n"),
        ]);
        let a = AgentDir::load(dir.path()).unwrap().unwrap();
        assert_eq!(a.manifest.name, "bot");
        assert!(a.manifest.model.is_none());
        assert!(
            a.warnings
                .iter()
                .any(|w| w.contains("only name, version and description are used")),
            "{:?}",
            a.warnings
        );
        assert!(a.identity_prompt().contains("I help."));
    }

    #[test]
    fn model_ids_name_their_provider() {
        let cases = [
            ("anthropic:claude-opus-5-5", "anthropic:claude-opus-5-5"),
            ("openai/gpt-oss-120b", "openai:gpt-oss-120b"),
            ("ollama/qwen3-coder:latest", "ollama:qwen3-coder:latest"),
            ("gpt-6-astra", "openai:gpt-6-astra"),
            ("o4-mini", "openai:o4-mini"),
            ("gemini-3.5-flash", "google:gemini-3.5-flash"),
        ];
        for (id, want) in cases {
            assert_eq!(model_ref(id).unwrap().to_string(), want, "{id}");
        }
        assert!(model_ref("mystery-model").is_err());
        let m = model_ref("openai:my-model@http://localhost:8000/v1").unwrap();
        assert_eq!(m.to_string(), "openai:my-model");
        assert_eq!(m.base_url.as_deref(), Some("http://localhost:8000/v1"));
        assert_eq!(m.family(), crate::Family::Compat);
        let m = model_ref("llama-3@https://host.example/v1").unwrap_err();
        assert!(m.to_string().contains("cannot tell its provider"));
        assert_eq!(
            model_ref("ollama/qwen3@https://h/v1")
                .unwrap()
                .base_url
                .as_deref(),
            Some("https://h/v1")
        );
    }

    #[test]
    fn semver_and_kebab_checks() {
        for v in ["0.1.0", "1.2.3-beta.1", "1.0.0+build.5"] {
            assert!(is_semver(v), "{v}");
        }
        for v in ["1.0", "v1.0.0", "1.0.x"] {
            assert!(!is_semver(v), "{v}");
        }
        assert!(is_kebab("support-bot2"));
        assert!(!is_kebab("2bot") && !is_kebab("Bot") && !is_kebab("bot_x"));
    }
}
