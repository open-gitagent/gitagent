//! Loads `gitagent.yaml`, the model profile and the prompts into an [`AgentSpec`].
//!
//! Every file has a built-in default compiled from the repository, so the
//! static binary runs with no files next to it. A config directory given with
//! `--config` overrides the defaults file by file.

pub mod agent_hooks;
mod agents;
pub mod compliance;
mod config;
pub mod declared;
pub mod env_config;
pub mod knowledge;
pub mod manifest;
pub mod mcp;
pub mod memory;
pub mod plugins;
mod profile;
pub mod sdk;
mod skills;
pub mod subagents;
pub mod v1;
pub mod validate;
pub mod workflows;

pub use agent_hooks::{HookEntry, HookEvent};
pub use agents::{AgentDef, GENERAL as GENERAL_AGENT};
pub use compliance::Supervision;
pub use config::{Config, RepoConfig};
pub use declared::DeclaredTool;
pub use env_config::EnvConfig;
pub use knowledge::Knowledge;
pub use manifest::AgentDir;
pub use mcp::McpServerCfg;
pub use memory::MemoryCfg;
pub use plugins::Plugin;
pub use profile::{
    AnthropicCfg, CacheCfg, ContextCfg, EditTool, Effort, GuardsCfg, Masking, OpenAiCfg, Profile,
    ShapeCfg, StopCfg, ThinkingBinding, ThinkingDisplay, ToolCalling, ToolNaming, ToolsCfg,
};
pub use skills::{split_frontmatter, Skill};
pub use workflows::Workflow;

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const DEFAULT_CONFIG: &str = include_str!("../../../gitagent.yaml");
const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../../../prompts/system.md");
const DEFAULT_CONVERSATION_PROMPT: &str = include_str!("../../../prompts/conversation.md");
const DEFAULT_LESSONS: &str = include_str!("../../../prompts/lessons.md");
const DEFAULT_PROFILES: &[(&str, &str)] = &[
    (
        "anthropic",
        include_str!("../../../profiles/anthropic.yaml"),
    ),
    ("openai", include_str!("../../../profiles/openai.yaml")),
    ("google", include_str!("../../../profiles/google.yaml")),
    ("compat", include_str!("../../../profiles/compat.yaml")),
];

/// Model provider family. Profiles are chosen per family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Anthropic,
    OpenAI,
    Google,
    Compat,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Anthropic => "anthropic",
            Family::OpenAI => "openai",
            Family::Google => "google",
            Family::Compat => "compat",
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `provider:model` reference, e.g. `openai:<model-id>`, optionally with
/// an OpenAI-compatible endpoint: `provider:model@https://host/v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
    /// When set, the model is served by this OpenAI-compatible endpoint
    /// whatever the provider name says.
    pub base_url: Option<String>,
}

impl ModelRef {
    pub fn new(provider: &str, model: &str) -> Self {
        ModelRef {
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: None,
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        let (s, base_url) = split_base_url(s);
        match s.split_once(':') {
            Some((p, m)) if !p.is_empty() && !m.is_empty() => Ok(ModelRef {
                base_url,
                ..ModelRef::new(p, m)
            }),
            _ => bail!("model must look like provider:model, got {s:?}"),
        }
    }

    pub fn family(&self) -> Family {
        if self.base_url.is_some() {
            return Family::Compat;
        }
        match self.provider.as_str() {
            "anthropic" => Family::Anthropic,
            "openai" => Family::OpenAI,
            "google" | "gemini" => Family::Google,
            _ => Family::Compat,
        }
    }
}

/// `id@https://…` into the id and the endpoint. An `@` not followed by an
/// http(s) URL is part of the id.
pub fn split_base_url(s: &str) -> (&str, Option<String>) {
    match s.find("@http://").or_else(|| s.find("@https://")) {
        Some(at) => (&s[..at], Some(s[at + 1..].to_string())),
        None => (s, None),
    }
}

impl fmt::Display for ModelRef {
    /// `provider:model`; the endpoint is left out (it may carry credentials).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
}

/// A model to switch to when the ones before it fail, with the profile of
/// its family.
#[derive(Debug, Clone)]
pub struct Fallback {
    pub model: ModelRef,
    pub profile: Profile,
}

/// Everything the agent needs from its configuration files.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub config: Config,
    pub model: ModelRef,
    pub profile: Profile,
    /// Name or path the profile was loaded from, for the record.
    pub profile_source: String,
    /// Byte-identical across tasks; nothing task-specific goes here.
    pub system_prompt: String,
    /// Where `gitagent.yaml` came from (a path, or `built-in`).
    pub config_source: String,
    /// Directories searched for `prompts/`, `profiles/`, `skills/` and
    /// `hooks/`, highest priority first. Empty means built-ins only.
    pub config_dirs: Vec<PathBuf>,
    /// Skills found in the config directories (when `skills: true`).
    pub skills: Vec<Skill>,
    /// Sub-agents the `subagent` tool can start (when the profile enables it).
    pub agents: Vec<AgentDef>,
    /// The project's agent definition (`agent.yaml` and its identity files),
    /// when the project directory has one. Never set in benchmark runs.
    pub agent: Option<AgentDir>,
    /// Models to switch to, in order, when the current one fails
    /// (`fallback_models` in gitagent.yaml, then the agent's `model.fallback`).
    pub fallbacks: Vec<Fallback>,
    /// The project's persistent memory, for agent directories or with
    /// `memory: true`. Never set in benchmark runs.
    pub memory: Option<MemoryCfg>,
    /// Tools the agent declares in `tools/*.yaml`. Never set in benchmark
    /// runs.
    pub declared_tools: Vec<DeclaredTool>,
    /// The agent's `hooks/hooks.yaml`. Never set in benchmark runs.
    pub agent_hooks: Vec<HookEntry>,
    /// MCP servers to connect. Never set in benchmark runs.
    pub mcp_servers: Vec<McpServerCfg>,
    /// Approvals and the kill switch, from the agent's `compliance`
    /// section; nothing is gated without one.
    pub supervision: Supervision,
    /// Problems found while loading that did not stop it (besides the
    /// agent directory's own, in `agent.warnings`).
    pub warnings: Vec<String>,
    /// Conversation mode (see [`LoadOptions::conversation`]).
    pub conversation: bool,
    /// The agent's `config/` settings for the environment. Never read in
    /// benchmark runs.
    pub env_config: EnvConfig,
    /// Keep an audit log (the agent's `compliance.recordkeeping` or the
    /// environment's `audit_logging`).
    pub audit_logging: bool,
    /// The agent's `knowledge/` documents. Never read in benchmark runs.
    pub knowledge: Knowledge,
    /// `hooks/bootstrap.md`: what the agent does at the start of a session
    /// (the last section of the system prompt).
    pub bootstrap: Option<String>,
    /// `hooks/teardown.md`: what the agent does as a session ends, run as
    /// one final turn (see `gitagent_core::teardown`).
    pub teardown: Option<String>,
    /// The agent's `workflows/`. Never read in benchmark runs.
    pub workflows: Vec<Workflow>,
    /// The plugins agent.yaml names, as loaded. Never read in benchmark runs.
    pub plugins: Vec<Plugin>,
}

