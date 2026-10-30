//! A client for MCP servers: connects to each configured server, offers its
//! tools to the model as `<server>__<tool>` and forwards calls.
//!
//! Servers connect at the same time and independently: one that cannot be
//! reached, or does not answer within its timeout (30 s by default, for
//! connecting and listing tools), is left out with a warning. Tool names are
//! cut to the characters and length model APIs accept (`[A-Za-z0-9_-]`, at
//! most 64); a name already taken is left out with a warning. Connections
//! live as long as the [`Servers`] value: drop it to close them.

mod sse;
mod transport;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gitagent_loader::mcp::{McpServerCfg, McpTransport};
use gitagent_provider::{ImageData, ToolSpec};
use serde_json::{json, Map, Value};

use transport::Transport;

/// The protocol version gitagent asks for; servers may answer with another
/// they support.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// What a tool call returned.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOutput {
    pub text: String,
    pub images: Vec<ImageData>,
    pub is_error: bool,
}

struct Client {
    transport: Transport,
    next_id: AtomicU64,
}

impl Client {
    /// Opens the transport and runs the initialize handshake.
    async fn start(cfg: &McpServerCfg) -> Result<Client, String> {
        let transport = Transport::open(&cfg.transport, cfg.timeout).await?;
        let client = Client {
            transport,
            next_id: AtomicU64::new(1),
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "gitagent", "version": env!("CARGO_PKG_VERSION")}
                }),
                cfg.timeout,
            )
            .await?;
        if let Some(v) = init["protocolVersion"].as_str() {
            client.transport.set_protocol_version(v);
        }
        client
            .transport
            .notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        Ok(client)
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer = self.transport.request(&msg, id, timeout).await?;
        if let Some(e) = answer.get("error") {
            let message = e["message"].as_str().unwrap_or("unknown error");
            return Err(match e["code"].as_i64() {
                Some(code) => format!("{message} ({code})"),
                None => message.to_string(),
            });
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Every tool the server offers, following pages to the end.
    async fn list_tools(&self, timeout: Duration) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params, timeout).await?;
            tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
            match page["nextCursor"].as_str() {
                Some(c) if !c.is_empty() && cursor.as_deref() != Some(c) => {
                    cursor = Some(c.to_string())
                }
                _ => return Ok(tools),
            }
        }
    }

    async fn call(
        &self,
        tool: &str,
        args: &Map<String, Value>,
        timeout: Duration,
    ) -> Result<CallOutput, String> {
        let result = self
            .request(
                "tools/call",
                json!({"name": tool, "arguments": args}),
                timeout,
            )
            .await?;
        Ok(flatten(&result))
    }
}

struct Tool {
    spec: ToolSpec,
    server: usize,
    /// The name the server knows it by.
    remote: String,
    /// The server says the tool changes nothing (`readOnlyHint`).
    read_only: bool,
}

/// The connected servers and their tools.
#[derive(Default)]
pub struct Servers {
    clients: Vec<Client>,
    tools: Vec<Tool>,
}

