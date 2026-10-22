//! `gitagent --ipc`: the protocol client libraries use to drive gitagent.
//!
//! One process serves one session. Messages are JSON objects, one per line:
//! the client writes to stdin, gitagent writes to stdout (and only protocol
//! messages); stderr carries logs.
//!
//! 1. The client sends `initialize` with the protocol version, its options,
//!    the callbacks it implements and its own tools; gitagent answers with
//!    `system`/`ready`, then `system`/`session_start`.
//! 2. Each `user` message runs one turn (conversation mode by default: a plain
//!    reply ends it). gitagent streams `delta`, `assistant`, `tool_use` and
//!    `tool_result` messages and ends the turn with `system`/`turn_end`.
//!    Later turns continue the same conversation and shell.
//! 3. During a turn the client may send `steer` (added before the next model
//!    call), `abort` (the turn stops at its next step), `kill` (stop and end
//!    the session) and `callback_response` answers.
//! 4. `end`, or closing stdin, ends the session with `system`/`session_end`.
//!
//! gitagent asks the client with `callback_request` (`kind`: `hook`,
//! `approval` or `tool`) and waits for the `callback_response` with the
//! same id: a hook that does not answer in time blocks (unless registered
//! with `failOpen`), an approval that does not answer is denied, a client
//! tool that does not answer returns an error.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gitagent_core::{
    Approval, ApprovalRequest, Approver, ExternalTools, Outcome, Resume, RunOptions, Steering,
};
use gitagent_hooks::{Decision, ToolHooks};
use gitagent_loader::v1;
use gitagent_provider::{Message, ToolSpec};
use gitagent_record::Events;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::Cli;

const PROTOCOL: u64 = 1;
/// How long a hook or client tool may take to answer by default.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);
/// Approvals wait as long as the agent's compliance settings allow.
const APPROVAL_WAIT: Duration = Duration::from_secs(24 * 3600);
/// How long an aborted turn may take to stop at its next step before it is
/// dropped (its shell is then closed and the conversation starts fresh).
const ABORT_GRACE: Duration = Duration::from_secs(5);

/// Messages to the client, written in order by one task.
#[derive(Clone)]
struct Out(mpsc::UnboundedSender<Value>);

impl Out {
    fn send(&self, v: Value) {
        let _ = self.0.send(v);
    }

    fn system(&self, subtype: &str, content: impl Into<String>, metadata: Option<Value>) {
        let mut m = json!({"type": "system", "subtype": subtype, "content": content.into()});
        if let Some(md) = metadata {
            m["metadata"] = md;
        }
        self.send(m);
    }
}

/// The running turn's controls, for the reader.
#[derive(Clone)]
struct TurnCtl {
    kill: gitagent_core::KillSwitch,
    steering: Steering,
    /// Signalled on abort; the turn is dropped if it has not stopped
    /// [`ABORT_GRACE`] later.
    aborted: Arc<tokio::sync::Notify>,
}

/// What the reader hands the session loop.
enum Cmd {
    User(String),
    End,
}

/// Requests to the client and their answers.
struct Callbacks {
    out: Out,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    next: AtomicU64,
    /// Callback names the client implements, with whether a failure lets
    /// the action go on.
    registered: HashMap<String, bool>,
    timeout: Duration,
    session_id: String,
    agent: String,
    /// Tool names and arguments as gitagent 1.x named them.
    v1_names: bool,
}

impl Callbacks {
    fn has(&self, name: &str) -> bool {
        self.registered.contains_key(name)
    }

    fn fail_open(&self, name: &str) -> bool {
        self.registered.get(name).copied().unwrap_or(false)
    }

    fn send(&self, kind: &str, name: &str, input: Value) -> String {
        let id = format!("c-{}", self.next.fetch_add(1, Ordering::Relaxed));
        self.out.send(json!({
            "type": "callback_request", "id": id, "kind": kind, "name": name, "input": input
        }));
        id
    }