impl AgentSpec {
    /// The first existing `relative` path across the config directories.
    pub fn find(&self, relative: &str) -> Option<PathBuf> {
        find_in(&self.config_dirs, relative)
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Path to a `gitagent.yaml`; its directory is searched first for
    /// `prompts/` and `profiles/`.
    pub config_path: Option<PathBuf>,
    /// Look for `gitagent.yaml` (or `.gitagent/gitagent.yaml`) here when no
    /// `config_path` is given. Benchmark runs leave this unset.
    pub project_dir: Option<PathBuf>,
    /// The user's home directory; `~/.gitagent/` is the lowest config layer.
    /// Benchmark runs leave this unset.
    pub home_dir: Option<PathBuf>,
    /// Overrides `model` from the config.
    pub model: Option<String>,
    /// Overrides `profile` from the config: `auto`, a profile name or a path to a YAML file.
    pub profile: Option<String>,
    /// Conversation mode: working instructions for talking with a person
    /// (`prompts/conversation.md`), and a plain reply ends each turn.
    pub conversation: bool,
    /// The environment whose `config/<env>.yaml` applies (`GITAGENT_ENV`
    /// when unset).
    pub env: Option<String>,
}

pub fn load(opts: &LoadOptions) -> Result<AgentSpec> {
    let project_layer = opts.project_dir.as_ref().map(|d| d.join(".gitagent"));
    let home_layer = opts.home_dir.as_ref().map(|d| d.join(".gitagent"));
    // `.claude/` layers: `.claude/agents`, `.claude/skills` and
    // `.claude/settings.json` (hooks) are read the same way, after ours.
    let claude_layers: Vec<PathBuf> = opts
        .project_dir
        .iter()
        .chain(opts.home_dir.iter())
        .map(|d| d.join(".claude"))
        .filter(|d| d.is_dir())
        .collect();
    let config_path = opts.config_path.clone().or_else(|| {
        let mut candidates = Vec::new();
        if let Some(d) = &opts.project_dir {
            candidates.push(d.join("gitagent.yaml"));
        }
        candidates.extend(project_layer.iter().map(|d| d.join("gitagent.yaml")));
        candidates.extend(home_layer.iter().map(|d| d.join("gitagent.yaml")));
        candidates.into_iter().find(|p| p.is_file())
    });

    let mut config_dirs: Vec<PathBuf> = Vec::new();
    let layers = config_path
        .as_ref()
        .map(|p| p.parent().unwrap_or(Path::new(".")).to_path_buf())
        .into_iter()
        .chain(project_layer.into_iter().filter(|d| d.is_dir()))
        .chain(home_layer.into_iter().filter(|d| d.is_dir()))
        .chain(claude_layers);
    for dir in layers {
        if !config_dirs.contains(&dir) {
            config_dirs.push(dir);
        }
    }

    let (config_text, config_source) = match &config_path {
        Some(path) => (read(path)?, path.display().to_string()),
        None => (DEFAULT_CONFIG.to_string(), "built-in".to_string()),
    };
    let mut config: Config = serde_saphyr::from_str(&config_text)
        .with_context(|| format!("parsing gitagent.yaml ({config_source})"))?;

    // An agent directory (`agent.yaml`) in the project: only outside
    // benchmark runs, which pass no project directory.
    let mut agent = match &opts.project_dir {
        Some(dir) => AgentDir::load(dir)?,
        None => None,
    };
    let (declared_tools, agent_hooks, plugins) = match agent.as_mut() {
        Some(a) => {
            let (mut tools, warnings) = declared::discover(a);
            a.warnings.extend(warnings);
            let (mut hooks, warnings) = agent_hooks::load(a);
            a.warnings.extend(warnings);
            // Plugins add their tools and hooks after the agent's own.
            let taken: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
            let loaded = plugins::load(a, opts.home_dir.as_deref(), &taken);
            a.warnings.extend(loaded.warnings.iter().cloned());
            tools.extend(loaded.tools.iter().cloned());
            hooks.extend(loaded.hooks.iter().cloned());
            (tools, hooks, loaded)
        }
        None => (Vec::new(), Vec::new(), plugins::Loaded::default()),
    };

    // The environment's settings (`config/`), only outside benchmark runs.
    let outside_bench = opts.project_dir.is_some() || opts.home_dir.is_some();
    let mut warnings = Vec::new();
    let env_config = match (&agent, outside_bench) {
        (Some(a), true) => {
            let from_var = std::env::var("GITAGENT_ENV").ok();
            let env = opts.env.as_deref().or(from_var.as_deref());
            let (cfg, problems) = env_config::load(&a.roots(), env);
            warnings.extend(problems);
            if cfg.compliance_mode {
                let report = validate::validate(&a.root, true);
                let errors: Vec<String> = report
                    .findings
                    .iter()
                    .filter(|f| f.severity == validate::Severity::Error)
                    .map(ToString::to_string)
                    .collect();
                if !errors.is_empty() {
                    bail!(
                        "compliance_mode is on and the agent does not pass `gitagent validate --compliance`:\n{}",
                        errors.join("\n")
                    );
                }
            }
            cfg
        }
        _ => EnvConfig::default(),
    };

    // --model, then the environment's model_override, then gitagent.yaml,
    // then the agent's `model.preferred`.
    let model_setting = opts
        .model
        .as_deref()
        .or(env_config.model_override.as_deref())
        .or(config.model.as_deref());
    let mut model = match model_setting {
        Some(s) => ModelRef::parse(s)?,
        None => match agent.as_ref().and_then(AgentDir::preferred_model) {
            Some(m) => m?,
            None => bail!(
                "no model given: pass --model provider:model, set `model` in gitagent.yaml, or set `model.preferred` in agent.yaml"
            ),
        },
    };
    // GITAGENT_MODEL_BASE_URL sends any model to an OpenAI-compatible
    // endpoint; never in benchmark runs (no project or home directory).
    if outside_bench && model.base_url.is_none() && model.provider != "scripted" {
        if let Ok(url) = std::env::var("GITAGENT_MODEL_BASE_URL") {
            if !url.trim().is_empty() {
                model.base_url = Some(url.trim().to_string());
            }
        }
    }

    let profile_sel = opts.profile.as_deref().unwrap_or(&config.profile);
    let (mut profile, profile_source) = load_profile(profile_sel, model.family(), &config_dirs)?;

    if let Some(a) = &agent {
        let m = &a.manifest;
        if let Some(n) = m.runtime.as_ref().and_then(|r| r.max_turns) {
            config.max_turns = n;
        }
        if let Some(t) = m.runtime.as_ref().and_then(|r| r.timeout) {
            config.timeout_s = t;
        }
        if let Some(t) = env_config.timeout_override {
            config.timeout_s = t;
        }
        // Temperature: runtime, then model constraints.
        let constraints = m.model.as_ref().and_then(|s| s.constraints.as_ref());
        if let Some(t) = m
            .runtime
            .as_ref()
            .and_then(|r| r.temperature)
            .or(constraints.and_then(|c| c.temperature))
        {
            profile.temperature = Some(t);
        }
        if let Some(n) = constraints.and_then(|c| c.max_tokens) {
            profile.max_output_tokens = n;
        }
        if let Some(t) = env_config.temperature_override {
            profile.temperature = Some(t);
        }
    }

    let skills = if config.skills {
        // The agent's own `skills/` first; its `skills:` list, when given,
        // names the ones enabled.
        let mut dirs: Vec<PathBuf> = agent.iter().flat_map(AgentDir::roots).collect();
        dirs.extend(config_dirs.iter().cloned());
        let mut found = skills::discover(&dirs);
        if let Some(enabled) = agent.as_ref().map(|a| &a.manifest.skills) {
            if !enabled.is_empty() {
                found.retain(|s| enabled.contains(&s.name));
            }
        }
        // Plugin skills come after the agent's own, which win by name.
        for s in &plugins.skills {
            if !found.iter().any(|f| f.name == s.name) {
                found.push(s.clone());
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    } else {
        Vec::new()
    };
    let mut fallbacks = Vec::new();
    let manifest_fallbacks = agent
        .iter()
        .filter_map(|a| a.manifest.model.as_ref())
        .flat_map(|m| m.fallback.iter());
    // A scripted model is a test run: no real model takes over from it.
    let fallback_ids = config
        .fallback_models
        .iter()
        .chain(manifest_fallbacks)
        .filter(|_| model.provider != "scripted");
    for id in fallback_ids {
        let fb = if id.contains(':') && !id.contains('/') {
            ModelRef::parse(id)?
        } else {
            manifest::model_ref(id)?
        };
        if fb == model || fallbacks.iter().any(|f: &Fallback| f.model == fb) {
            continue;
        }
        let (profile, _) = load_profile("auto", fb.family(), &config_dirs)?;
        fallbacks.push(Fallback { model: fb, profile });
    }

    let mut system_prompt = build_system_prompt(&config_dirs, opts.conversation)?;
    if let Some(a) = &agent {
        system_prompt = format!("{}\n\n{system_prompt}", a.identity_prompt());
    }
    if profile.lessons {
        let lessons = lessons(&config_dirs)?;
        if !lessons.is_empty() {
            system_prompt.push_str("\n# Lessons from earlier tasks\n\n");
            system_prompt.push_str(&lessons);
            system_prompt.push('\n');
        }
    }
    if !skills.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&skills::index(&skills));
    }
    // Sub-agents: the config directories' (when the profile allows them)
    // and the agent directory's own, which turn the tool on.
    let mut agents = if profile.tools.subagents {
        agents::discover(&config_dirs)
    } else {
        Vec::new()
    };
    if let Some(a) = &agent {
        let (own, problems) = subagents::discover(a);
        warnings.extend(problems);
        if !own.is_empty() {
            profile.tools.subagents = true;
            if agents.is_empty() {
                agents.push(agents::general());
            }
            let section = subagents::prompt_section(&own, a.manifest.delegation.as_ref());
            for def in own {
                agents.retain(|x| x.name != def.name);
                agents.push(def);
            }
            agents.sort_by(|x, y| x.name.cmp(&y.name));
            if !section.is_empty() {
                system_prompt.push('\n');
                system_prompt.push_str(&section);
            }
        }
    }
    // Workflows from the agent directory.
    let workflows = match &agent {
        Some(a) => {
            let root = a
                .file_root("workflows")
                .or_else(|| a.file_root("skillflows"))
                .unwrap_or_else(|| a.root.clone());
            let (ws, problems) = workflows::discover(&root);
            warnings.extend(problems);
            let section = workflows::prompt_section(&ws);
            if !section.is_empty() {
                system_prompt.push('\n');
                system_prompt.push_str(&section);
            }
            ws
        }
        None => Vec::new(),
    };
    // Knowledge and examples from the agent directory.
    let knowledge = match &agent {
        Some(a) => {
            let knowledge_root = a
                .file_root("knowledge/index.yaml")
                .unwrap_or_else(|| a.root.clone());
            let examples_root = a.file_root("examples").unwrap_or_else(|| a.root.clone());
            let (k, problems) = knowledge::load(&knowledge_root);
            warnings.extend(problems);
            for section in [
                k.prompt_section(),
                knowledge::examples_section(&examples_root),
            ] {
                if !section.is_empty() {
                    system_prompt.push('\n');
                    system_prompt.push_str(&section);
                }
            }
            k
        }
        None => Knowledge::default(),
    };
    for section in &plugins.prompts {
        system_prompt.push('\n');
        system_prompt.push_str(section);
    }
    // MCP servers: `.mcp.json`, then agent.yaml, then gitagent.yaml.
    let mcp_servers = match &opts.project_dir {
        Some(project) if outside_bench => {
            let mcp_json = mcp::read_mcp_json(&project.join(".mcp.json")).unwrap_or_else(|e| {
                warnings.push(format!(".mcp.json: {e}"));
                Default::default()
            });
            let none = Default::default();
            let from_agent = agent.as_ref().map_or(&none, |a| &a.manifest.mcp_servers);
            let base = agent
                .as_ref()
                .map_or(project.as_path(), |a| a.root.as_path());
            let (servers, problems) = mcp::servers(
                &[
                    (".mcp.json", &mcp_json),
                    ("agent.yaml", from_agent),
                    ("gitagent.yaml", &config.mcp_servers),
                ],
                base,
            );
            warnings.extend(problems);
            servers
        }
        _ => Vec::new(),
    };
    let section = mcp::prompt_section(&mcp_servers);
    if !section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&section);
    }
    let compliance = agent.as_ref().and_then(|a| a.manifest.compliance.as_ref());
    let (supervision, section) = match compliance {
        Some(c) => compliance::read(c),
        None => (Supervision::default(), String::new()),
    };
    let audit_logging = env_config.audit_logging
        || compliance
            .is_some_and(|c| c["recordkeeping"]["audit_logging"] == serde_json::Value::Bool(true));
    if !section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&section);
    }
    let memory = match &opts.project_dir {
        Some(project) if agent.is_some() || config.memory => {
            Some(MemoryCfg::for_project(project, config.memory_commit)?)
        }
        _ => None,
    };
    if let Some(m) = &memory {
        let section = m.prompt_section();
        if !section.is_empty() {
            system_prompt.push('\n');
            system_prompt.push_str(&section);
        }
    }
    // Lifecycle instructions in Markdown: bootstrap.md ends the prompt,
    // teardown.md is kept for the session's last turn.
    let lifecycle = |name: &str| -> Option<String> {
        let a = agent.as_ref()?;
        let root = a.file_root(&format!("hooks/{name}"))?;
        std::fs::read_to_string(root.join("hooks").join(name))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    let bootstrap = lifecycle("bootstrap.md");
    let teardown = lifecycle("teardown.md");
    if let Some(text) = &bootstrap {
        system_prompt.push_str(&format!(
            "\n# Session start\n\nAt the start of a session, before the first request, do the following (once; the conversation shows whether it is done):\n\n{text}\n"
        ));
    }

    Ok(AgentSpec {
        config,
        model,
        profile,
        profile_source,
        system_prompt,
        config_source,
        agents,
        config_dirs,
        skills,
        agent,
        fallbacks,
        memory,
        declared_tools,
        agent_hooks,
        mcp_servers,
        supervision,
        warnings,
        conversation: opts.conversation,
        env_config,
        audit_logging,
        knowledge,
        bootstrap,
        teardown,
        workflows,
        plugins: plugins.plugins,
    })
}

