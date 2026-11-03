//! How JSON-RPC messages reach a server: a child process's stdin and stdout
//! (one message per line), streamable HTTP (one POST per message, answered
//! with JSON or an event stream), or the older HTTP+SSE transport (answers
//! arrive on a long-lived event stream, messages go to the endpoint it names).

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gitagent_loader::declared::expand_env;
use gitagent_loader::mcp::McpTransport;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::sse::SseParser;

/// Environment variables a stdio server inherits; everything else it gets
/// from its `env` setting, so API keys in gitagent's environment stay there.
const INHERITED_ENV: &[&str] = &[
    "HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER", "LANG", "TMPDIR",
];

/// The last bytes of a server's stderr kept for error messages.
const STDERR_TAIL: usize = 2000;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub(crate) enum Transport {
    Stdio(StdioConn),
    Http(HttpConn),
    Sse(SseConn),
}

impl Transport {
    pub async fn open(cfg: &McpTransport, timeout: Duration) -> Result<Transport, String> {
        match cfg {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => StdioConn::spawn(command, args, env, cwd.as_deref()).map(Transport::Stdio),
            McpTransport::Http { url, headers } => {
                Ok(Transport::Http(HttpConn::new(url, headers)?))
            }
            McpTransport::Sse { url, headers } => SseConn::connect(url, headers, timeout)
                .await
                .map(Transport::Sse),
        }
    }

    /// Sends a request and waits for the answer with the same id.
    pub async fn request(&self, msg: &Value, id: u64, timeout: Duration) -> Result<Value, String> {
        let answer = match self {
            Transport::Stdio(c) => c.request(msg, id, timeout).await,
            Transport::Http(c) => c.request(msg, id, timeout).await,
            Transport::Sse(c) => c.request(msg, id, timeout).await,
        };
        answer.map_err(|e| match self {
            Transport::Stdio(c) => c.with_stderr(e),
            _ => e,
        })
    }

    pub async fn notify(&self, msg: &Value) -> Result<(), String> {
        match self {
            Transport::Stdio(c) => c.write(msg).await,
            Transport::Http(c) => c.post(msg, Duration::from_secs(30)).await.map(|_| ()),
            Transport::Sse(c) => c.post(msg).await,
        }
    }

    /// Tells the transport the negotiated protocol version (HTTP sends it
    /// with every later request).
    pub fn set_protocol_version(&self, version: &str) {
        if let Transport::Http(c) = self {
            *c.version.lock().unwrap_or_else(|e| e.into_inner()) = Some(version.to_string());
        }
    }
}

async fn wait(
    rx: oneshot::Receiver<Value>,
    pending: &Pending,
    id: u64,
    timeout: Duration,
) -> Result<Value, String> {
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err("the server closed the connection".into()),
        Err(_) => {
            pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            Err(format!("no answer within {}s", timeout.as_secs()))
        }
    }
}

/// Hands an incoming message to whoever waits for it; answers a server's
/// own requests (only `ping` is understood) through `reply`.
fn dispatch(v: Value, pending: &Pending) -> Option<Value> {
    let id = v.get("id").cloned();
    if v.get("method").is_some() {
        // A request from the server needs an answer; a notification does not.
        let id = id?;
        return Some(if v["method"] == "ping" {
            json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
        });
    }
    if let Some(n) = id.as_ref().and_then(Value::as_u64) {
        if let Some(tx) = pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&n) {
            let _ = tx.send(v);
        }
    }
    None
}

pub(crate) struct StdioConn {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Pending,
    stderr: Arc<Mutex<String>>,
    _child: Child,
}

impl StdioConn {
    fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&std::path::Path>,
    ) -> Result<StdioConn, String> {
        let mut cmd = Command::new(expand_env(command));
        cmd.args(args.iter().map(|a| expand_env(a)))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in INHERITED_ENV {
            if let Some(v) = std::env::var_os(name) {
                cmd.env(name, v);
            }
        }
        for (k, v) in env {
            cmd.env(k, expand_env(v));
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {command}: {e}"))?;
        let stdin = Arc::new(tokio::sync::Mutex::new(
            child.stdin.take().ok_or("no stdin")?,
        ));
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr")?;
        let pending: Pending = Arc::default();
        let stderr: Arc<Mutex<String>> = Arc::default();

        let (p, w) = (pending.clone(), stdin.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(reply) = dispatch(v, &p) {
                    let mut w = w.lock().await;
                    let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                    let _ = w.flush().await;
                }
            }
            // Waiting requests fail at once instead of timing out.
            p.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        let tail = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                if t.len() > STDERR_TAIL {
                    let cut = t.len() - STDERR_TAIL;
                    let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                    t.drain(..cut);
                }
            }
        });
        Ok(StdioConn {
            stdin,
            pending,
            stderr,
            _child: child,
        })
    }

    async fn write(&self, msg: &Value) -> Result<(), String> {
        let mut w = self.stdin.lock().await;
        w.write_all(format!("{msg}\n").as_bytes())
            .await
            .map_err(|e| format!("writing to the server: {e}"))?;
        w.flush().await.map_err(|e| e.to_string())
    }

    async fn request(&self, msg: &Value, id: u64, timeout: Duration) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        if let Err(e) = self.write(msg).await {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }
        wait(rx, &self.pending, id, timeout).await
    }

    fn with_stderr(&self, error: String) -> String {
        let tail = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        let tail = tail.trim();
        if tail.is_empty() {
            error
        } else {
            format!("{error}; its stderr ends with: {tail}")
        }
    }
}