    /// Asks and waits; `None` when no answer came in time.
    async fn request(&self, kind: &str, name: &str, input: Value, wait: Duration) -> Option<Value> {
        let (tx, rx) = oneshot::channel();
        let id = format!("c-{}", self.next.fetch_add(1, Ordering::Relaxed));
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        self.out.send(json!({
            "type": "callback_request", "id": id, "kind": kind, "name": name, "input": input
        }));
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(v)) => Some(v),
            _ => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                None
            }
        }
    }

    fn answer(&self, id: &str, result: Value) {
        let tx = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(tx) = tx {
            let _ = tx.send(result);
        }
    }

    fn tool_name(&self, tool: &str) -> String {
        if self.v1_names {
            v1::v1_name(tool).to_string()
        } else {
            tool.to_string()
        }
    }

    fn args_out(&self, tool: &str, args: &Map<String, Value>) -> Value {
        Value::Object(if self.v1_names {
            v1::to_v1_input(tool, args)
        } else {
            args.clone()
        })
    }

    fn args_in(&self, tool: &str, args: &Map<String, Value>) -> Map<String, Value> {
        if self.v1_names {
            v1::from_v1_input(tool, args)
        } else {
            args.clone()
        }
    }

    fn context(&self, event: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("sessionId".into(), json!(self.session_id));
        m.insert("agentName".into(), json!(self.agent));
        m.insert("event".into(), json!(event));
        m
    }

    /// A lifecycle hook that may block (`onSessionStart`, `preQuery`): the
    /// reason when it blocks.
    async fn blocks(&self, name: &str, event: &str, extra: Map<String, Value>) -> Option<String> {
        if !self.has(name) {
            return None;
        }
        let mut input = self.context(event);
        input.extend(extra);
        match self
            .request("hook", name, Value::Object(input), self.timeout)
            .await
        {
            Some(r) if r["action"] == "block" => Some(
                r["reason"]
                    .as_str()
                    .unwrap_or("no reason given")
                    .to_string(),
            ),
            Some(_) => None,
            None if self.fail_open(name) => None,
            None => Some(format!("the client's {name} hook did not answer")),
        }
    }

    /// A hook gitagent does not wait for (`postResponse`, `onError`).
    fn tell(&self, name: &str, event: &str, extra: Map<String, Value>) {
        if self.has(name) {
            let mut input = self.context(event);
            input.extend(extra);
            self.send("hook", name, Value::Object(input));
        }
    }
}

struct ClientHooks(Arc<Callbacks>);

#[async_trait::async_trait]
impl ToolHooks for ClientHooks {
    async fn pre(&self, tool: &str, arguments: &Map<String, Value>) -> Decision {
        let cb = &self.0;
        if !cb.has("preToolUse") {
            return Decision::Allow;
        }
        let mut input = cb.context("PreToolUse");
        input.insert("toolName".into(), json!(cb.tool_name(tool)));
        input.insert("args".into(), cb.args_out(tool, arguments));
        let Some(r) = cb
            .request("hook", "preToolUse", Value::Object(input), cb.timeout)
            .await
        else {
            return if cb.fail_open("preToolUse") {
                Decision::Allow
            } else {
                Decision::Block {
                    reason: "the client's preToolUse hook did not answer".into(),
                }
            };
        };
        match r["action"].as_str() {
            Some("block") => Decision::Block {
                reason: r["reason"]
                    .as_str()
                    .unwrap_or("no reason given")
                    .to_string(),
            },
            Some("modify") => match r["args"].as_object() {
                Some(args) => Decision::Modify(Value::Object(cb.args_in(tool, args))),
                None => Decision::Allow,
            },
            _ => Decision::Allow,
        }
    }