/// Only `gitagent.yaml` (explicit path, else `~/.gitagent/gitagent.yaml`,
/// else built-in), for settings needed before a working directory exists.
pub fn load_config(config_path: Option<&Path>, home_dir: Option<&Path>) -> Result<Config> {
    let path = config_path.map(Path::to_path_buf).or_else(|| {
        home_dir
            .map(|h| h.join(".gitagent/gitagent.yaml"))
            .filter(|p| p.is_file())
    });
    let text = match &path {
        Some(p) => read(p)?,
        None => DEFAULT_CONFIG.to_string(),
    };
    serde_saphyr::from_str(&text).context("parsing gitagent.yaml")
}

/// The built-in lessons, then each layer's `prompts/lessons.md` (lessons add
/// up rather than override; a file identical to one already taken is skipped).
fn lessons(dirs: &[PathBuf]) -> Result<String> {
    let mut parts = vec![strip_header_comment(DEFAULT_LESSONS).trim().to_string()];
    for dir in dirs {
        let path = dir.join("prompts/lessons.md");
        if path.is_file() {
            let text = strip_header_comment(&read(&path)?).trim().to_string();
            if !parts.contains(&text) {
                parts.push(text);
            }
        }
    }
    parts.retain(|p| !p.is_empty());
    Ok(parts.join("\n"))
}

