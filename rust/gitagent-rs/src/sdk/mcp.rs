//! MCP client (stdio transport) — a self-contained JSON-RPC 2.0 implementation
//! of the Model Context Protocol handshake (`initialize` → `notifications/
//! initialized` → `tools/list`), surfacing each server's tools as namespaced
//! `AgentTool`s (`mcp__<server>__<tool>`). Tool calls go through `tools/call`.
//!
//! Servers are declared in `mcp.yaml`:
//! ```yaml
//! servers:
//!   - name: fs
//!     command: "npx -y @modelcontextprotocol/server-filesystem ."
//! ```
//! The connection is a long-lived child process reused across calls. Blocking
//! stdio runs inside `spawn_blocking`, so it never stalls the async runtime.
//! A server that fails to start is skipped (its tools simply don't appear).
//!
//! Every request has a deadline (`GITAGENT_MCP_TIMEOUT_SECS`, default 120s;
//! the connect handshake uses `GITAGENT_MCP_CONNECT_TIMEOUT_SECS`, default
//! 30s). A server that misses it is killed (with its process group) and
//! respawned on the next call, so one hung call can't block the server's
//! later calls forever.

use crate::pi::message::ToolResult;
use crate::pi::tool::{AgentTool, ExecutionMode};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crate::sdk::proc::{kill_group, new_group_std};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct McpFile {
    #[serde(default)]
    servers: Vec<ServerSpec>,
}

#[derive(Deserialize)]
struct ServerSpec {
    name: String,
    /// Shell command that launches the server (spawned via `sh -c`).
    command: String,
}

fn env_secs(var: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var).ok().and_then(|s| s.parse::<u64>().ok()).filter(|n| *n > 0).unwrap_or(default),
    )
}

fn call_timeout() -> Duration {
    env_secs("GITAGENT_MCP_TIMEOUT_SECS", 120)
}

fn connect_timeout() -> Duration {
    env_secs("GITAGENT_MCP_CONNECT_TIMEOUT_SECS", 30)
}

/// A live stdio connection to one MCP server.
pub struct McpConn {
    child: Child,
    stdin: ChildStdin,
    /// Lines from the server's stdout, read by a dedicated thread so every
    /// request can wait with a deadline.
    lines: Receiver<String>,
    next_id: u64,
}

