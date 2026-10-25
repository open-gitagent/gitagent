//! Environment settings in an agent's `config/`: `default.yaml`, with
//! `config/<env>.yaml` merged on top for the environment named by `--env`
//! (or `GITAGENT_ENV`). Mappings merge key by key; lists and single values
//! from the environment file replace the defaults.
//!
//! ```yaml
//! log_level: info               # debug, info, warn, error
//! model_override: claude-sonnet-5-5   # beats --model, as in 1.x
//! temperature_override: 0.1
//! timeout_override: 600         # seconds a command may run before it goes on in the background
//! env_vars: {API_BASE: "https://staging.example.com"}   # for tool and hook scripts
//! compliance_mode: true         # the agent must pass `validate --compliance`
//! audit_logging: true
//! ```
//!
//! Other keys are the agent's own; they are kept in [`EnvConfig::settings`]
//! and never cause an error.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnvConfig {
    /// The environment the settings came from (`default` alone when none
    /// was named).
    pub name: Option<String>,
    pub log_level: Option<String>,
    pub model_override: Option<String>,
    pub temperature_override: Option<f32>,
    pub timeout_override: Option<u64>,
    /// Set for the agent's tool and hook scripts.
    pub env_vars: BTreeMap<String, String>,
    pub compliance_mode: bool,
    pub audit_logging: bool,
    /// Every setting after merging, the known ones included.
    pub settings: Map<String, Value>,
}

/// The settings for `env` (or none), each file taken from the first of
/// `roots` (the agent, then its parents) that has it, and what was wrong.
pub fn load(roots: &[PathBuf], env: Option<&str>) -> (EnvConfig, Vec<String>) {
    let mut warnings = Vec::new();
    let find = |name: &str| -> Option<PathBuf> {
        roots
            .iter()
            .map(|r| r.join("config").join(format!("{name}.yaml")))
            .find(|p| p.is_file())
    };
    let mut read = |name: &str| -> Map<String, Value> {
        let Some(path) = find(name) else {
            return Map::new();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Map::new();
        };
        match serde_saphyr::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => m,
            Ok(Value::Null) => Map::new(),
            Ok(_) => {
                warnings.push(format!("config/{name}.yaml: expected a mapping; ignored"));
                Map::new()
            }
            Err(e) => {
                warnings.push(format!("config/{name}.yaml: not valid YAML: {e}; ignored"));
                Map::new()
            }
        }
    };
    let mut settings = read("default");
    if let Some(env) = env.map(str::trim).filter(|e| !e.is_empty()) {
        if !env
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            warnings.push(format!(
                "the environment name {env:?} may only use letters, digits, `-` and `_`; ignored"
            ));
        } else if find(env).is_none() {
            warnings.push(format!(
                "config/{env}.yaml does not exist; using the defaults"
            ));
        } else {
            merge(&mut settings, read(env));
        }
    }
    let (cfg, mut problems) = parse(settings, env);
    warnings.append(&mut problems);
    (cfg, warnings)
}

