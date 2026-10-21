//! MCP servers to connect, from `mcp_servers` in `agent.yaml` and
//! `gitagent.yaml` and `mcpServers` in the project's `.mcp.json`:
//!
//! ```yaml
//! mcp_servers:
//!   github:                       # stdio: a program spoken to on stdin/stdout
//!     command: npx
//!     args: ["-y", "@modelcontextprotocol/server-github"]
//!     env: {GITHUB_TOKEN: "${GITHUB_TOKEN}"}
//!   docs:                         # HTTP (streamable; `type: sse` for the older transport)
//!     url: https://docs.example.com/mcp
//!     headers: {Authorization: "Bearer ${DOCS_TOKEN}"}
//!     timeout: 60                 # seconds to connect and list tools (30)
//! ```
//!
//! `${VAR}` is filled from the environment when connecting, not here, so
//! values never end up in the prompt or the record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct McpServerCfg {
    pub name: String,
    pub transport: McpTransport,
    /// To connect and list the tools.
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    /// Streamable HTTP.
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// The older HTTP+SSE transport.
    Sse {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl McpTransport {
    pub fn kind(&self) -> &'static str {
        match self {
            McpTransport::Stdio { .. } => "stdio",
            McpTransport::Http { .. } => "HTTP",
            McpTransport::Sse { .. } => "SSE",
        }
    }
}

/// The servers of several sources, later sources replacing earlier ones of
/// the same name; sorted by name. `base` resolves relative `cwd`s.
pub fn servers(
    sources: &[(&str, &BTreeMap<String, Value>)],
    base: &Path,
) -> (Vec<McpServerCfg>, Vec<String>) {
    let mut by_name: BTreeMap<String, McpServerCfg> = BTreeMap::new();
    let mut warnings = Vec::new();
    for (source, map) in sources {
        for (name, v) in *map {
            match parse(name, v, base) {
                Ok(cfg) => {
                    by_name.insert(name.clone(), cfg);
                }
                Err(why) => warnings.push(format!("{source}: MCP server {name:?}: {why}")),
            }
        }
    }
    (by_name.into_values().collect(), warnings)
}

/// `mcpServers` of a `.mcp.json` file, if there is one.
pub fn read_mcp_json(path: &Path) -> Result<BTreeMap<String, Value>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(BTreeMap::new());
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
    Ok(v.get("mcpServers")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default())
}

/// One server's settings.
pub fn parse(name: &str, v: &Value, base: &Path) -> Result<McpServerCfg, String> {
    if name.trim().is_empty() {
        return Err("the name is empty".into());
    }
    let s = |key: &str| v.get(key).and_then(Value::as_str).map(str::trim);
    let map = |key: &str| -> BTreeMap<String, String> {
        v.get(key)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        let v = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                        (k.clone(), v)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let timeout = match (v.get("timeout"), v.get("timeoutMs")) {
        (Some(t), _) => Duration::from_secs(
            t.as_u64()
                .filter(|n| *n >= 1)
                .ok_or("`timeout` must be a whole number of seconds")?,
        ),
        (None, Some(t)) => Duration::from_millis(
            t.as_u64()
                .filter(|n| *n >= 1)
                .ok_or("`timeoutMs` must be a whole number")?,
        ),
        (None, None) => DEFAULT_TIMEOUT,
    };
    let transport = match (s("command"), s("url"), s("type")) {
        (Some(_), Some(_), _) => return Err("give either `command` or `url`, not both".into()),
        (Some(command), None, None | Some("stdio")) => McpTransport::Stdio {
            command: command.to_string(),
            args: v
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            env: map("env"),
            cwd: s("cwd").map(|c| base.join(c)),
        },
        (None, Some(url), None | Some("http" | "streamable-http" | "streamableHttp")) => {
            McpTransport::Http {
                url: url.to_string(),
                headers: map("headers"),
            }
        }
        (None, Some(url), Some("sse")) => McpTransport::Sse {
            url: url.to_string(),
            headers: map("headers"),
        },
        (None, None, _) => return Err("needs `command` (stdio) or `url` (HTTP)".into()),
        (_, _, Some(t)) => return Err(format!("`type: {t}` does not fit the other settings")),
    };
    Ok(McpServerCfg {
        name: name.to_string(),
        transport,
        timeout,
    })
}

/// The system prompt's note on MCP tools, or nothing without servers.
pub fn prompt_section(servers: &[McpServerCfg]) -> String {
    if servers.is_empty() {
        return String::new();
    }
    let list: Vec<String> = servers
        .iter()
        .map(|s| format!("- {} ({})", s.name, s.transport.kind()))
        .collect();
    format!(
        "# MCP servers\n\nTools from these servers are named `<server>__<tool>`:\n{}\n",
        list.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> BTreeMap<String, Value> {
        v.as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[test]
    fn transports_sources_and_problems() {
        let base = Path::new("/agent");
        let agent = map(json!({
            "github": {"command": "npx", "args": ["-y", "server-github"], "env": {"T": "${T}"}, "cwd": "tools"},
            "docs": {"url": "https://a.example/mcp", "headers": {"A": "b"}, "timeout": 5},
            "both": {"command": "x", "url": "https://x"},
            "none": {"args": []}
        }));
        let runtime = map(json!({
            "docs": {"type": "sse", "url": "https://b.example/sse", "timeoutMs": 1500}
        }));
        let (servers, warnings) =
            servers(&[("agent.yaml", &agent), ("gitagent.yaml", &runtime)], base);
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["docs", "github"]);
        assert_eq!(
            servers[0],
            McpServerCfg {
                name: "docs".into(),
                transport: McpTransport::Sse {
                    url: "https://b.example/sse".into(),
                    headers: BTreeMap::new()
                },
                timeout: Duration::from_millis(1500),
            }
        );
        match &servers[1].transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["-y", "server-github"]);
                assert_eq!(env["T"], "${T}");
                assert_eq!(cwd.as_deref(), Some(Path::new("/agent/tools")));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(servers[1].timeout, DEFAULT_TIMEOUT);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with("agent.yaml: MCP server \"both\""));
        assert!(warnings[1].contains("needs `command`"));
        assert_eq!(
            prompt_section(&servers),
            "# MCP servers\n\nTools from these servers are named `<server>__<tool>`:\n- docs (SSE)\n- github (stdio)\n"
        );
    }
}