    async fn post(
        &self,
        tool: &str,
        arguments: &Map<String, Value>,
        output: &str,
        is_error: bool,
    ) -> Decision {
        let cb = &self.0;
        if !cb.has("postToolUse") {
            return Decision::Allow;
        }
        let mut input = cb.context("PostToolUse");
        input.insert("toolName".into(), json!(cb.tool_name(tool)));
        input.insert("args".into(), cb.args_out(tool, arguments));
        input.insert("content".into(), json!(output));
        input.insert("isError".into(), json!(is_error));
        match cb
            .request("hook", "postToolUse", Value::Object(input), cb.timeout)
            .await
        {
            Some(r) if r["action"] == "block" => Decision::Block {
                reason: r["reason"]
                    .as_str()
                    .unwrap_or("no reason given")
                    .to_string(),
            },
            Some(r) if r["action"] == "modify" => match r["content"].as_str() {
                Some(c) => Decision::Modify(json!(c)),
                None => Decision::Allow,
            },
            Some(_) => Decision::Allow,
            None if cb.fail_open("postToolUse") => Decision::Allow,
            None => Decision::Block {
                reason: "the client's postToolUse hook did not answer".into(),
            },
        }
    }
}

struct ClientApprover(Arc<Callbacks>);

#[async_trait::async_trait]
impl Approver for ClientApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Approval {
        let cb = &self.0;
        let input = json!({
            "sessionId": cb.session_id,
            "toolName": cb.tool_name(&request.tool_name),
            "args": cb.args_out(&request.tool_name, &request.arguments),
            "reason": request.reason,
        });
        match cb
            .request("approval", &request.tool_name, input, APPROVAL_WAIT)
            .await
        {
            Some(r) => Approval {
                approved: r["approved"] == Value::Bool(true),
                reason: r["reason"].as_str().map(str::to_string),
            },
            None => Approval::denied("the client did not answer"),
        }
    }
}

struct ClientTools {
    cb: Arc<Callbacks>,
    specs: Vec<ToolSpec>,
}

#[async_trait::async_trait]
impl ExternalTools for ClientTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    async fn call(&self, name: &str, arguments: &Map<String, Value>) -> (String, bool) {
        match self
            .cb
            .request(
                "tool",
                name,
                Value::Object(arguments.clone()),
                self.cb.timeout,
            )
            .await
        {
            Some(r) => {
                let content = match &r["content"] {
                    Value::String(s) => s.clone(),
                    Value::Null => r["text"].as_str().unwrap_or_default().to_string(),
                    other => other.to_string(),
                };
                (content, r["isError"] == Value::Bool(true))
            }
            None => (
                format!(
                    "The client did not answer within {}s.",
                    self.cb.timeout.as_secs()
                ),
                true,
            ),
        }
    }
}

/// Token counts and cost so far.
#[derive(Default, Clone, Copy)]
struct Totals {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
    requests: u64,
}

impl Totals {
    fn json(&self) -> Value {
        json!({
            "inputTokens": self.input,
            "outputTokens": self.output,
            "cacheReadTokens": self.cache_read,
            "cacheWriteTokens": self.cache_write,
            "totalTokens": self.input + self.output,
            "costUsd": (self.cost * 1e6).round() / 1e6,
            "requests": self.requests,
        })
    }
}

/// USD per million tokens, for the cost figures.
#[derive(Clone, Copy)]
struct Prices {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

/// Turns gitagent's events into protocol messages.
struct Stream {
    out: Out,
    cb: Arc<Callbacks>,
    provider: String,
    prices: Option<Prices>,
    totals: Mutex<Totals>,
}

impl Stream {
    fn feed(&self, e: &Value) {
        let kind = e["event"].as_str().unwrap_or_default();
        match kind {
            "model_text" => self.assistant(e),
            "tool_call" => {
                let name = e["name"].as_str().unwrap_or_default();
                let args = e["arguments"].as_object().cloned().unwrap_or_default();
                self.out.send(json!({
                    "type": "tool_use",
                    "toolCallId": e["id"],
                    "toolName": self.cb.tool_name(name),
                    "args": self.cb.args_out(name, &args),
                }));
            }
            "tool_result" => {
                let name = e["name"].as_str().unwrap_or_default();
                let content = e.get("output").unwrap_or(&e["content"]);
                self.out.send(json!({
                    "type": "tool_result",
                    "toolCallId": e["id"],
                    "toolName": self.cb.tool_name(name),
                    "content": content,
                    "isError": e["is_error"],
                }));
            }
            _ => {}
        }
    }

