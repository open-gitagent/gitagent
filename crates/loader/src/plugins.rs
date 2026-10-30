//! Plugins, as gitagent 1.x had them: a directory with a `plugin.yaml` that
//! can add tools (`tools/*.yaml`), skills (`skills/`), hook scripts and a
//! piece of system prompt.
//!
//! ```yaml
//! id: web-search
//! name: Web search
//! version: 1.0.0
//! description: Searches the web.
//! provides:
//!   tools: true                      # tools/*.yaml in the plugin
//!   skills: true                     # skills/*/SKILL.md in the plugin
//!   prompt: PROMPT.md                # appended to the system prompt
//!   hooks: {pre_tool_use: [{script: check.sh, description: Checks}]}
//! config:
//!   properties: {api_key: {type: string, env: SEARCH_API_KEY}}
//!   required: [api_key]
//! ```
//!
//! Only plugins named in agent.yaml load:
//!
//! ```yaml
//! plugins:
//!   web-search: {enabled: true, config: {api_key: "${SEARCH_API_KEY}"}}
//!   linter: {source: https://github.com/x/linter.git, version: v1}
//! ```
//!
//! A plugin is looked for in `<agent>/plugins/<name>/`,
//! `~/.gitagent/plugins/<name>/` and `<agent>/.gitagent/plugins/<name>/`;
//! one with a git `source` is cloned into the last of those when missing.
//! A plugin whose tool names collide with the agent's or an earlier
//! plugin's is left out, as 1.x did.
//!
//! The plugin's settings reach its script tools and hook scripts: each one
//! in the variable its property names (`SEARCH_API_KEY` above), and all of
//! them as JSON in `GITAGENT_PLUGIN_CONFIG`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agent_hooks::{entry_in, parse_event, HookEntry};
use crate::declared::{expand_env, read_dir as read_tools, DeclaredTool};
use crate::manifest::{git_clone, is_git_source};
use crate::skills::Skill;
use crate::AgentDir;

/// A loaded plugin, for the record and the `plugin list` command.
#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    /// Its settings, resolved from agent.yaml, the environment and the
    /// defaults.
    pub config: Map<String, Value>,
}

/// What the agent's plugins add.
#[derive(Debug, Default)]
pub struct Loaded {
    pub plugins: Vec<Plugin>,
    pub tools: Vec<DeclaredTool>,
    pub skills: Vec<Skill>,
    pub hooks: Vec<HookEntry>,
    /// `# Plugin: <name>` sections for the system prompt.
    pub prompts: Vec<String>,
    pub warnings: Vec<String>,
}

/// Where a plugin named `name` is, if anywhere.
pub fn find(agent_root: &Path, home: Option<&Path>, name: &str) -> Option<PathBuf> {
    let mut places = vec![agent_root.join("plugins").join(name)];
    if let Some(h) = home {
        places.push(h.join(".gitagent/plugins").join(name));
    }
    places.push(agent_root.join(".gitagent/plugins").join(name));
    places.into_iter().find(|p| p.join("plugin.yaml").is_file())
}

/// The plugins agent.yaml names, with their tools, skills, hooks and prompt
/// text; `taken` are tool names already in use.
pub fn load(agent: &AgentDir, home: Option<&Path>, taken: &[String]) -> Loaded {
    let mut out = Loaded::default();
    let mut taken: Vec<String> = taken.to_vec();
    for (name, settings) in &agent.manifest.plugins {
        if settings["enabled"] == Value::Bool(false) {
            continue;
        }
        let mut dir = find(&agent.root, home, name);
        if dir.is_none() {
            if let Some(source) = settings["source"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                match install_source(&agent.root, source, settings["version"].as_str()) {
                    Ok(d) => dir = Some(d),
                    Err(e) => {
                        out.warnings.push(format!("plugin {name}: {e}; left out"));
                        continue;
                    }
                }
            }
        }
        let Some(dir) = dir else {
            out.warnings.push(format!(
                "plugin {name}: not found in plugins/, ~/.gitagent/plugins/ or .gitagent/plugins/; run `gitagent plugin install`"
            ));
            continue;
        };
        match load_one(name, &dir, settings, &mut taken) {
            Ok((plugin, tools, skills, hooks, prompt, warnings)) => {
                out.warnings.extend(warnings);
                out.plugins.push(plugin);
                out.tools.extend(tools);
                out.skills.extend(skills);
                out.hooks.extend(hooks);
                out.prompts.extend(prompt);
            }
            Err(e) => out.warnings.push(format!("plugin {name}: {e}; left out")),
        }
    }
    out
}

