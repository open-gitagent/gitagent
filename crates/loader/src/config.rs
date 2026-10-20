use serde::Deserialize;

/// `gitagent.yaml` (BLUEPRINT §6).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub name: String,
    pub model: Option<String>,
    pub fallback_models: Vec<String>,
    pub max_turns: u32,
    pub timeout_s: u64,
    pub profile: String,
    pub sandbox: String,
    pub skills: bool,
    pub hooks: bool,
    /// `off` or `online`: after each finished task outside `--bench`, learn
    /// lessons into the project's `.gitagent/`.
    pub learning: String,
    pub repo: RepoConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: "gitagent".into(),
            model: None,
            fallback_models: Vec::new(),
            max_turns: 200,
            timeout_s: 1800,
            profile: "auto".into(),
            sandbox: "auto".into(),
            skills: true,
            hooks: true,
            learning: "off".into(),
            repo: RepoConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RepoConfig {
    pub branch_prefix: String,
    pub commit_every_turns: u32,
    pub author: String,
    pub push: bool,
}

impl Default for RepoConfig {
    fn default() -> Self {
        RepoConfig {
            branch_prefix: "gitagent/".into(),
            commit_every_turns: 0,
            author: "gitagent <gitagent@localhost>".into(),
            push: false,
        }
    }
}