    fn assistant(&self, e: &Value) {
        let text = e["text"].as_str().unwrap_or_default();
        let thinking = e["reasoning"].as_str().unwrap_or_default();
        if !thinking.is_empty() {
            self.out
                .send(json!({"type": "delta", "deltaType": "thinking", "content": thinking}));
        }
        if !text.is_empty() {
            self.out
                .send(json!({"type": "delta", "deltaType": "text", "content": text}));
        }
        let u = &e["usage"];
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        let turn = Totals {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_tokens"),
            cache_write: n("cache_write_tokens"),
            cost: self.prices.map_or(0.0, |p| {
                let uncached = n("input_tokens")
                    .saturating_sub(n("cache_read_tokens"))
                    .saturating_sub(n("cache_write_tokens"));
                (uncached as f64 * p.input
                    + n("cache_read_tokens") as f64 * p.cache_read
                    + n("cache_write_tokens") as f64 * p.cache_write
                    + n("output_tokens") as f64 * p.output)
                    / 1e6
            }),
            requests: 1,
        };
        {
            let mut t = self.totals.lock().unwrap_or_else(|e| e.into_inner());
            t.input += turn.input;
            t.output += turn.output;
            t.cache_read += turn.cache_read;
            t.cache_write += turn.cache_write;
            t.cost += turn.cost;
            t.requests += 1;
        }
        let (stop, error) = match (
            e["tool_calls"].as_u64().unwrap_or(0),
            e["stop_reason"].as_str(),
        ) {
            (n, _) if n > 0 => ("toolUse", None),
            (_, Some("max_tokens")) => ("length", None),
            (_, Some("refusal")) => ("error", Some("the model declined to continue")),
            _ => ("stop", None),
        };
        let served = e["model"].as_str().unwrap_or("unknown");
        let (provider, model) = served
            .split_once(':')
            .unwrap_or((self.provider.as_str(), served));
        let mut usage = turn.json();
        if let Some(o) = usage.as_object_mut() {
            o.remove("requests");
        }
        let mut m = json!({
            "type": "assistant",
            "content": text,
            "model": model,
            "provider": provider,
            "stopReason": stop,
            "usage": usage,
        });
        if !thinking.is_empty() {
            m["thinking"] = json!(thinking);
        }
        if let Some(err) = error {
            m["errorMessage"] = json!(err);
        }
        self.out.send(m);
    }
}

/// The settings `initialize` carries.
struct Init {
    options: Value,
    callbacks: HashMap<String, bool>,
    tools: Vec<ToolSpec>,
}

fn read_init(line: &str) -> Result<Init, String> {
    let v: Value =
        serde_json::from_str(line).map_err(|e| format!("the first message is not JSON: {e}"))?;
    if v["type"] != "initialize" {
        return Err("the first message must be `initialize`".into());
    }
    let protocol = v["protocol"].as_u64().unwrap_or(PROTOCOL);
    if protocol != PROTOCOL {
        return Err(format!(
            "protocol {protocol} is not supported; this gitagent speaks protocol {PROTOCOL}"
        ));
    }
    let mut callbacks = HashMap::new();
    for c in v["callbacks"].as_array().into_iter().flatten() {
        match c {
            Value::String(name) => {
                callbacks.insert(name.clone(), false);
            }
            Value::Object(o) => {
                if let Some(name) = o.get("name").and_then(Value::as_str) {
                    let open = o.get("failOpen").and_then(Value::as_bool).unwrap_or(false);
                    callbacks.insert(name.to_string(), open);
                }
            }
            _ => {}
        }
    }
    let mut tools = Vec::new();
    for t in v["tools"].as_array().into_iter().flatten() {
        let name = t["name"].as_str().ok_or("a client tool has no `name`")?;
        let mut schema = t
            .get("inputSchema")
            .or(t.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        schema["type"] = json!("object");
        if schema.get("properties").is_none() {
            schema["properties"] = json!({});
        }
        tools.push(ToolSpec {
            name: name.to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: schema,
        });
    }
    Ok(Init {
        options: v["options"].clone(),
        callbacks,
        tools,
    })
}

/// Where a session's conversation is kept when `resume` is on.
fn session_file(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || id.starts_with('.')
    {
        return Err(format!(
            "sessionId {id:?} may only use letters, digits, `-`, `_` and `.`"
        ));
    }
    Ok(dir
        .join(".gitagent/sessions")
        .join(id)
        .join("messages.json"))
}

pub(crate) async fn run(cli: Cli) -> ! {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let out = Out(tx);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(v) = rx.recv().await {
            if v.is_null() {
                break;
            }
            let mut line = v.to_string();
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });
    // `session` drops its shells and MCP servers before returning.
    let code = session(cli, out.clone()).await;
    out.send(Value::Null);
    let _ = writer.await;
    // The stdin reader may still be waiting in a blocking read the runtime
    // would wait for (a client may keep its end open after `session_end`).
    std::process::exit(code)
}

async fn session(cli: Cli, out: Out) -> i32 {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let fatal = |out: &Out, why: String| {
        out.system("error", why, None);
        out.system("session_end", "The session could not start", None);
        2
    };
    let first = match lines.next_line().await {
        Ok(Some(l)) => l,
        _ => return fatal(&out, "no `initialize` message arrived".into()),
    };
    let init = match read_init(&first) {
        Ok(i) => i,
        Err(e) => return fatal(&out, e),
    };
    let o = &init.options;
    let s_opt = |k: &str| o[k].as_str().map(str::to_string);

    // The CLI's own setup, with the options applied.
    let mut cfg = cli;
    if let Some(m) = s_opt("model") {
        cfg.model = Some(m);
    }
    if let Some(d) = s_opt("dir").or(s_opt("cwd")) {
        cfg.dir = Some(PathBuf::from(d));
    }
    if let Some(n) = o["maxTurns"].as_u64() {
        cfg.max_turns = Some(n as u32);
    }
    if let Some(ms) = o["timeoutMs"].as_u64() {
        cfg.timeout_s = Some(ms.div_ceil(1000));
    }
    let conversation = o["conversation"].as_bool().unwrap_or(true);
    let mut s = match crate::setup(&cfg, conversation) {
        Ok(s) => s,
        Err(e) => return fatal(&out, format!("{e:#}")),
    };
    for key in ["env", "repo", "sandbox"] {
        if !o[key].is_null() {
            out.system(
                "warning",
                format!("the `{key}` option is not supported in protocol {PROTOCOL}; ignored"),
                None,
            );
        }
    }
    let agent_warnings = s.spec.agent.iter().flat_map(|a| a.warnings.iter());
    for w in agent_warnings.chain(s.spec.warnings.iter()) {
        out.system("warning", w.clone(), None);
    }
    if let Some(p) = o["systemPrompt"].as_str() {
        s.spec.system_prompt = format!("{}\n", p.trim_end());
    }
    if let Some(p) = o["systemPromptSuffix"].as_str() {
        s.spec.system_prompt.push('\n');
        s.spec.system_prompt.push_str(p.trim_end());
        s.spec.system_prompt.push('\n');
    }
    let c = &o["constraints"];
    if let Some(t) = c["temperature"].as_f64() {
        s.spec.profile.temperature = Some(t as f32);
    }
    if let Some(n) = c["maxTokens"].as_u64().or(c["max_tokens"].as_u64()) {
        s.spec.profile.max_output_tokens = n as u32;
    }
    for (name, v) in o["mcpServers"].as_object().into_iter().flatten() {
        match gitagent_loader::mcp::parse(name, v, &s.work_dir) {
            Ok(server) => {
                s.spec.mcp_servers.retain(|x| x.name != *name);
                s.spec.mcp_servers.push(server);
            }
            Err(e) => out.system(
                "warning",
                format!("MCP server {name:?}: {e}; left out"),
                None,
            ),
        }
    }
    s.spec.mcp_servers.sort_by(|a, b| a.name.cmp(&b.name));

    let names = |k: &str| -> Vec<String> {
        o[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let allowed = names("allowedTools");
    let tool_allowlist =
        (!allowed.is_empty()).then(|| gitagent_loader::sdk::normalize_tool_list(&allowed));
    let mut tool_denylist = gitagent_loader::sdk::normalize_tool_list(&names("disallowedTools"));
    if o["replaceBuiltinTools"] == Value::Bool(true) {
        tool_denylist.extend(
            gitagent_tools::specs(&s.spec.profile.tools, s.spec.profile.edit_tool)
                .into_iter()
                .map(|t| t.name),
        );
        tool_denylist.push(gitagent_tools::MEMORY.into());
    }

    let session_id =
        s_opt("sessionId").unwrap_or_else(|| format!("gitagent-{}", std::process::id()));
    let store = if o["resume"] == Value::Bool(true) {
        match session_file(&s.work_dir, &session_id) {
            Ok(p) => Some(p),
            Err(e) => return fatal(&out, e),
        }
    } else {
        None
    };
    let agent_name = s
        .spec
        .agent
        .as_ref()
        .map_or_else(|| s.spec.config.name.clone(), |a| a.manifest.name.clone());
    let cb = Arc::new(Callbacks {
        out: out.clone(),
        pending: Mutex::new(HashMap::new()),
        next: AtomicU64::new(1),
        registered: init.callbacks,
        timeout: o["callbackTimeoutMs"]
            .as_u64()
            .map_or(CALLBACK_TIMEOUT, Duration::from_millis),
        session_id: session_id.clone(),
        agent: agent_name.clone(),
        v1_names: o["toolNames"] == "v1",
    });
    let prices = o["prices"].as_object().map(|p| {
        let f = |k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        Prices {
            input: f("input"),
            output: f("output"),
            cache_read: f("cacheRead"),
            cache_write: f("cacheWrite"),
        }
    });
    let stream = Arc::new(Stream {
        out: out.clone(),
        cb: cb.clone(),
        provider: s.spec.model.provider.clone(),
        prices,
        totals: Mutex::new(Totals::default()),
    });

    let (mcp, warnings) = gitagent_core::connect_mcp(&s.spec).await;
    for w in warnings {
        out.system("warning", w, None);
    }
    let external: Option<Arc<dyn ExternalTools>> = (!init.tools.is_empty()).then(|| {
        Arc::new(ClientTools {
            cb: cb.clone(),
            specs: init.tools.clone(),
        }) as Arc<dyn ExternalTools>
    });
    let tools: Vec<String> = gitagent_core::offered_tools(
        &s.spec,
        mcp.as_deref(),
        external.as_deref(),
        tool_allowlist.as_deref(),
        &tool_denylist,
    )
    .into_iter()
    .map(|t| cb.tool_name(&t.name))
    .collect();
    let manifest = s.spec.agent.as_ref().map(|a| {
        json!({"name": a.manifest.name, "version": a.manifest.version, "description": a.manifest.description})
    });
    out.system(
        "ready",
        "",
        Some(json!({
            "protocol": PROTOCOL,
            "version": env!("CARGO_PKG_VERSION"),
            "sessionId": session_id,
            "agent": manifest,
            "model": s.spec.model.to_string(),
            "tools": tools,
        })),
    );

    // The reader: answers and controls are handled at once, the rest queued.
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let turn: Arc<Mutex<Option<TurnCtl>>> = Arc::default();
    {
        let (cb, out, turn) = (cb.clone(), out.clone(), turn.clone());
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(m) = serde_json::from_str::<Value>(&line) else {
                    out.system("error", "a message was not valid JSON; ignored", None);
                    continue;
                };
                let text = || {
                    m["content"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| m["content"].to_string())
                };
                let running = turn.lock().unwrap_or_else(|e| e.into_inner()).clone();
                match m["type"].as_str().unwrap_or_default() {
                    "callback_response" => {
                        cb.answer(m["id"].as_str().unwrap_or_default(), m["result"].clone())
                    }
                    "user" => {
                        let _ = cmd_tx.send(Cmd::User(text()));
                    }
                    "steer" => match running {
                        Some(t) => t.steering.push(text()),
                        None => {
                            let _ = cmd_tx.send(Cmd::User(text()));
                        }
                    },
                    "abort" => {
                        if let Some(t) = running {
                            t.kill.stop("aborted by the client");
                            t.aborted.notify_one();
                        }
                    }
                    "kill" => {
                        if let Some(t) = running {
                            t.kill.trip();
                            t.aborted.notify_one();
                        }
                        let _ = cmd_tx.send(Cmd::End);
                    }
                    "end" => {
                        let _ = cmd_tx.send(Cmd::End);
                    }
                    other => out.system(
                        "error",
                        format!("unknown message type {other:?}; ignored"),
                        None,
                    ),
                }
            }
            let _ = cmd_tx.send(Cmd::End);
        });
    }

