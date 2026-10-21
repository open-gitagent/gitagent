//! Tools an agent declares in `tools/<name>.yaml`: a name, a description,
//! an input schema and how to run it. Both layouts are read:
//!
//! ```yaml
//! implementation: {type: script, path: lint.py, runtime: python3, timeout: 30}
//! implementation: {type: http, url: "https://…", method: POST, headers: {Authorization: "Bearer ${TOKEN}"}}
//! implementation: {script: lint.sh, runtime: bash}   # 1.x
//! ```
//!
//! Script paths are relative to `tools/`. The agent's `tools:` list, when
//! given, names the ones enabled; otherwise every file is. Problems are
//! warnings and the tool is left out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::AgentDir;

/// How long a declared tool may run unless it says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Names of gitagent's own tools, which a declared tool cannot take.
const RESERVED: &[&str] = &[
    "exec",
    "write_stdin",
    "read_file",
    "write_file",
    "str_replace",
    "apply_patch",
    "image_read",
    "todo",
    "task_complete",
    "skill",
    "subagent",
    "memory",
];

#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredTool {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (`type: object`).
    pub input_schema: Value,
    pub runner: Runner,
    pub timeout: Duration,
    /// `annotations.requires_confirmation`: a person approves each call.
    pub requires_confirmation: bool,
    /// `annotations.read_only`: the tool changes nothing.
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Runner {
    /// A program and its arguments, run in the agent directory; the call's
    /// arguments arrive on stdin as one JSON object.
    Script { command: Vec<String>, dir: PathBuf },
    /// A request with the arguments as a JSON body (POST, PUT) or as query
    /// parameters (GET, DELETE). `${VAR}` in headers is filled from the
    /// environment.
    Http {
        url: String,
        method: String,
        headers: BTreeMap<String, String>,
    },
    /// The tool of the same name on the MCP server at `url`.
    Mcp { url: String },
}

/// The enabled tools of an agent directory, and what was wrong with the rest.
pub fn discover(agent: &AgentDir) -> (Vec<DeclaredTool>, Vec<String>) {
    let dir = agent.root.join("tools");
    let mut warnings = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")))
        .collect();
    files.sort();

    let mut tools: Vec<DeclaredTool> = Vec::new();
    for file in files {
        match read(&file, &agent.root) {
            Ok(t) if tools.iter().any(|o| o.name == t.name) => warnings.push(format!(
                "{}: a tool named {:?} is already declared",
                shown(&file, &agent.root),
                t.name
            )),
            Ok(t) => tools.push(t),
            Err(why) => warnings.push(format!("{}: {why}", shown(&file, &agent.root))),
        }
    }
    let enabled = &agent.manifest.tools;
    if !enabled.is_empty() {
        for name in enabled {
            if !tools.iter().any(|t| &t.name == name) {
                warnings.push(format!(
                    "agent.yaml lists the tool {name:?}, but no usable tools/{name}.yaml declares it"
                ));
            }
        }
        tools.retain(|t| enabled.contains(&t.name));
    }
    (tools, warnings)
}

fn shown(file: &Path, root: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string()
}

