//! Loads `gitagent.yaml`, the model profile and the prompts into an [`AgentSpec`].
//!
//! Every file has a built-in default compiled from the repository, so the
//! static binary runs with no files next to it. A config directory given with
//! `--config` overrides the defaults file by file.

mod agents;
mod config;
mod profile;
pub mod sdk;
mod skills;

pub use agents::{AgentDef, GENERAL as GENERAL_AGENT};
pub use config::{Config, RepoConfig};
pub use profile::{
    AnthropicCfg, CacheCfg, ContextCfg, EditTool, Effort, GuardsCfg, Masking, OpenAiCfg, Profile,
    ShapeCfg, StopCfg, ThinkingBinding, ThinkingDisplay, ToolCalling, ToolNaming, ToolsCfg,
};
pub use skills::{split_frontmatter, Skill};

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const DEFAULT_CONFIG: &str = include_str!("../../../gitagent.yaml");
const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../../../prompts/system.md");
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

/// A `provider:model` reference, e.g. `openai:<model-id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn parse(s: &str) -> Result<Self> {
        match s.split_once(':') {
            Some((p, m)) if !p.is_empty() && !m.is_empty() => Ok(ModelRef {
                provider: p.to_string(),
                model: m.to_string(),
            }),
            _ => bail!("model must look like provider:model, got {s:?}"),
        }
    }

    pub fn family(&self) -> Family {
        match self.provider.as_str() {
            "anthropic" => Family::Anthropic,
            "openai" => Family::OpenAI,
            "google" | "gemini" => Family::Google,
            _ => Family::Compat,
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
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
    let config: Config = serde_saphyr::from_str(&config_text)
        .with_context(|| format!("parsing gitagent.yaml ({config_source})"))?;

    let model_str =
        opts.model.as_deref().or(config.model.as_deref()).context(
            "no model given: pass --model provider:model or set `model` in gitagent.yaml",
        )?;
    let model = ModelRef::parse(model_str)?;

    let profile_sel = opts.profile.as_deref().unwrap_or(&config.profile);
    let (profile, profile_source) = load_profile(profile_sel, model.family(), &config_dirs)?;

    let skills = if config.skills {
        skills::discover(&config_dirs)
    } else {
        Vec::new()
    };
    let mut system_prompt = build_system_prompt(&config_dirs)?;
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

    let agents = if profile.tools.subagents {
        agents::discover(&config_dirs)
    } else {
        Vec::new()
    };
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
fn build_system_prompt(dirs: &[PathBuf]) -> Result<String> {
    let system = match find_in(dirs, "prompts/system.md") {
        Some(p) => read(&p)?,
        None => DEFAULT_SYSTEM_PROMPT.to_string(),
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
}