    if let Some(reason) = cb
        .blocks("onSessionStart", "SessionStart", Map::new())
        .await
    {
        out.system(
            "hook_blocked",
            format!("Session blocked by hook: {reason}"),
            None,
        );
        out.system(
            "session_end",
            format!("Agent {agent_name} finished"),
            Some(json!({"sessionId": session_id, "costs": Totals::default().json()})),
        );
        return 0;
    }
    out.system(
        "session_start",
        format!("Agent {agent_name} started"),
        Some(json!({"sessionId": session_id})),
    );

    let mut resume: Option<Resume> = None;
    if let Some(path) = &store {
        if let Ok(text) = std::fs::read_to_string(path) {
            match serde_json::from_str::<Vec<Message>>(&text) {
                Ok(messages) => {
                    resume = Some(Resume {
                        messages,
                        terminal: None,
                    })
                }
                Err(e) => out.system(
                    "warning",
                    format!("the stored conversation could not be read ({e}); starting fresh"),
                    None,
                ),
            }
        }
    }
    let hooks: Option<Arc<dyn ToolHooks>> = (cb.has("preToolUse") || cb.has("postToolUse"))
        .then(|| Arc::new(ClientHooks(cb.clone())) as Arc<dyn ToolHooks>);
    let approver: Option<Arc<dyn Approver>> = cb
        .has("approval")
        .then(|| Arc::new(ClientApprover(cb.clone())) as Arc<dyn Approver>);