/// One tool file; the error says why it cannot be used.
fn read(file: &Path, root: &Path) -> Result<DeclaredTool, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let v: Value = serde_saphyr::from_str(&text).map_err(|e| format!("not valid YAML: {e}"))?;
    let s = |key: &str| v.get(key).and_then(Value::as_str).map(str::trim);

    let name = s("name").filter(|n| !n.is_empty()).ok_or("no `name`")?;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || name.len() > 64
    {
        return Err(format!(
            "the name {name:?} may only use letters, digits, `-` and `_` (at most 64)"
        ));
    }
    if RESERVED.contains(&name) {
        return Err(format!("{name:?} is the name of a built-in tool"));
    }
    let description = s("description")
        .filter(|d| !d.is_empty())
        .ok_or("no `description`")?;
    let mut input_schema = match v.get("input_schema") {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        None => json!({}),
        Some(_) => return Err("`input_schema` must be a mapping".into()),
    };
    input_schema["type"] = json!("object");
    if input_schema.get("properties").is_none() {
        input_schema["properties"] = json!({});
    }

    let imp = v.get("implementation").ok_or("no `implementation`")?;
    let field = |key: &str| imp.get(key).and_then(Value::as_str).map(str::trim);
    let timeout = match imp.get("timeout") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT,
        Some(t) => match t.as_u64() {
            Some(n) if n >= 1 => Duration::from_secs(n),
            _ => return Err("`implementation.timeout` must be a whole number of seconds".into()),
        },
    };
    let kind = field("type").unwrap_or(if field("url").is_some() && field("method").is_some() {
        "http"
    } else {
        "script"
    });
    let runner = match kind {
        "script" => {
            let rel = field("path")
                .or(field("script"))
                .ok_or("a script tool needs `implementation.path`")?;
            let path = root.join("tools").join(rel);
            if !path.is_file() {
                return Err(format!("the script tools/{rel} does not exist"));
            }
            let mut command = interpreter(field("runtime"), &path);
            command.push(path.display().to_string());
            Runner::Script {
                command,
                dir: root.to_path_buf(),
            }
        }
        "http" => {
            let url = field("url").ok_or("an http tool needs `implementation.url`")?;
            let method = field("method").unwrap_or("POST").to_ascii_uppercase();
            if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "DELETE") {
                return Err(format!(
                    "`implementation.method` must be GET, POST, PUT or DELETE, not {method}"
                ));
            }
            let headers = imp
                .get("headers")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            Runner::Http {
                url: url.to_string(),
                method,
                headers,
            }
        }
        "mcp_server" => Runner::Mcp {
            url: field("url")
                .ok_or("an mcp_server tool needs `implementation.url`")?
                .to_string(),
        },
        other => {
            return Err(format!(
                "`implementation.type` must be script, http or mcp_server, not {other:?}"
            ))
        }
    };
    let flag = |key: &str| {
        v.get("annotations")
            .and_then(|a| a.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Ok(DeclaredTool {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        runner,
        timeout,
        requires_confirmation: flag("requires_confirmation"),
        read_only: flag("read_only"),
    })
}

/// The program that runs a script: the declared runtime, else one chosen by
/// the file's extension, else the file itself when it is executable, else `sh`.
fn interpreter(runtime: Option<&str>, path: &Path) -> Vec<String> {
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let by_name = |r: &str| match r {
        "python3" | "python" => Some(words(&["python3"])),
        "node" => Some(words(&["node"])),
        "bash" => Some(words(&["bash"])),
        "sh" => Some(words(&["sh"])),
        "deno" => Some(words(&["deno", "run", "--allow-all"])),
        "ruby" => Some(words(&["ruby"])),
        "go" => Some(words(&["go", "run"])),
        _ => None,
    };
    if let Some(r) = runtime.filter(|r| !r.is_empty()) {
        return by_name(r).unwrap_or_else(|| words(&[r]));
    }
    let by_ext = match path.extension().and_then(|e| e.to_str()) {
        Some("py") => by_name("python3"),
        Some("js" | "mjs" | "cjs") => by_name("node"),
        Some("ts") => by_name("deno"),
        Some("rb") => by_name("ruby"),
        Some("go") => by_name("go"),
        Some("sh" | "bash") => by_name("bash"),
        _ => None,
    };
    by_ext.unwrap_or_else(|| {
        if is_executable(path) {
            Vec::new()
        } else {
            words(&["sh"])
        }
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    false
}

/// `${VAR}` replaced by the variable's value (empty when unset).
pub fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start + 2..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        let name = &rest[start + 2..start + 2 + len];
        out.push_str(&std::env::var(name).unwrap_or_default());
        rest = &rest[start + 2 + len + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(root: &Path, manifest_tools: &str) -> AgentDir {
        std::fs::write(
            root.join("agent.yaml"),
            format!("name: t\nversion: 1.0.0\ndescription: d\n{manifest_tools}"),
        )
        .unwrap();
        std::fs::write(root.join("SOUL.md"), "I test.\n").unwrap();
        AgentDir::load(root).unwrap().unwrap()
    }

    fn tool(root: &Path, file: &str, yaml: &str) {
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(root.join("tools").join(file), yaml).unwrap();
    }

    #[test]
    fn both_layouts_are_read_and_problems_are_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tool(root, "lint.sh", "echo ok\n");
        tool(
            root,
            "lint.yaml",
            "name: lint\ndescription: Lint a file.\ninput_schema:\n  properties:\n    path: {type: string}\n  required: [path]\nimplementation: {script: lint.sh}\nannotations: {read_only: true}\n",
        );
        tool(
            root,
            "search.yml",
            "name: search\ndescription: Search.\ninput_schema: {type: object, properties: {q: {type: string}}}\nimplementation: {type: http, url: \"https://example.com/s\", method: get, headers: {X-Key: \"${KEY}\"}, timeout: 5}\n",
        );
        tool(
            root,
            "broken.yaml",
            "name: broken\ndescription: Missing its script.\nimplementation: {type: script, path: nope.py}\n",
        );
        tool(
            root,
            "exec.yaml",
            "name: exec\ndescription: Shadowing.\nimplementation: {script: lint.sh}\n",
        );
        let (tools, warnings) = discover(&agent(root, ""));
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["lint", "search"]);

        let lint = &tools[0];
        assert_eq!(lint.input_schema["type"], "object");
        assert!(lint.read_only && !lint.requires_confirmation);
        assert_eq!(lint.timeout, DEFAULT_TIMEOUT);
        match &lint.runner {
            Runner::Script { command, dir } => {
                assert_eq!(command[0], "bash");
                assert!(command[1].ends_with("tools/lint.sh"));
                assert_eq!(dir, root);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(tools[1].timeout, Duration::from_secs(5));
        assert!(matches!(&tools[1].runner, Runner::Http { method, .. } if method == "GET"));

        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("tools/nope.py does not exist"));
        assert!(warnings[1].contains("built-in tool"));
    }

    #[test]
    fn the_agents_tools_list_picks_the_enabled_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tool(root, "a.sh", "echo a\n");
        for name in ["a", "b"] {
            tool(
                root,
                &format!("{name}.yaml"),
                &format!("name: {name}\ndescription: {name}.\nimplementation: {{script: a.sh}}\n"),
            );
        }
        let (tools, warnings) = discover(&agent(root, "tools: [b, c]\n"));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "b");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("\"c\""));
    }

    #[test]
    fn env_references_expand() {
        std::env::set_var("GITAGENT_TEST_DECLARED", "v");
        assert_eq!(
            expand_env("Bearer ${GITAGENT_TEST_DECLARED}/${GITAGENT_TEST_UNSET_X}!"),
            "Bearer v/!"
        );
        assert_eq!(expand_env("${open"), "${open");
    }
}