impl Servers {
    /// Connects to every server; `taken` are tool names already in use.
    /// Returns what could be connected and what went wrong.
    pub async fn connect(cfgs: &[McpServerCfg], taken: &[String]) -> (Servers, Vec<String>) {
        let mut warnings = Vec::new();
        let handles: Vec<_> = cfgs
            .iter()
            .cloned()
            .map(|cfg| {
                tokio::spawn(async move {
                    let started = async {
                        let client = Client::start(&cfg).await?;
                        let tools = client.list_tools(cfg.timeout).await?;
                        Ok::<_, String>((client, tools))
                    };
                    tokio::time::timeout(cfg.timeout, started)
                        .await
                        .unwrap_or_else(|_| {
                            Err(format!("no answer within {}s", cfg.timeout.as_secs()))
                        })
                })
            })
            .collect();
        let mut servers = Servers::default();
        let mut names: Vec<String> = taken.to_vec();
        for (cfg, handle) in cfgs.iter().zip(handles) {
            let (client, tools) = match handle.await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    warnings.push(format!(
                        "MCP server {:?} ({}): {e}; left out",
                        cfg.name,
                        cfg.transport.kind()
                    ));
                    continue;
                }
                Err(e) => {
                    warnings.push(format!("MCP server {:?}: {e}; left out", cfg.name));
                    continue;
                }
            };
            let index = servers.clients.len();
            servers.clients.push(client);
            for t in tools {
                let Some(remote) = t["name"].as_str() else {
                    continue;
                };
                let name = tool_name(&cfg.name, remote);
                if names.contains(&name) {
                    warnings.push(format!(
                        "MCP server {:?}: the tool {name:?} is already taken; left out",
                        cfg.name
                    ));
                    continue;
                }
                names.push(name.clone());
                let mut schema = match &t["inputSchema"] {
                    Value::Object(m) => Value::Object(m.clone()),
                    _ => json!({}),
                };
                schema["type"] = json!("object");
                if schema.get("properties").is_none() {
                    schema["properties"] = json!({});
                }
                let description = t["description"]
                    .as_str()
                    .filter(|d| !d.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("MCP tool {remote} from {}", cfg.name));
                servers.tools.push(Tool {
                    spec: ToolSpec {
                        name,
                        description,
                        input_schema: schema,
                    },
                    server: index,
                    remote: remote.to_string(),
                    read_only: t["annotations"]["readOnlyHint"] == Value::Bool(true),
                });
            }
        }
        (servers, warnings)
    }

    /// The tools, in server order and then in the order each server gave.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec.clone()).collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.spec.name == name)
    }

    /// Whether the server marks the tool as changing nothing.
    pub fn read_only(&self, name: &str) -> bool {
        self.tools
            .iter()
            .any(|t| t.spec.name == name && t.read_only)
    }

    pub async fn call(
        &self,
        name: &str,
        args: &Map<String, Value>,
        timeout: Duration,
    ) -> Result<CallOutput, String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.spec.name == name)
            .ok_or_else(|| format!("no MCP tool named {name:?}"))?;
        self.clients[tool.server]
            .call(&tool.remote, args, timeout)
            .await
    }
}

/// Calls one tool on the server at `url` (streamable HTTP) over a connection
/// opened for this call only.
pub async fn call_once(
    url: &str,
    tool: &str,
    args: &Map<String, Value>,
    timeout: Duration,
) -> Result<CallOutput, String> {
    let cfg = McpServerCfg {
        name: tool.to_string(),
        transport: McpTransport::Http {
            url: url.to_string(),
            headers: Default::default(),
        },
        timeout,
    };
    Client::start(&cfg).await?.call(tool, args, timeout).await
}

/// `<server>__<tool>`, with other characters than `[A-Za-z0-9_-]` replaced
/// by `_` and cut to 64 characters.
pub fn tool_name(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// A `tools/call` result as text and images. Audio and binary resources are
/// named rather than included; with no text, `structuredContent` is shown.
fn flatten(result: &Value) -> CallOutput {
    let mut parts: Vec<String> = Vec::new();
    let mut images = Vec::new();
    for block in result["content"].as_array().into_iter().flatten() {
        let s = |k: &str| block[k].as_str().unwrap_or_default();
        match s("type") {
            "text" => parts.push(s("text").to_string()),
            "image" if !s("data").is_empty() => images.push(ImageData {
                media_type: s("mimeType").to_string(),
                data: s("data").to_string(),
            }),
            "image" | "audio" => parts.push(format!(
                "[{}: {}, data omitted]",
                s("type"),
                block["mimeType"].as_str().unwrap_or("unknown")
            )),
            "resource" => {
                let r = &block["resource"];
                match r["text"].as_str() {
                    Some(t) => parts.push(t.to_string()),
                    None => parts.push(format!(
                        "[resource: {} ({})]",
                        r["uri"].as_str().unwrap_or("unknown"),
                        r["mimeType"].as_str().unwrap_or("binary")
                    )),
                }
            }
            "resource_link" => parts.push(format!(
                "[resource_link: {}]",
                block["uri"].as_str().unwrap_or("unknown")
            )),
            _ => {
                if let Some(t) = block["text"].as_str() {
                    parts.push(t.to_string());
                }
            }
        }
    }
    let mut text = parts.join("\n");
    if text.is_empty() {
        if let Some(sc) = result.get("structuredContent").filter(|v| !v.is_null()) {
            text = sc.to_string();
        }
    }
    if text.is_empty() && !images.is_empty() {
        text = format!("{} image(s) returned by the tool.", images.len());
    }
    CallOutput {
        text,
        images,
        is_error: result["isError"].as_bool() == Some(true),
    }
}

#[cfg(test)]
mod tests;