/// A git source cloned into `.gitagent/plugins/<basename>`; a local path is
/// used where it is.
fn install_source(root: &Path, source: &str, version: Option<&str>) -> Result<PathBuf, String> {
    if is_git_source(source) {
        let base = source
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("plugin")
            .trim_end_matches(".git");
        let dest = root.join(".gitagent/plugins").join(base);
        if !dest.join("plugin.yaml").is_file() {
            if dest.exists() {
                return Err(format!("{} exists but has no plugin.yaml", dest.display()));
            }
            git_clone(
                source,
                &dest,
                version.map(str::trim).filter(|v| !v.is_empty()),
            )
            .map_err(|e| format!("cloning {source}: {e}"))?;
        }
        return Ok(dest);
    }
    let path = if Path::new(source).is_absolute() {
        PathBuf::from(source)
    } else {
        root.join(source)
    };
    if path.join("plugin.yaml").is_file() {
        Ok(path)
    } else {
        Err(format!("{} has no plugin.yaml", path.display()))
    }
}

type One = (
    Plugin,
    Vec<DeclaredTool>,
    Vec<Skill>,
    Vec<HookEntry>,
    Option<String>,
    Vec<String>,
);

fn load_one(
    name: &str,
    dir: &Path,
    settings: &Value,
    taken: &mut Vec<String>,
) -> Result<One, String> {
    let text = std::fs::read_to_string(dir.join("plugin.yaml")).map_err(|e| e.to_string())?;
    let m: Value =
        serde_saphyr::from_str(&text).map_err(|e| format!("plugin.yaml is not valid YAML: {e}"))?;
    let s = |key: &str| m[key].as_str().map(str::trim).map(str::to_string);
    let id = s("id")
        .filter(|i| !i.is_empty())
        .ok_or("plugin.yaml has no `id`")?;
    let mut warnings = Vec::new();
    if let Some(engine) = s("engine") {
        if let Some(min) = engine.strip_prefix(">=").map(str::trim) {
            if version_before(env!("CARGO_PKG_VERSION"), min) {
                warnings.push(format!(
                    "plugin {name}: asks for gitagent {engine}; this is {}",
                    env!("CARGO_PKG_VERSION")
                ));
            }
        }
    }
    if m.get("entry").is_some() {
        warnings.push(format!(
            "plugin {name}: `entry` (a JavaScript module) is not run by this version; its tools, skills, hooks and prompt still load"
        ));
    }
    let provides = &m["provides"];
    let mut tools = Vec::new();
    if provides["tools"] == Value::Bool(true) {
        let (found, problems) = read_tools(dir);
        warnings.extend(problems.into_iter().map(|p| format!("plugin {name}: {p}")));
        for t in &found {
            if taken.contains(&t.name) {
                return Err(format!(
                    "its tool {:?} collides with an existing tool",
                    t.name
                ));
            }
        }
        taken.extend(found.iter().map(|t| t.name.clone()));
        tools = found;
    }
    let skills = if provides["skills"] == Value::Bool(true) {
        crate::skills::discover(std::slice::from_ref(&dir.to_path_buf()))
    } else {
        Vec::new()
    };
    let mut hooks = Vec::new();
    for (event_name, list) in provides["hooks"].as_object().into_iter().flatten() {
        let Some(event) = parse_event(event_name) else {
            warnings.push(format!("plugin {name}: unknown hook event {event_name:?}"));
            continue;
        };
        for h in list.as_array().into_iter().flatten() {
            let Some(script) = h["script"].as_str() else {
                warnings.push(format!(
                    "plugin {name}: a {event_name} hook has no `script`"
                ));
                continue;
            };
            match entry_in(
                event,
                script,
                h["description"].as_str().unwrap_or_default(),
                dir,
            ) {
                Ok(e) => hooks.push(e),
                Err(why) => warnings.push(format!("plugin {name}: {why}")),
            }
        }
    }
    let prompt = match provides["prompt"].as_str() {
        Some(p) => match std::fs::read_to_string(dir.join(p)) {
            Ok(t) if !t.trim().is_empty() => Some(format!(
                "# Plugin: {}\n\n{}\n",
                s("name").unwrap_or_else(|| id.clone()),
                t.trim_end()
            )),
            Ok(_) => None,
            Err(e) => {
                warnings.push(format!("plugin {name}: prompt file {p}: {e}"));
                None
            }
        },
        None => None,
    };
    // Settings: agent.yaml's `config`, then the property's environment
    // variable, then its default.
    let mut config = Map::new();
    let given = settings["config"].as_object();
    for (key, prop) in m["config"]["properties"].as_object().into_iter().flatten() {
        let from_agent = given
            .and_then(|g| g.get(key))
            .filter(|v| !v.is_null())
            .map(|v| match v {
                Value::String(s) => Value::String(expand_env(s)),
                other => other.clone(),
            });
        let from_env = prop["env"]
            .as_str()
            .and_then(|var| std::env::var(var).ok())
            .map(|raw| match prop["type"].as_str() {
                Some("number") => raw
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map_or(Value::String(raw), Value::Number),
                Some("boolean") => Value::Bool(matches!(raw.as_str(), "true" | "1")),
                _ => Value::String(raw),
            });
        if let Some(v) = from_agent
            .or(from_env)
            .or_else(|| prop.get("default").filter(|d| !d.is_null()).cloned())
        {
            config.insert(key.clone(), v);
        }
    }
    for key in m["config"]["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !config.contains_key(key) {
            warnings.push(format!(
                "plugin {name}: the setting `{key}` is required and not set"
            ));
        }
    }
    // The settings reach the plugin's own scripts: each under the variable
    // its property names, and all of them as JSON.
    let mut env = BTreeMap::new();
    for (key, prop) in m["config"]["properties"].as_object().into_iter().flatten() {
        if let (Some(var), Some(v)) = (prop["env"].as_str(), config.get(key)) {
            let text = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            env.insert(var.to_string(), text);
        }
    }
    env.insert(
        "GITAGENT_PLUGIN_CONFIG".to_string(),
        Value::Object(config.clone()).to_string(),
    );
    for t in &mut tools {
        t.env = env.clone();
    }
    for h in &mut hooks {
        h.env = env.clone();
    }
    let plugin = Plugin {
        id,
        name: s("name").unwrap_or_else(|| name.to_string()),
        version: s("version").unwrap_or_default(),
        description: s("description").unwrap_or_default(),
        dir: dir.to_path_buf(),
        config,
    };
    Ok((plugin, tools, skills, hooks, prompt, warnings))
}