impl Drop for McpConn {
    fn drop(&mut self) {
        kill_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpConn {
    fn spawn(command: &str, cwd: &Path) -> Result<Child> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        new_group_std(&mut cmd);
        Ok(cmd.spawn()?)
    }

    /// Connect + handshake + list tools. Returns the connection and its tools.
    fn connect(command: &str, cwd: &Path) -> Result<(McpConn, Vec<ToolDef>)> {
        let mut child = Self::spawn(command, cwd)?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // EOF → the channel closes
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut conn = McpConn { child, stdin, lines, next_id: 1 };
        let timeout = connect_timeout();

        conn.request_with_timeout(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "gitagent", "version": "0.1" }
            }),
            timeout,
        )?;
        conn.notify("notifications/initialized", json!({}))?;

        let listed = conn.request_with_timeout("tools/list", json!({}), timeout)?;
        let tools = listed
            .get("tools")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        Some(ToolDef {
                            name: t.get("name")?.as_str()?.to_string(),
                            description: t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                            schema: t
                                .get("inputSchema")
                                .cloned()
                                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((conn, tools))
    }

    fn write_message(&mut self, msg: &Value) -> Result<()> {
        self.stdin.write_all(msg.to_string().as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Send a request and return its `result` (skipping unrelated
    /// notifications). Errors with "timed out" if no answer arrives in time.
    fn request_with_timeout(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;

        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(anyhow!("MCP request '{method}' timed out after {}s", timeout.as_secs()))
                }
                Err(RecvTimeoutError::Disconnected) => return Err(anyhow!("MCP server closed the connection")),
            };
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
            // Skip notifications / responses to other ids.
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(anyhow!("MCP error: {err}"));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn call_tool(&mut self, name: &str, args: Value, timeout: Duration) -> Result<String> {
        let res = self.request_with_timeout("tools/call", json!({ "name": name, "arguments": args }), timeout)?;
        // result.content is an array of typed parts; concatenate the text parts.
        let text = res
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Ok(if text.is_empty() { res.to_string() } else { text })
    }
}

struct ToolDef {
    name: String,
    description: String,
    schema: Value,
}

pub struct McpTool {
    /// Namespaced tool name exposed to the model: `mcp__<server>__<tool>`.
    display_name: String,
    /// The raw tool name the server expects in `tools/call`.
    raw_name: String,
    description: String,
    schema: Value,
    server: Arc<Server>,
}

/// One configured server: its live connection (None after a failure) plus
/// what's needed to respawn it.
struct Server {
    command: String,
    cwd: PathBuf,
    conn: Mutex<Option<McpConn>>,
}

impl Server {
    /// Call a tool, (re)connecting if the previous connection died. Any error
    /// drops the connection (killing the server), so the next call starts
    /// from a fresh process instead of a wedged one.
    fn call(&self, raw: &str, args: Value) -> Result<String> {
        let mut slot = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let (conn, _) = McpConn::connect(&self.command, &self.cwd)?;
            *slot = Some(conn);
        }
        let res = slot.as_mut().expect("connected").call_tool(raw, args, call_timeout());
        if res.is_err() {
            *slot = None; // Drop kills the process group
        }
        res
    }
}

#[async_trait]
impl AgentTool for McpTool {
    fn name(&self) -> &str {
        &self.display_name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    // Network + side effects → serialize (never race in a parallel batch).
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }
    async fn execute(&self, _id: &str, args: Value, cancel: &CancellationToken) -> Result<ToolResult> {
        let server = self.server.clone();
        let raw = self.raw_name.clone();
        // Blocking stdio JSON-RPC runs off the async runtime; it is bounded by
        // the per-request timeout, so the lock is never held indefinitely.
        let call = tokio::task::spawn_blocking(move || server.call(&raw, args));
        tokio::pin!(call);
        let joined = tokio::select! {
            _ = cancel.cancelled() => return Ok(ToolResult::text("(aborted)")),
            r = &mut call => r,
        };
        match joined {
            Ok(Ok(text)) => Ok(ToolResult::text(text)),
            Ok(Err(e)) => Ok(ToolResult::text(format!("Error: {e}"))),
            Err(e) => Ok(ToolResult::text(format!("Error: {e}"))),
        }
    }
}

/// Connect every server in `mcp.yaml` and return their tools. Servers that fail
/// to start are skipped with a stderr note (they just don't contribute tools).
pub fn load_mcp_tools(dir: &Path) -> Vec<Arc<dyn AgentTool>> {
    let Ok(raw) = std::fs::read_to_string(dir.join("mcp.yaml")) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_yaml::from_str::<McpFile>(&raw) else {
        return Vec::new();
    };

    let mut out: Vec<Arc<dyn AgentTool>> = Vec::new();
    for server in parsed.servers {
        match McpConn::connect(&server.command, dir) {
            Ok((conn, defs)) => {
                let shared = Arc::new(Server {
                    command: server.command.clone(),
                    cwd: dir.to_path_buf(),
                    conn: Mutex::new(Some(conn)),
                });
                for d in defs {
                    out.push(Arc::new(McpTool {
                        display_name: format!("mcp__{}__{}", server.name, d.name),
                        raw_name: d.name,
                        description: d.description,
                        schema: d.schema,
                        server: shared.clone(),
                    }));
                }
            }
            Err(e) => eprintln!("\x1b[33mmcp: server '{}' failed to start: {e}\x1b[0m", server.name),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hung_call_times_out_and_the_server_is_respawned() {
        std::env::set_var("GITAGENT_MCP_TIMEOUT_SECS", "1");
        // Answers the handshake, then never answers tools/call.
        let script = r#"read l; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read l; read l; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"t"}]}}'; sleep 30"#;
        let dir = std::env::temp_dir();
        let (conn, defs) = McpConn::connect(script, &dir).unwrap();
        assert_eq!(defs.len(), 1);
        let server = Server { command: script.to_string(), cwd: dir, conn: Mutex::new(Some(conn)) };
        let t = Instant::now();
        let e = server.call("t", json!({})).unwrap_err();
        assert!(e.to_string().contains("timed out"), "{e}");
        assert!(server.conn.lock().unwrap().is_none(), "dead connection dropped");
        // The next call reconnects (fresh process) instead of blocking forever.
        assert!(server.call("t", json!({})).unwrap_err().to_string().contains("timed out"));
        assert!(t.elapsed() < Duration::from_secs(10));
    }
}