    let mut queue: VecDeque<String> = VecDeque::new();
    let mut ending = false;
    let mut n = 0;
    while !ending {
        let content = match queue.pop_front() {
            Some(c) => c,
            None => match cmd_rx.recv().await {
                Some(Cmd::User(c)) => c,
                Some(Cmd::End) | None => break,
            },
        };
        n += 1;
        out.send(json!({"type": "user", "content": content}));
        let mut extra = Map::new();
        extra.insert("prompt".into(), json!(content));
        if let Some(reason) = cb.blocks("preQuery", "PreQuery", extra).await {
            out.system(
                "hook_blocked",
                format!("Query blocked by hook: {reason}"),
                None,
            );
            out.system(
                "turn_end",
                "",
                Some(json!({"outcome": "hook_blocked", "turns": 0})),
            );
            continue;
        }
        let ctl = TurnCtl {
            kill: gitagent_core::KillSwitch::default(),
            steering: Steering::default(),
            aborted: Arc::new(tokio::sync::Notify::new()),
        };
        *turn.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctl.clone());
        let events = {
            let stream = stream.clone();
            Events::to_callback(move |e| stream.feed(&e))
        };
        let before = *stream.totals.lock().unwrap_or_else(|e| e.into_inner());
        let opts = RunOptions {
            instruction: content,
            work_dir: s.work_dir.clone(),
            // One directory per session, so sessions running side by side
            // keep their records apart.
            out_dir: cfg.out_dir.join(&session_id).join(format!("turn-{n}")),
            timeout: Some(s.timeout),
            session_id: Some(session_id.clone()),
            events,
            resume: resume.take(),
            sandbox: s.sandbox.clone(),
            tool_allowlist: tool_allowlist.clone(),
            tool_denylist: tool_denylist.clone(),
            hook_callbacks: hooks.clone(),
            mcp: mcp.clone(),
            approver: approver.clone(),
            kill: Some(ctl.kill.clone()),
            steer: Some(ctl.steering.clone()),
            external_tools: external.clone(),
        };
        let run = gitagent_core::run(&s.spec, s.provider.as_ref(), opts);
        tokio::pin!(run);
        let result = loop {
            tokio::select! {
                r = &mut run => break Some(r),
                cmd = cmd_rx.recv(), if !ending => match cmd {
                    Some(Cmd::User(c)) => queue.push_back(c),
                    Some(Cmd::End) | None => {
                        ending = true;
                        ctl.kill.stop("the session was ended");
                        ctl.aborted.notify_one();
                    }
                },
                _ = async { ctl.aborted.notified().await; tokio::time::sleep(ABORT_GRACE).await } => {
                    break None;
                }
            }
        };
        *turn.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let after = *stream.totals.lock().unwrap_or_else(|e| e.into_inner());
        let turn_usage = Totals {
            input: after.input - before.input,
            output: after.output - before.output,
            cache_read: after.cache_read - before.cache_read,
            cache_write: after.cache_write - before.cache_write,
            cost: after.cost - before.cost,
            requests: after.requests - before.requests,
        };
        match result {
            None => {
                out.system(
                    "turn_end",
                    "",
                    Some(json!({
                        "outcome": "aborted",
                        "usage": turn_usage.json(),
                        "note": "the turn did not stop in time and was dropped; the next message starts a fresh conversation",
                    })),
                );
            }
            Some(Err(e)) => {
                let message = format!("{e:#}");
                out.system("error", message.clone(), None);
                let mut extra = Map::new();
                extra.insert("error".into(), json!(message));
                cb.tell("onError", "OnError", extra);
                out.system(
                    "turn_end",
                    "",
                    Some(json!({"outcome": "error", "usage": turn_usage.json()})),
                );
            }
            Some(Ok(finished)) => {
                let r = &finished.report;
                match &r.outcome {
                    Outcome::ProviderFailed(e) => {
                        out.system(
                            "error",
                            e.clone(),
                            Some(json!({"model": s.spec.model.to_string()})),
                        );
                        let mut extra = Map::new();
                        extra.insert("error".into(), json!(e));
                        cb.tell("onError", "OnError", extra);
                    }
                    Outcome::HookBlocked(reason) => {
                        out.system(
                            "hook_blocked",
                            format!("Query blocked by hook: {reason}"),
                            None,
                        );
                    }
                    Outcome::Completed => cb.tell("postResponse", "PostResponse", Map::new()),
                    _ => {}
                }
                out.system(
                    "turn_end",
                    "",
                    Some(json!({
                        "outcome": r.outcome.to_string(),
                        "turns": r.turns,
                        "result": r.summary,
                        "usage": turn_usage.json(),
                        "costUsd": (turn_usage.cost * 1e6).round() / 1e6,
                    })),
                );
                if let Some(path) = &store {
                    if let Err(e) = save(path, &finished.messages) {
                        out.system(
                            "warning",
                            format!("the conversation could not be stored: {e}"),
                            None,
                        );
                    }
                }
                resume = Some(finished.into_resume());
            }
        }
    }

    gitagent_core::end_session(&s.spec, &s.work_dir, &session_id).await;
    let totals = *stream.totals.lock().unwrap_or_else(|e| e.into_inner());
    out.system(
        "session_end",
        format!("Agent {agent_name} finished"),
        Some(json!({"sessionId": session_id, "costs": totals.json()})),
    );
    0
}

fn save(path: &Path, messages: &[Message]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(messages)?)?;
    std::fs::rename(tmp, path)
}