/// Whether version `a` is older than `b` (`X.Y.Z`).
fn version_before(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    parts(a) < parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: PathBuf, t: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }

    #[test]
    fn plugins_add_tools_skills_hooks_and_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agent");
        let home = dir.path().join("home");
        std::env::set_var("GITAGENT_TEST_PLUGIN_KEY", "from-env");
        write(
            root.join("agent.yaml"),
            "name: a\nversion: 1.0.0\ndescription: A.\nplugins:\n  search: {config: {region: eu}}\n  web-dup: {}\n  off: {enabled: false}\n  missing: {}\n  home-one: {}\n",
        );
        write(root.join("SOUL.md"), "I am A.\n");
        write(
            root.join("plugins/search/plugin.yaml"),
            "id: search\nname: Search\nversion: 1.0.0\ndescription: Searches.\nengine: \">=0.0.1\"\nprovides:\n  tools: true\n  skills: true\n  prompt: PROMPT.md\n  hooks:\n    pre_tool_use: [{script: check.sh, description: Checks}]\n    on_lunch: []\nconfig:\n  properties:\n    api_key: {type: string, env: GITAGENT_TEST_PLUGIN_KEY}\n    region: {type: string, default: us}\n    retries: {type: number, default: 2}\n    token: {type: string}\n  required: [api_key, token]\nentry: index.js\n",
        );
        write(root.join("plugins/search/tools/web.sh"), "echo ok\n");
        write(
            root.join("plugins/search/tools/web.yaml"),
            "name: web\ndescription: Web search.\nimplementation: {script: web.sh}\n",
        );
        write(
            root.join("plugins/search/skills/cite/SKILL.md"),
            "---\nname: cite\ndescription: Cite sources.\n---\nx\n",
        );
        write(root.join("plugins/search/check.sh"), "true\n");
        write(
            root.join("plugins/search/PROMPT.md"),
            "Prefer primary sources.\n",
        );
        write(
            root.join("plugins/web-dup/plugin.yaml"),
            "id: web-dup\nname: Web dup\nversion: 1.0.0\ndescription: Collides.\nprovides: {tools: true}\n",
        );
        write(
            root.join("plugins/web-dup/tools/web.yaml"),
            "name: web\ndescription: Another web.\nimplementation: {script: web.sh}\n",
        );
        write(root.join("plugins/web-dup/tools/web.sh"), "echo clash\n");
        write(
            home.join(".gitagent/plugins/home-one/plugin.yaml"),
            "id: home-one\nname: Home one\nversion: 0.1.0\ndescription: From the home directory.\nprovides: {prompt: P.md}\n",
        );
        write(
            home.join(".gitagent/plugins/home-one/P.md"),
            "Home says hi.\n",
        );
        let agent = AgentDir::load(&root).unwrap().unwrap();
        let loaded = load(&agent, Some(&home), &[]);
        let ids: Vec<&str> = loaded.plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["home-one", "search"]);
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools[0].description, "Web search.");
        assert_eq!(loaded.skills[0].name, "cite");
        assert_eq!(loaded.hooks[0].description, "Checks");
        assert!(loaded.hooks[0]
            .command
            .last()
            .unwrap()
            .ends_with("plugins/search/check.sh"));
        assert_eq!(
            loaded.prompts,
            vec![
                "# Plugin: Home one\n\nHome says hi.\n",
                "# Plugin: Search\n\nPrefer primary sources.\n"
            ]
        );
        let search = loaded.plugins.iter().find(|p| p.id == "search").unwrap();
        assert_eq!(search.config["api_key"], "from-env");
        assert_eq!(search.config["region"], "eu");
        assert_eq!(search.config["retries"], 2);
        // The plugin's own tool and hook get its settings.
        for env in [&loaded.tools[0].env, &loaded.hooks[0].env] {
            assert_eq!(env["GITAGENT_TEST_PLUGIN_KEY"], "from-env");
            assert_eq!(
                env["GITAGENT_PLUGIN_CONFIG"],
                r#"{"api_key":"from-env","region":"eu","retries":2}"#
            );
        }
        let w = loaded.warnings.join("\n");
        for expected in [
            "plugin web-dup: its tool \"web\" collides with an existing tool; left out",
            "plugin missing: not found",
            "plugin search: the setting `token` is required and not set",
            "plugin search: unknown hook event \"on_lunch\"",
            "plugin search: `entry` (a JavaScript module) is not run",
        ] {
            assert!(w.contains(expected), "missing {expected:?} in:\n{w}");
        }
        assert!(!w.contains("plugin off"));
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(version_before("0.1.0", "0.3.0"));
        assert!(!version_before("2.2.0", "0.3.0"));
        assert!(!version_before("1.0.0", "1.0.0"));
    }
}
