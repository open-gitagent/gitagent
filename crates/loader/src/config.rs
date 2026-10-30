use std::collections::BTreeMap;

use serde::Deserialize;

/// The time limit of an unattended run when none is set: 30 minutes.
pub const DEFAULT_TIMEOUT_S: u64 = 1800;

/// `gitagent.yaml` (BLUEPRINT §6). Settings it does not know are ignored
/// with a warning ([`parse`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub model: Option<String>,
    pub fallback_models: Vec<String>,
    pub max_turns: u32,
    /// Wall-clock limit per run in seconds. When unset, unattended runs
    /// (`-p`, schedules, the SDKs) use [`DEFAULT_TIMEOUT_S`] and the console
    /// has no limit.
    pub timeout_s: Option<u64>,
    pub profile: String,
    pub sandbox: String,
    pub skills: bool,
    pub hooks: bool,
    /// `off` or `online`: after each finished task outside `--bench`, learn
    /// lessons into the project's `.gitagent/`.
    pub learning: String,
    /// Persistent memory in the project's `memory/` and the `memory` tool
    /// (always on for agent directories with an `agent.yaml`).
    pub memory: bool,
    /// Commit each memory save to git.
    pub memory_commit: bool,
    /// MCP servers to connect (see `mcp.rs`); these replace servers of the
    /// same name in `agent.yaml` and `.mcp.json`.
    pub mcp_servers: BTreeMap<String, serde_json::Value>,
    pub repo: RepoConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: "gitagent".into(),
            model: None,
            fallback_models: Vec::new(),
            max_turns: 200,
            timeout_s: None,
            profile: "auto".into(),
            sandbox: "auto".into(),
            skills: true,
            hooks: true,
            learning: "off".into(),
            memory: false,
            memory_commit: true,
            mcp_servers: BTreeMap::new(),
            repo: RepoConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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

/// The settings of [`Config`] and [`RepoConfig`], by their names in the file.
const KEYS: &[&str] = &[
    "name",
    "model",
    "fallback_models",
    "max_turns",
    "timeout_s",
    "profile",
    "sandbox",
    "skills",
    "hooks",
    "learning",
    "memory",
    "memory_commit",
    "mcp_servers",
    "repo",
];
const REPO_KEYS: &[&str] = &["branch_prefix", "commit_every_turns", "author", "push"];

/// Reads `gitagent.yaml`, and the settings in it that it does not know
/// (`max_turn`, `repo.remote`), which are ignored. A known setting with a
/// wrong value is an error.
pub fn parse(text: &str) -> anyhow::Result<(Config, Vec<String>)> {
    let config: Config = serde_saphyr::from_str(text)?;
    let mut unknown = Vec::new();
    // Only the names are looked at here; the values were read above.
    if let Ok(serde_json::Value::Object(top)) = serde_saphyr::from_str(text) {
        unknown.extend(top.keys().filter(|k| !KEYS.contains(&k.as_str())).cloned());
        let repo = top.get("repo").and_then(serde_json::Value::as_object);
        for key in repo.into_iter().flat_map(|r| r.keys()) {
            if !REPO_KEYS.contains(&key.as_str()) {
                unknown.push(format!("repo.{key}"));
            }
        }
    }
    Ok((config, unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_is_known_and_unknown_ones_are_ignored() {
        let (_, unknown) = parse(include_str!("../../../gitagent.yaml")).unwrap();
        assert!(unknown.is_empty(), "{unknown:?}");
        let (c, unknown) = parse(
            "model: anthropic:m\ntimeout_s: 60\nmax_turn: 5\nrepo: {push: true, remote: x}\n",
        )
        .unwrap();
        assert_eq!(c.model.as_deref(), Some("anthropic:m"));
        assert_eq!(c.timeout_s, Some(60));
        assert_eq!(c.max_turns, 200);
        assert!(c.repo.push);
        assert_eq!(unknown, ["max_turn", "repo.remote"]);
        // A wrong value for a known setting is still an error.
        assert!(parse("max_turns: many\n").is_err());
        // An empty file is the defaults.
        assert_eq!(parse("# nothing\n").unwrap().0.max_turns, 200);
    }
}