fn header_list(headers: &BTreeMap<String, String>) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| (k.clone(), expand_env(v)))
        .collect()
}

pub(crate) struct HttpConn {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
}

impl HttpConn {
    fn new(url: &str, headers: &BTreeMap<String, String>) -> Result<HttpConn, String> {
        Ok(HttpConn {
            client: reqwest::Client::builder()
                .build()
                .map_err(|e| e.to_string())?,
            url: expand_env(url),
            headers: header_list(headers),
            session: Mutex::new(None),
            version: Mutex::new(None),
        })
    }

    async fn post(&self, msg: &Value, timeout: Duration) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(msg.to_string());
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some(s) = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            req = req.header("mcp-session-id", s);
        }
        if let Some(v) = self
            .version
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            req = req.header("mcp-protocol-version", v);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.to_string());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {status}: {}", body.trim()));
        }
        Ok(resp)
    }

    async fn request(&self, msg: &Value, id: u64, timeout: Duration) -> Result<Value, String> {
        let answer = async {
            let mut resp = self.post(msg, timeout).await?;
            let is_stream = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("text/event-stream"));
            let matches = |v: &Value| v.get("id").and_then(Value::as_u64) == Some(id);
            if !is_stream {
                let text = resp.text().await.map_err(|e| e.to_string())?;
                let v: Value = serde_json::from_str(&text)
                    .map_err(|e| format!("the answer is not JSON: {e}"))?;
                return match v {
                    Value::Array(batch) => batch
                        .into_iter()
                        .find(matches)
                        .ok_or_else(|| "the answer has no matching id".to_string()),
                    v => Ok(v),
                };
            }
            let mut parser = SseParser::default();
            while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
                for event in parser.push(&chunk) {
                    if let Ok(v) = serde_json::from_str::<Value>(&event.data) {
                        if matches(&v) && v.get("method").is_none() {
                            return Ok(v);
                        }
                    }
                }
            }
            Err("the event stream ended without an answer".to_string())
        };
        tokio::time::timeout(timeout, answer)
            .await
            .unwrap_or_else(|_| Err(format!("no answer within {}s", timeout.as_secs())))
    }
}

pub(crate) struct SseConn {
    client: reqwest::Client,
    endpoint: String,
    headers: Vec<(String, String)>,
    pending: Pending,
    reader: JoinHandle<()>,
}

impl Drop for SseConn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl SseConn {
    async fn connect(
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<SseConn, String> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| e.to_string())?;
        let url = expand_env(url);
        let headers = header_list(headers);
        let mut req = client.get(&url).header("accept", "text/event-stream");
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        let opened = async {
            let mut resp = req.send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let mut parser = SseParser::default();
            while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
                // Only the endpoint can come before the first request.
                if let Some(e) = parser
                    .push(&chunk)
                    .into_iter()
                    .find(|e| e.name == "endpoint")
                {
                    return Ok((resp, parser, e.data));
                }
            }
            Err("the event stream ended before naming its endpoint".to_string())
        };
        let (mut resp, mut parser, endpoint) = tokio::time::timeout(timeout, opened)
            .await
            .unwrap_or_else(|_| Err(format!("no endpoint within {}s", timeout.as_secs())))?;
        let endpoint = reqwest::Url::parse(&url)
            .and_then(|base| base.join(endpoint.trim()))
            .map_err(|e| format!("bad endpoint {endpoint:?}: {e}"))?
            .to_string();
        let pending: Pending = Arc::default();
        let p = pending.clone();
        let reader = tokio::spawn(async move {
            while let Ok(Some(chunk)) = resp.chunk().await {
                for event in parser.push(&chunk) {
                    if let Ok(v) = serde_json::from_str::<Value>(&event.data) {
                        // Requests from the server are not answered here: the
                        // older transport has no way to tell them apart from
                        // ours, and servers do not need them for tools.
                        dispatch(v, &p);
                    }
                }
            }
            p.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        Ok(SseConn {
            client,
            endpoint,
            headers,
            pending,
            reader,
        })
    }

    async fn post(&self, msg: &Value) -> Result<(), String> {
        let mut req = self
            .client
            .post(&self.endpoint)
            .timeout(Duration::from_secs(30))
            .header("content-type", "application/json")
            .body(msg.to_string());
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", resp.status()))
        }
    }

    async fn request(&self, msg: &Value, id: u64, timeout: Duration) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        if let Err(e) = self.post(msg).await {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }
        wait(rx, &self.pending, id, timeout).await
    }
}