fn find_in(dirs: &[PathBuf], relative: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(relative)).find(|p| p.exists())
}

fn load_profile(sel: &str, family: Family, dirs: &[PathBuf]) -> Result<(Profile, String)> {
    let name = if sel == "auto" { family.as_str() } else { sel };
    let (text, source) = if name.ends_with(".yaml") || name.ends_with(".yml") || name.contains('/')
    {
        (read(Path::new(name))?, name.to_string())
    } else if let Some(path) = find_in(dirs, &format!("profiles/{name}.yaml")) {
        (read(&path)?, path.display().to_string())
    } else if let Some((_, text)) = DEFAULT_PROFILES.iter().find(|(n, _)| *n == name) {
        (text.to_string(), format!("built-in:{name}"))
    } else {
        bail!("unknown profile {name:?}");
    };
    let profile: Profile =
        serde_saphyr::from_str(&text).with_context(|| format!("parsing profile {source}"))?;
    validate_profile(&profile).with_context(|| format!("profile {source}"))?;
    Ok((profile, source))
}

/// Combinations that would fail at the provider.
fn validate_profile(p: &Profile) -> Result<()> {
    if p.context.masking == Masking::Server && p.family != Family::Anthropic {
        bail!("context.masking: server needs an Anthropic model; use client or none");
    }
    if p.context.masking == Masking::Client
        && p.anthropic
            .as_ref()
            .is_some_and(|a| a.thinking_binding == ThinkingBinding::Error)
    {
        bail!(
            "context.masking: client edits earlier turns, which Claude rejects while \
             anthropic.thinking_binding is error; set it to drop_block, or use masking: server"
        );
    }
    if !matches!(p.cache.ttl.as_str(), "5m" | "1h") {
        bail!("cache.ttl must be 5m or 1h, got {:?}", p.cache.ttl);
    }
    Ok(())
}