/// `over` merged into `base`: mappings key by key, anything else replaced.
pub fn merge(base: &mut Map<String, Value>, over: Map<String, Value>) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(Value::Object(b)), Value::Object(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// The known settings, with the problems in them.
fn parse(settings: Map<String, Value>, env: Option<&str>) -> (EnvConfig, Vec<String>) {
    let mut warnings = Vec::new();
    let mut warn = |key: &str, what: &str| {
        warnings.push(format!("config: `{key}` must be {what}; ignored"));
    };
    let mut cfg = EnvConfig {
        name: env.map(str::to_string),
        ..EnvConfig::default()
    };
    for (key, v) in &settings {
        match (key.as_str(), v) {
            (_, Value::Null) => {}
            ("log_level", Value::String(s))
                if matches!(s.as_str(), "debug" | "info" | "warn" | "error") =>
            {
                cfg.log_level = Some(s.clone())
            }
            ("log_level", _) => warn(key, "debug, info, warn or error"),
            ("model_override", Value::String(s)) if !s.trim().is_empty() => {
                cfg.model_override = Some(s.trim().to_string())
            }
            ("model_override", _) => warn(key, "a model name"),
            ("temperature_override", v) => match v.as_f64() {
                Some(t) if (0.0..=2.0).contains(&t) => cfg.temperature_override = Some(t as f32),
                _ => warn(key, "a number between 0 and 2"),
            },
            ("timeout_override", v) => match v.as_u64() {
                Some(t) if t >= 1 => cfg.timeout_override = Some(t),
                _ => warn(key, "a whole number of seconds"),
            },
            ("env_vars", Value::Object(m)) => {
                for (name, value) in m {
                    let value = match value {
                        Value::String(s) => s.clone(),
                        Value::Null => continue,
                        other => other.to_string(),
                    };
                    cfg.env_vars.insert(name.clone(), value);
                }
            }
            ("env_vars", _) => warn(key, "a mapping of variable names to values"),
            ("compliance_mode", Value::Bool(b)) => cfg.compliance_mode = *b,
            ("compliance_mode", _) => warn(key, "true or false"),
            ("audit_logging", Value::Bool(b)) => cfg.audit_logging = *b,
            ("audit_logging", _) => warn(key, "true or false"),
            _ => {}
        }
    }
    cfg.settings = settings;
    (cfg, warnings)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use serde_json::json;

    fn write(root: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config").join(name), text).unwrap();
    }

    #[test]
    fn environments_merge_onto_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let roots = [root.to_path_buf()];
        let (cfg, warnings) = load(&roots, None);
        assert_eq!(cfg, EnvConfig::default());
        assert!(warnings.is_empty());

        write(
            root,
            "default.yaml",
            "log_level: info\nmodel_override: claude-sonnet-5-5\nenv_vars: {API_BASE: dev, REGION: eu}\nsearch: {depth: 2, engines: [a, b]}\n",
        );
        write(
            root,
            "prod.yaml",
            "model_override: claude-opus-5-5\ntimeout_override: 600\nenv_vars: {API_BASE: prod}\nsearch: {engines: [c]}\ncompliance_mode: true\n",
        );
        let (cfg, warnings) = load(&roots, Some("prod"));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg.name.as_deref(), Some("prod"));
        assert_eq!(cfg.model_override.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(cfg.timeout_override, Some(600));
        assert_eq!(cfg.log_level.as_deref(), Some("info"));
        assert!(cfg.compliance_mode && !cfg.audit_logging);
        assert_eq!(
            cfg.env_vars,
            [("API_BASE", "prod"), ("REGION", "eu")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        );
        // The agent's own settings merge the same way; lists are replaced.
        assert_eq!(
            cfg.settings["search"],
            json!({"depth": 2, "engines": ["c"]})
        );

        let (cfg, warnings) = load(&roots, Some("staging"));
        assert_eq!(cfg.model_override.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            warnings,
            vec!["config/staging.yaml does not exist; using the defaults"]
        );
    }

    #[test]
    fn wrong_values_are_warnings() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "default.yaml",
            "log_level: loud\ntemperature_override: 5\ntimeout_override: 0\nenv_vars: [a]\ncompliance_mode: yes\nmodel_override: null\n",
        );
        let roots = [dir.path().to_path_buf()];
        let (cfg, warnings) = load(&roots, None);
        assert_eq!(cfg.log_level, None);
        assert_eq!(cfg.temperature_override, None);
        assert_eq!(cfg.timeout_override, None);
        assert!(cfg.env_vars.is_empty());
        assert!(cfg.compliance_mode, "YAML reads `yes` as true");
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        write(dir.path(), "default.yaml", "- not a mapping\n");
        let (_, warnings) = load(&roots, None);
        assert_eq!(
            warnings,
            vec!["config/default.yaml: expected a mapping; ignored"]
        );
    }
}
