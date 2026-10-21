//! Agent directories in the git-native agent format: an `agent.yaml` manifest
//! next to `SOUL.md` (identity), `RULES.md` (hard constraints), `DUTIES.md`
//! (segregation of duties) and, as a fallback for agents without `SOUL.md`,
//! `AGENTS.md`.
//!
//! Loading is lenient, so existing agent repositories keep working: schema
//! problems become warnings here and errors only in `validate`. The identity
//! files are placed ahead of gitagent's own working instructions in the system
//! prompt, in the order header, SOUL, RULES, DUTIES.

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
}

/// An agent directory: the manifest and its identity files.
#[derive(Debug, Clone)]
pub struct AgentDir {
    pub root: PathBuf,
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
        let manifest: Manifest = serde_json::from_value(raw.clone())
            .with_context(|| format!("reading {}", path.display()))?;

        let file = |name: &str| -> Option<String> {
            std::fs::read_to_string(root.join(name))
                .ok()
                .filter(|t| !t.trim().is_empty())
        };
        let soul = file("SOUL.md");
        match &soul {
            None => warnings.push("SOUL.md is missing (it is required)".into()),
            Some(text) if !has_paragraph(text) => {
                warnings.push("SOUL.md has headings but no text".into())
            }
            Some(_) => {}
        }
        Ok(Some(AgentDir {
            root: root.to_path_buf(),
            manifest,
            soul,
            rules: file("RULES.md"),
            duties: file("DUTIES.md"),
            agents_md: file("AGENTS.md"),
            warnings,
        }))
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
/// name the provider; a bare id is recognised by its prefix.
pub fn model_ref(id: &str) -> Result<ModelRef> {
    let id = id.trim();
    if id.contains('@') {
        bail!("model {id:?}: a base URL after `@` is not supported yet; set {{PROVIDER}}_BASE_URL instead");
    }
    // `provider/model` (the model id may itself contain `:`, e.g. a tag),
    // then `provider:model`.
    let slash = id.find('/');
    if slash.is_some_and(|s| id.find(':').is_none_or(|c| s < c)) {
        if let Some((provider, model)) = id.split_once('/') {
            if !provider.is_empty() && !model.is_empty() {
                return Ok(ModelRef {
                    provider: provider.to_string(),
                    model: model.to_string(),
                });
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
    Ok(ModelRef {
        provider: provider.into(),
        model: id.into(),
    })
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
        assert!(model_ref("openai:gpt-6@http://localhost:8000").is_err());
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