/// `prompts/system.md`, followed by `prompts/rules.md` when present.
fn build_system_prompt(dirs: &[PathBuf], conversation: bool) -> Result<String> {
    let (file, default) = if conversation {
        ("prompts/conversation.md", DEFAULT_CONVERSATION_PROMPT)
    } else {
        ("prompts/system.md", DEFAULT_SYSTEM_PROMPT)
    };
    let system = match find_in(dirs, file) {
        Some(p) => read(&p)?,
        None => default.to_string(),
    };
    let mut out = strip_header_comment(&system).trim_end().to_string();
    if let Some(rules) = find_in(dirs, "prompts/rules.md") {
        out.push_str("\n\n");
        out.push_str(strip_header_comment(&read(&rules)?).trim_end());
    }
    out.push('\n');
    Ok(out)
}

/// Drops a leading `<!-- ... -->` block (notes for maintainers), so
/// it stays in the file but is never sent to the model.
fn strip_header_comment(text: &str) -> &str {
    let trimmed = text.trim_start();
    match trimmed
        .strip_prefix("<!--")
        .and_then(|rest| rest.find("-->").map(|end| &rest[end + 3..]))
    {
        Some(rest) => rest.trim_start_matches(['\n', '\r']),
        None => text,
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(model: &str) -> LoadOptions {
        LoadOptions {
            model: Some(model.into()),
            ..Default::default()
        }
    }

    #[test]
    fn built_in_defaults_load_for_every_family() {
        for (model, family) in [
            ("anthropic:m", Family::Anthropic),
            ("openai:m", Family::OpenAI),
            ("google:m", Family::Google),
            ("openrouter:org/model", Family::Compat),
        ] {
            let spec = load(&opts(model)).unwrap();
            assert_eq!(spec.profile.family, family);
            assert_eq!(spec.profile_source, format!("built-in:{family}"));
            assert_eq!(spec.config.max_turns, 200);
        }
    }

    #[test]
    fn anthropic_profile_sends_no_temperature_and_has_its_options() {
        let spec = load(&opts("anthropic:m")).unwrap();
        assert_eq!(spec.profile.temperature, None);
        let a = spec.profile.anthropic.expect("anthropic section");
        assert_eq!(a.thinking_display, ThinkingDisplay::Summarized);
        assert_eq!(a.thinking_binding, ThinkingBinding::Error);
        assert!(a.refusal_fallback);
        assert!(load(&opts("openai:m")).unwrap().profile.anthropic.is_none());
    }

    #[test]
    fn openai_profile_has_its_options_and_no_temperature() {
        let p = load(&opts("openai:m")).unwrap().profile;
        assert_eq!(p.temperature, None);
        let o = p.openai.expect("openai section");
        assert_eq!(o.reasoning_summary, "auto");
        assert_eq!(o.prompt_cache_key.as_deref(), Some("gitagent-v2"));
    }

    #[test]
    fn rejects_combinations_the_provider_would_fail() {
        let anthropic = DEFAULT_PROFILES[0].1;
        let check = |text: String| {
            let p: Profile = serde_saphyr::from_str(&text).unwrap();
            validate_profile(&p)
        };
        assert!(check(anthropic.to_string()).is_ok());
        let client = anthropic.replace("masking: none", "masking: client");
        let err = check(client.clone()).unwrap_err().to_string();
        assert!(err.contains("drop_block"), "{err}");
        assert!(
            check(client.replace("thinking_binding: error", "thinking_binding: drop_block"))
                .is_ok()
        );
        assert!(check(anthropic.replace("masking: none", "masking: server")).is_ok());
        let openai = DEFAULT_PROFILES[1]
            .1
            .replace("masking: none", "masking: server");
        assert!(check(openai).unwrap_err().to_string().contains("Anthropic"));
        assert!(check(anthropic.replace("ttl: 5m", "ttl: 10m")).is_err());
    }

    #[test]
    fn model_ref_splits_on_first_colon() {
        let m = ModelRef::parse("compat:host:port/model").unwrap();
        assert_eq!(m.provider, "compat");
        assert_eq!(m.model, "host:port/model");
        assert!(ModelRef::parse("no-provider").is_err());
        assert!(ModelRef::parse(":x").is_err());
    }

    #[test]
    fn missing_model_is_an_error() {
        let err = load(&LoadOptions::default()).unwrap_err();
        assert!(err.to_string().contains("no model given"));
    }

    /// The prompt without the learned-lessons section, which the committed
    /// built-in lessons file adds to every prompt.
    fn without_lessons(prompt: &str) -> &str {
        prompt
            .split("\n# Lessons from earlier tasks\n")
            .next()
            .unwrap()
    }

    #[test]
    fn config_dir_overrides_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("prompts")).unwrap();
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(
            root.join("gitagent.yaml"),
            "model: anthropic:x\nmax_turns: 7\nprofile: custom\n",
        )
        .unwrap();
        std::fs::write(root.join("prompts/system.md"), "SYSTEM\n\n").unwrap();
        std::fs::write(root.join("prompts/rules.md"), "RULES\n").unwrap();
        let custom = DEFAULT_PROFILES[0].1.replace("effort: high", "effort: low");
        std::fs::write(root.join("profiles/custom.yaml"), custom).unwrap();

        let spec = load(&LoadOptions {
            config_path: Some(root.join("gitagent.yaml")),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(spec.config.max_turns, 7);
        assert_eq!(spec.model.to_string(), "anthropic:x");
        assert_eq!(spec.profile.effort, Effort::Low);
        assert_eq!(without_lessons(&spec.system_prompt), "SYSTEM\n\nRULES\n");
    }

    #[test]
    fn discovers_project_and_home_config_layers() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let home_cfg = home.path().join(".gitagent");
        std::fs::create_dir_all(home_cfg.join("prompts")).unwrap();
        std::fs::write(
            home_cfg.join("gitagent.yaml"),
            "model: anthropic:home\nmax_turns: 5\n",
        )
        .unwrap();
        std::fs::write(home_cfg.join("prompts/rules.md"), "HOME RULES\n").unwrap();
        let opts = LoadOptions {
            project_dir: Some(project.path().to_path_buf()),
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        };
        // Only the home config exists.
        let spec = load(&opts).unwrap();
        assert_eq!(spec.model.model, "home");
        assert_eq!(spec.config.max_turns, 5);
        assert!(without_lessons(&spec.system_prompt).ends_with("HOME RULES\n"));

        // A project config wins, and still picks up home prompts it lacks.
        std::fs::write(project.path().join("gitagent.yaml"), "model: openai:proj\n").unwrap();
        let spec = load(&opts).unwrap();
        assert_eq!(spec.model.model, "proj");
        assert_eq!(spec.config.max_turns, 200);
        assert!(without_lessons(&spec.system_prompt).ends_with("HOME RULES\n"));
        assert_eq!(
            spec.config_dirs,
            vec![project.path().to_path_buf(), home_cfg.clone()]
        );
        assert_eq!(
            spec.find("prompts/rules.md"),
            Some(home_cfg.join("prompts/rules.md"))
        );

        // Without project or home directories (benchmark runs), only built-ins.
        let spec = load(&LoadOptions {
            model: Some("anthropic:x".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(spec.config_dirs.is_empty());
        assert_eq!(spec.config_source, "built-in");
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gitagent.yaml");
        std::fs::write(&path, "model: anthropic:x\nmax_turn: 7\n").unwrap();
        let err = load(&LoadOptions {
            config_path: Some(path),
            ..Default::default()
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("max_turn"), "{err:#}");
    }

    #[test]
    fn lessons_are_appended_when_present_and_enabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("prompts")).unwrap();
        std::fs::write(dir.path().join("gitagent.yaml"), "model: anthropic:x\n").unwrap();
        std::fs::write(
            dir.path().join("prompts/lessons.md"),
            "<!-- notes -->\n- Start servers so they outlive the command.\n",
        )
        .unwrap();
        let opts = LoadOptions {
            config_path: Some(dir.path().join("gitagent.yaml")),
            ..Default::default()
        };
        let spec = load(&opts).unwrap();
        // The project's lessons follow the built-in ones under one heading.
        assert!(spec.system_prompt.contains(
            "
# Lessons from earlier tasks

- "
        ));
        assert!(spec.system_prompt.ends_with(
            "- Start servers so they outlive the command.
"
        ));
        assert_eq!(
            spec.system_prompt
                .matches("# Lessons from earlier tasks")
                .count(),
            1
        );
        // With lessons off, none of it is added.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir2.path().join("profiles")).unwrap();
        std::fs::write(
            dir2.path().join("gitagent.yaml"),
            "model: anthropic:x
profile: quiet
",
        )
        .unwrap();
        std::fs::write(
            dir2.path().join("profiles/quiet.yaml"),
            include_str!("../../../profiles/anthropic.yaml")
                .replace("lessons: true", "lessons: false"),
        )
        .unwrap();
        let plain = load(&LoadOptions {
            config_path: Some(dir2.path().join("gitagent.yaml")),
            ..Default::default()
        })
        .unwrap();
        assert!(!plain.system_prompt.contains("Lessons from earlier tasks"));
    }

    #[test]
    fn claude_code_layers_are_read_after_ours() {
        let project = tempfile::tempdir().unwrap();
        let claude = project.path().join(".claude");
        std::fs::create_dir_all(claude.join("agents")).unwrap();
        std::fs::create_dir_all(claude.join("skills/pdf")).unwrap();
        std::fs::write(
            claude.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews.\ntools: Read, Bash\nmaxTurns: 5\n---\nReview.\n",
        )
        .unwrap();
        std::fs::write(
            claude.join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDF forms.\n---\nUse pypdf.\n",
        )
        .unwrap();
        // Our own layer still comes first for the same name.
        std::fs::create_dir_all(project.path().join(".gitagent/skills/pdf")).unwrap();
        std::fs::write(
            project.path().join(".gitagent/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: Ours wins.\n---\nx\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join(".gitagent/gitagent.yaml"),
            "model: anthropic:x\nprofile: anthropic\n",
        )
        .unwrap();
        let mut spec = load(&LoadOptions {
            project_dir: Some(project.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(spec.config_dirs.last().unwrap(), &claude);
        assert_eq!(spec.skills.len(), 1);
        assert_eq!(spec.skills[0].description, "Ours wins.");
        // Sub-agent files are read when the profile enables sub-agents.
        spec.profile.tools.subagents = true;
        let agents = agents::discover(&spec.config_dirs);
        let r = agents.iter().find(|a| a.name == "reviewer").unwrap();
        assert_eq!(r.max_turns, 5);
        assert_eq!(
            r.tools.as_deref(),
            Some(
                &[
                    "read_file".to_string(),
                    "image_read".to_string(),
                    "exec".to_string()
                ][..]
            )
        );
        // Benchmark runs have no project or home, so none of this applies.
        let bench = load(&LoadOptions {
            model: Some("anthropic:x".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(bench.config_dirs.is_empty());
    }

    #[test]
    fn lessons_from_every_layer_add_up() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        for (dir, lesson) in [
            (project.path(), "- From the project.\n"),
            (home.path(), "- From home.\n"),
        ] {
            let prompts = dir.join(".gitagent/prompts");
            std::fs::create_dir_all(&prompts).unwrap();
            std::fs::write(prompts.join("lessons.md"), format!("<!-- h -->\n{lesson}")).unwrap();
        }
        let spec = load(&LoadOptions {
            model: Some("anthropic:x".into()),
            project_dir: Some(project.path().to_path_buf()),
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        assert!(spec.system_prompt.ends_with(
            "- From the project.
- From home.
"
        ));
    }

    #[test]
    fn header_comments_are_not_sent_to_the_model() {
        let prompt = load(&opts("anthropic:a")).unwrap().system_prompt;
        assert!(prompt.starts_with("# Role and environment"), "{prompt}");
        assert!(!prompt.contains("<!--"));
        assert_eq!(strip_header_comment("<!-- x -->\n\nBody\n"), "Body\n");
        assert_eq!(
            strip_header_comment("Body <!-- kept -->"),
            "Body <!-- kept -->"
        );
    }

    #[test]
    fn system_prompt_does_not_depend_on_the_model() {
        let a = load(&opts("anthropic:a")).unwrap().system_prompt;
        let b = load(&opts("openai:b")).unwrap().system_prompt;
        assert_eq!(a, b);
    }

    fn agent_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("agent.yaml"),
            "name: support-bot\nversion: 1.0.0\ndescription: Answers billing questions.\nmodel:\n  preferred: gpt-6-astra\n  constraints: {max_tokens: 9000}\nruntime: {max_turns: 30, temperature: 0.3}\nskills: [refunds]\n",
        )
        .unwrap();
        std::fs::write(root.join("SOUL.md"), "I am calm and precise.\n").unwrap();
        std::fs::write(root.join("RULES.md"), "- Never share card numbers.\n").unwrap();
        for (name, desc) in [("refunds", "Issue refunds"), ("invoices", "Fix invoices")] {
            let skill = root.join("skills").join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {desc}\n---\nSteps.\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn a_parents_skills_tools_hooks_and_knowledge_are_inherited_and_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = dir.path().join("child");
        let write = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        write(
            parent.join("agent.yaml"),
            "name: base\nversion: 1.0.0\ndescription: Base.\n",
        );
        write(parent.join("SOUL.md"), "Base soul.\n");
        write(
            parent.join("skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: From the parent.\n---\nParent steps.\n",
        );
        write(
            parent.join("skills/only-parent/SKILL.md"),
            "---\nname: only-parent\ndescription: Only the parent has it.\n---\nx\n",
        );
        write(parent.join("tools/count.sh"), "echo 1\n");
        write(
            parent.join("tools/count.yaml"),
            "name: count\ndescription: Parent counts.\nimplementation: {script: count.sh}\n",
        );
        write(
            parent.join("tools/ping.yaml"),
            "name: ping\ndescription: Parent pings.\nimplementation: {script: count.sh}\n",
        );
        write(parent.join("hooks/log.sh"), "true\n");
        write(
            parent.join("hooks/hooks.yaml"),
            "hooks:\n  pre_tool_use: [{script: log.sh, description: Parent log}]\n",
        );
        write(parent.join("knowledge/policy.md"), "Parent policy.\n");
        write(
            parent.join("knowledge/index.yaml"),
            "documents: [{path: policy.md, always_load: true}]\n",
        );
        write(
            parent.join("config/default.yaml"),
            "env_vars: {REGION: eu}\n",
        );
        write(
            child.join("agent.yaml"),
            "name: kid\nversion: 1.0.0\ndescription: Kid.\nextends: ../parent\n",
        );
        write(
            child.join("skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: From the child.\n---\nChild steps.\n",
        );
        write(child.join("tools/count.sh"), "echo 2\n");
        write(
            child.join("tools/count.yaml"),
            "name: count\ndescription: Child counts.\nimplementation: {script: count.sh}\n",
        );
        write(child.join("config/prod.yaml"), "env_vars: {STAGE: prod}\n");
        let spec = load(&LoadOptions {
            project_dir: Some(child.clone()),
            model: Some("scripted:s.yaml".into()),
            env: Some("prod".into()),
            ..Default::default()
        })
        .unwrap();
        let shared = spec.skills.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.description, "From the child.");
        assert!(spec.skills.iter().any(|s| s.name == "only-parent"));
        let tools: Vec<(&str, &str)> = spec
            .declared_tools
            .iter()
            .map(|t| (t.name.as_str(), t.description.as_str()))
            .collect();
        assert_eq!(
            tools,
            vec![("count", "Child counts."), ("ping", "Parent pings.")]
        );
        assert_eq!(spec.agent_hooks[0].description, "Parent log");
        assert!(spec.system_prompt.contains("Parent policy."));
        assert_eq!(spec.env_config.env_vars["REGION"], "eu");
        assert_eq!(spec.env_config.env_vars["STAGE"], "prod");
        // Memory stays the child's own.
        assert_eq!(spec.memory.as_ref().unwrap().dir, child.join("memory"));
    }

    #[test]
    fn knowledge_and_examples_follow_the_skills_in_the_prompt() {
        let dir = agent_project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("knowledge")).unwrap();
        std::fs::write(
            root.join("knowledge/policy.md"),
            "Refunds within 30 days.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("knowledge/index.yaml"),
            "documents:\n  - {path: policy.md, always_load: true}\n  - {path: missing.md}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("examples")).unwrap();
        std::fs::write(root.join("examples/good-outputs.md"), "Short and exact.\n").unwrap();
        let spec = load(&LoadOptions {
            project_dir: Some(root.to_path_buf()),
            model: Some("scripted:s.yaml".into()),
            ..Default::default()
        })
        .unwrap();
        let p = &spec.system_prompt;
        let at = |s: &str| {
            p.find(s)
                .unwrap_or_else(|| panic!("{s:?} missing in:\n{p}"))
        };
        assert!(at("# Skills") < at("# Knowledge"));
        assert!(at("# Knowledge") < at("# Examples"));
        assert!(!p.contains("# Memory") || at("# Examples") < at("# Memory"));
        assert!(p.contains("## policy.md\n\nRefunds within 30 days."));
        assert!(p.contains("## good-outputs\n\nShort and exact."));
        assert_eq!(spec.knowledge.documents.len(), 1);
        assert!(spec
            .warnings
            .iter()
            .any(|w| w.contains("knowledge/missing.md does not exist")));
    }

    #[test]
    fn the_environments_settings_apply_in_order() {
        let dir = agent_project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(
            root.join("config/default.yaml"),
            "model_override: anthropic:claude-haiku-5-5\ntemperature_override: 0.9\n",
        )
        .unwrap();
        std::fs::write(
            root.join("config/prod.yaml"),
            "model_override: anthropic:claude-opus-5-5\ntimeout_override: 90\nenv_vars: {REGION: eu}\n",
        )
        .unwrap();
        let load_env = |env: Option<&str>, model: Option<&str>| {
            load(&LoadOptions {
                project_dir: Some(root.to_path_buf()),
                env: env.map(str::to_string),
                model: model.map(str::to_string),
                ..Default::default()
            })
            .unwrap()
        };
        // The defaults alone: model_override beats the agent's preferred
        // model, temperature_override beats runtime.temperature.
        let spec = load_env(None, None);
        assert_eq!(spec.model.to_string(), "anthropic:claude-haiku-5-5");
        assert_eq!(spec.profile.temperature, Some(0.9));
        assert_eq!(spec.env_config.name, None);
        // An environment on top; its env_vars reach the spec.
        let spec = load_env(Some("prod"), None);
        assert_eq!(spec.model.to_string(), "anthropic:claude-opus-5-5");
        assert_eq!(spec.config.timeout_s, 90);
        assert_eq!(spec.env_config.env_vars["REGION"], "eu");
        assert_eq!(spec.env_config.name.as_deref(), Some("prod"));
        // --model still wins.
        let spec = load_env(Some("prod"), Some("scripted:s.yaml"));
        assert_eq!(spec.model.to_string(), "scripted:s.yaml");
        // Benchmark runs read none of it.
        let bench = load(&LoadOptions {
            model: Some("scripted:s.yaml".into()),
            env: Some("prod".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(bench.env_config, EnvConfig::default());

        // compliance_mode: the agent must pass validate --compliance (this
        // one is high risk with no supervision).
        std::fs::write(root.join("config/strict.yaml"), "compliance_mode: true\n").unwrap();
        let mut manifest = std::fs::read_to_string(root.join("agent.yaml")).unwrap();
        manifest.push_str("compliance: {risk_tier: high}\n");
        std::fs::write(root.join("agent.yaml"), manifest).unwrap();
        let err = load(&LoadOptions {
            project_dir: Some(root.to_path_buf()),
            env: Some("strict".into()),
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("compliance_mode is on") && err.contains("[FINRA 3110]"),
            "{err}"
        );
    }

    #[test]
    fn the_agents_runtime_timeout_applies() {
        let dir = agent_project();
        std::fs::write(
            dir.path().join("agent.yaml"),
            "name: bot\nversion: 1.0.0\ndescription: d\nruntime: {timeout: 120}\n",
        )
        .unwrap();
        let spec = load(&LoadOptions {
            project_dir: Some(dir.path().to_path_buf()),
            model: Some("scripted:s.yaml".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(spec.config.timeout_s, 120);
    }

    #[test]
    fn an_agent_directory_shapes_the_prompt_model_and_limits() {
        let dir = agent_project();
        let spec = load(&LoadOptions {
            project_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        let identity = "# support-bot v1.0.0\nAnswers billing questions.\n\nI am calm and precise.\n\n- Never share card numbers.\n\n";
        assert!(
            spec.system_prompt.starts_with(identity),
            "{}",
            spec.system_prompt
        );
        // gitagent's own working instructions follow the identity.
        let built_in = load(&opts("anthropic:a")).unwrap().system_prompt;
        let first_line = built_in.lines().next().unwrap();
        assert!(spec.system_prompt[identity.len()..].starts_with(first_line));

        assert_eq!(spec.model.to_string(), "openai:gpt-6-astra");
        assert_eq!(spec.config.max_turns, 30);
        assert_eq!(spec.profile.temperature, Some(0.3));
        assert_eq!(spec.profile.max_output_tokens, 9000);
        let names: Vec<&str> = spec.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["refunds"]);
        assert!(spec.agent.is_some());
    }

    #[test]
    fn an_explicit_model_wins_over_the_agents_preferred_one() {
        let dir = agent_project();
        let spec = load(&LoadOptions {
            project_dir: Some(dir.path().to_path_buf()),
            model: Some("anthropic:m".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(spec.model.to_string(), "anthropic:m");
    }

    #[test]
    fn fallbacks_come_from_config_then_manifest_with_their_own_profiles() {
        let dir = agent_project();
        std::fs::write(
            dir.path().join("agent.yaml"),
            "name: bot\nversion: 1.0.0\ndescription: d\nmodel:\n  preferred: anthropic:claude-opus-5-5\n  fallback: [gpt-6-astra, anthropic:claude-opus-5-5, ollama/qwen3:8b@http://localhost:11434/v1]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("gitagent.yaml"),
            "fallback_models: [\"google:gemini-3.5-flash\", \"openai:gpt-6-astra\"]\n",
        )
        .unwrap();
        let spec = load(&LoadOptions {
            project_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        let chain: Vec<(String, Family)> = spec
            .fallbacks
            .iter()
            .map(|f| (f.model.to_string(), f.profile.family))
            .collect();
        // The primary and duplicates are skipped; each keeps its family's profile.
        assert_eq!(
            chain,
            vec![
                ("google:gemini-3.5-flash".to_string(), Family::Google),
                ("openai:gpt-6-astra".to_string(), Family::OpenAI),
                ("ollama:qwen3:8b".to_string(), Family::Compat),
            ]
        );
        assert_eq!(
            spec.fallbacks[2].model.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );

        // A scripted (test) model gets no real model behind it.
        let spec = load(&LoadOptions {
            project_dir: Some(dir.path().to_path_buf()),
            model: Some("scripted:s.yaml".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(spec.fallbacks.is_empty());
    }

    #[test]
    fn a_base_url_after_the_model_makes_it_openai_compatible() {
        let m = ModelRef::parse("openai:my-finetune@https://llm.internal/v1").unwrap();
        assert_eq!(
            (m.provider.as_str(), m.model.as_str()),
            ("openai", "my-finetune")
        );
        assert_eq!(m.base_url.as_deref(), Some("https://llm.internal/v1"));
        assert_eq!(m.family(), Family::Compat);
        assert_eq!(m.to_string(), "openai:my-finetune");
        // An `@` that does not start a URL belongs to the model id.
        let m = ModelRef::parse("compat:model@v2").unwrap();
        assert_eq!((m.model.as_str(), m.base_url), ("model@v2", None));
    }

    #[test]
    fn without_a_project_directory_agent_files_are_never_read() {
        // Benchmark runs pass no project directory.
        let spec = load(&opts("anthropic:a")).unwrap();
        assert!(spec.agent.is_none());
        assert!(!spec.system_prompt.contains("support-bot"));
    }
}
