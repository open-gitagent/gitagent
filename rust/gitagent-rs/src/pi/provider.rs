//! Streaming provider (OpenAI-compatible). Replaces pi-ai's `streamSimple` for
//! the OpenAI/gateway API. Emits text deltas as they arrive and returns the
//! assembled AssistantMessage. Like pi, provider/network errors are encoded as
//! `stop_reason = Error` (not thrown), so the loop stops cleanly.

use crate::pi::event::{AgentEvent, DeltaKind};
use crate::pi::message::{AssistantMessage, ContentBlock, StopReason, Usage};
use crate::pi::message::AgentMessage;
use crate::pi::tool::AgentTool;
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ModelSpec {
    pub model_id: String,
    pub base_url: String,
    pub api_key: String,
}

/// Sampling parameters forwarded to the provider (only serialized when set).
#[derive(Clone, Default)]
pub struct GenParams {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub top_p: Option<f64>,
}

/// Best-effort context window (tokens) for a model id — sizes compaction to the
/// real model instead of a flat guess. Unknown → 128k (a safe common default).
pub fn model_context_window(model_id: &str) -> usize {
    let m = model_id.to_lowercase();
    let has = |n: &str| m.contains(n);
    if has("gemini") {
        1_000_000
    } else if has("claude") || has("sonnet") || has("opus") || has("haiku") {
        200_000
    } else if has("o1") || has("o3") || has("o4") || has("gpt-5") {
        200_000
    } else if has("gpt-4.1") {
        1_000_000
    } else {
        128_000
    }
}

/// Best-effort (input, output) USD per million tokens. Unknown models → (0, 0).
/// Lets us compute cost from token counts even when a gateway reports $0 (G24).
pub fn model_cost_per_mtok(model_id: &str) -> (f64, f64) {
    let m = model_id.to_lowercase();
    let has = |n: &str| m.contains(n);
    if has("gpt-4o-mini") {
        (0.15, 0.60)
    } else if has("gpt-4o") {
        (2.50, 10.0)
    } else if has("gpt-4.1-mini") {
        (0.40, 1.60)
    } else if has("gpt-4.1") {
        (2.0, 8.0)
    } else if has("o3-mini") || has("o4-mini") {
        (1.10, 4.40)
    } else if has("haiku") {
        (0.80, 4.0)
    } else if has("sonnet") {
        (3.0, 15.0)
    } else if has("opus") {
        (15.0, 75.0)
    } else {
        (0.0, 0.0)
    }
}

/// Pop one complete `\n`-terminated line off the front of a byte buffer and
/// decode it. Returns None (leaving the bytes buffered) until a newline arrives.
fn take_line(buf: &mut Vec<u8>) -> Option<String> {
    let nl = buf.iter().position(|&b| b == b'\n')?;
    let line: Vec<u8> = buf.drain(..=nl).collect();
    Some(String::from_utf8_lossy(&line).into_owned())
}

pub fn resolve_model(spec: &str) -> ModelSpec {
    // "provider:model[@baseurl]" — the provider prefix selects a default endpoint.
    let (provider, rest) = spec.split_once(':').unwrap_or(("openai", spec));
    let (model_id, base_from_at) = match rest.split_once('@') {
        Some((m, b)) => (m.to_string(), Some(b.to_string())),
        None => (rest.to_string(), None),
    };
    // Per-provider default endpoint (overridden by @baseurl or GITAGENT_MODEL_BASE_URL).
    let default_base = match provider {
        "ollama" => "http://localhost:11434/v1", // local models, no key needed
        "anthropic" => "https://api.anthropic.com/v1",
        _ => "https://api.openai.com/v1",
    };
    let env_base = std::env::var("GITAGENT_MODEL_BASE_URL").ok().filter(|s| !s.trim().is_empty());
    let base_url = base_from_at
        .or_else(|| env_base.clone())
        .unwrap_or_else(|| default_base.to_string());
    let api_key = provider_api_key(provider, &base_url, env_base.as_deref());
    ModelSpec { model_id, base_url, api_key }
}

/// The env var holding a provider's key and the official API host that key
/// belongs to. `None` = the provider takes no key (local Ollama).
fn provider_key_source(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "ollama" => None,
        "anthropic" => Some(("ANTHROPIC_API_KEY", "api.anthropic.com")),
        // Everything else speaks the OpenAI wire format against api.openai.com.
        _ => Some(("OPENAI_API_KEY", "api.openai.com")),
    }
}

/// Whether a provider's key may be sent to `base_url`: only to the provider's
/// official HTTPS host, or to the endpoint the user configured explicitly via
/// `GITAGENT_MODEL_BASE_URL`. A `@baseurl` in a model string never receives a
/// key on its own — model strings can come from untrusted agent repos.
pub fn key_allowed_for(provider: &str, base_url: &str, user_base: Option<&str>) -> bool {
    let Some((_, official_host)) = provider_key_source(provider) else {
        return false;
    };
    if let Some(ub) = user_base {
        if ub.trim_end_matches('/') == base_url.trim_end_matches('/') {
            return true;
        }
    }
    match reqwest::Url::parse(base_url) {
        Ok(u) => u.scheme() == "https" && u.host_str() == Some(official_host),
        Err(_) => false,
    }
}

/// One key per provider (no cross-provider fallback), attached only when
/// [`key_allowed_for`] approves the destination.
fn provider_api_key(provider: &str, base_url: &str, user_base: Option<&str>) -> String {
    let Some((var, _)) = provider_key_source(provider) else {
        return String::new();
    };
    if !key_allowed_for(provider, base_url, user_base) {
        return String::new();
    }
    std::env::var(var).unwrap_or_default()
}

/// Whether a `@baseurl` points at this machine (loopback). Model strings that
/// arrive over the local HTTP API may only override the endpoint with these.
pub fn is_loopback_base_url(base_url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(base_url) else { return false };
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    let Some(host) = u.host_str() else { return false };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallWire>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct ToolCallWire {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionWire,
}

#[derive(Serialize)]
struct FunctionWire {
    name: String,
    arguments: String,
}

/// convert_to_llm: AgentMessage[] → OpenAI chat messages.
fn to_chat_messages(system: &str, messages: &[AgentMessage]) -> Vec<ChatMessage> {
    let mut out = vec![ChatMessage {
        role: "system".into(),
        content: Some(system.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];
    for m in messages {
        match m {
            AgentMessage::User(s) => out.push(ChatMessage {
                role: "user".into(),
                content: Some(s.clone()),
                tool_calls: None,
                tool_call_id: None,
            }),
            AgentMessage::Assistant(a) => {
                let text = a.text();
                let tcs: Vec<ToolCallWire> = a
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall { id, name, arguments } => Some(ToolCallWire {
                            id: id.clone(),
                            kind: "function".into(),
                            function: FunctionWire { name: name.clone(), arguments: arguments.to_string() },
                        }),
                        _ => None,
                    })
                    .collect();
                out.push(ChatMessage {
                    // content must ALWAYS be a string: Ollama rejects a missing
                    // content field with `invalid message content type: <nil>`
                    // (empty assistant turns happen, e.g. thinking-only cuts).
                    role: "assistant".into(),
                    content: Some(text),
                    tool_calls: (!tcs.is_empty()).then_some(tcs),
                    tool_call_id: None,
                });
            }
            AgentMessage::ToolResult(t) => out.push(ChatMessage {
                role: "tool".into(),
                content: Some(t.content.clone()),
                tool_calls: None,
                tool_call_id: Some(t.tool_call_id.clone()),
            }),
        }
    }
    out
}

/// Whether a provider error looks transient (worth retrying / falling back).
pub fn is_transient_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "429", "500", "502", "503", "504", "overloaded", "rate limit", "timeout", "timed out",
        "connection", "connect", "reset", "temporarily", "unavailable", "dns", "broken pipe",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// `stream_assistant` with provider fallback + bounded retry/backoff. Tries each
/// spec in order; within a spec, retries transient failures with exponential
/// backoff. Returns the first non-error result, or the last error. Transient
/// errors almost always occur at request time (before any delta is streamed), so
/// a retry does not duplicate visible output.
#[allow(clippy::too_many_arguments)]
pub async fn stream_assistant_resilient(
    client: &reqwest::Client,
    specs: &[ModelSpec],
    system: &str,
    messages: &[AgentMessage],
    tools: &[Arc<dyn AgentTool>],
    params: &GenParams,
    max_retries: u32,
    cancel: &CancellationToken,
    tx: &Sender<AgentEvent>,
) -> AssistantMessage {
    let mut last = AssistantMessage::failure(StopReason::Error, "no model configured");
    for spec in specs {
        let mut attempt = 0u32;
        loop {
            if cancel.is_cancelled() {
                return AssistantMessage::failure(StopReason::Aborted, "aborted");
            }
            let msg = stream_assistant(client, spec, system, messages, tools, params, cancel, tx).await;
            if msg.stop_reason != StopReason::Error {
                return msg; // success / tool-use / stop / aborted
            }
            let transient = msg.error_message.as_deref().map(is_transient_error).unwrap_or(false);
            last = msg;
            if transient && attempt < max_retries {
                let backoff = Duration::from_millis(400u64 * (1u64 << attempt.min(5)));
                tokio::select! {
                    _ = cancel.cancelled() => return AssistantMessage::failure(StopReason::Aborted, "aborted"),
                    _ = tokio::time::sleep(backoff) => {}
                }
                attempt += 1;
                continue; // retry the same spec
            }
            break; // non-transient or retries exhausted → next fallback spec
        }
    }
    last
}

#[allow(clippy::too_many_arguments)]
pub async fn stream_assistant(
    client: &reqwest::Client,
    spec: &ModelSpec,
    system: &str,
    messages: &[AgentMessage],
    tools: &[Arc<dyn AgentTool>],
    params: &GenParams,
    cancel: &CancellationToken,
    tx: &Sender<AgentEvent>,
) -> AssistantMessage {
    let chat = to_chat_messages(system, messages);
    let tool_schemas: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({ "type": "function", "function": {
                "name": t.name(), "description": t.description(), "parameters": t.parameters()
            }})
        })
        .collect();

    match stream_inner(client, spec, &chat, &tool_schemas, params, cancel, tx).await {
        Ok(msg) => msg,
        // Some local models can't accept a tools param at all — degrade to a
        // plain chat rather than failing the turn.
        Err(e) if e.to_string().contains("does not support tools") && !tool_schemas.is_empty() => {
            match stream_inner(client, spec, &chat, &[], params, cancel, tx).await {
                Ok(msg) => msg,
                Err(e2) => AssistantMessage::failure(StopReason::Error, e2.to_string()),
            }
        }
        Err(e) => AssistantMessage::failure(StopReason::Error, e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_inner(
    client: &reqwest::Client,
    spec: &ModelSpec,
    chat: &[ChatMessage],
    tools: &[Value],
    params: &GenParams,
    cancel: &CancellationToken,
    tx: &Sender<AgentEvent>,
) -> Result<AssistantMessage> {
    let mut body = json!({ "model": spec.model_id, "messages": chat, "stream": true });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    if let Some(t) = params.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(mt) = params.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    if let Some(tp) = params.top_p {
        body["top_p"] = json!(tp);
    }

    let url = format!("{}/chat/completions", spec.base_url.trim_end_matches('/'));
    let mut req = client.post(&url);
    if !spec.api_key.is_empty() {
        req = req.bearer_auth(&spec.api_key);
    }
    let resp = req.json(&body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("provider error {status}: {text}"));
    }

    let mut stream = resp.bytes_stream();
    // Raw bytes: a multi-byte UTF-8 char may be split across chunks, so only
    // complete lines are decoded.
    let mut buf: Vec<u8> = Vec::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut acc: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
    let mut usage = Usage::default();
    let mut had_tool_calls = false;
    let mut finish_reason: Option<String> = None;
    // Muse Glimmer interleaves a to=self reasoning channel in its text output.
    let mut splitter = spec.model_id.to_lowercase().contains("muse").then(ChannelSplitter::new);

    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                return Ok(AssistantMessage::failure(StopReason::Aborted, "aborted"));
            }
            c = stream.next() => match c {
                Some(Ok(bytes)) => bytes,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            }
        };
        buf.extend_from_slice(&chunk);

        while let Some(line) = take_line(&mut buf) {
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(json) = serde_json::from_str::<Value>(data) else { continue };

            if let Some(u) = json.get("usage").filter(|u| !u.is_null()) {
                usage.input = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(usage.input);
                usage.output = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(usage.output);
                usage.total = u.get("total_tokens").and_then(Value::as_u64).unwrap_or(usage.total);
            }

            if let Some(fr) = json.pointer("/choices/0/finish_reason").and_then(Value::as_str) {
                finish_reason = Some(fr.to_string());
            }

            let Some(delta) = json.pointer("/choices/0/delta") else { continue };
            // Reasoning models stream their chain-of-thought separately
            // (`reasoning_content` on DeepSeek/vLLM, `reasoning` on some gateways).
            if let Some(r) = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .or_else(|| delta.get("reasoning").and_then(Value::as_str))
            {
                if !r.is_empty() {
                    thinking.push_str(r);
                    let _ = tx.send(AgentEvent::MessageDelta { kind: DeltaKind::Thinking, text: r.to_string() }).await;
                }
            }
            if let Some(c) = delta.get("content").and_then(Value::as_str) {
                if !c.is_empty() {
                    if let Some(sp) = splitter.as_mut() {
                        let (nt, na) = sp.push(c);
                        if !nt.is_empty() {
                            thinking.push_str(&nt);
                            let _ = tx.send(AgentEvent::MessageDelta { kind: DeltaKind::Thinking, text: nt }).await;
                        }
                        if !na.is_empty() {
                            text.push_str(&na);
                            let _ = tx.send(AgentEvent::MessageDelta { kind: DeltaKind::Text, text: na }).await;
                        }
                    } else {
                        text.push_str(c);
                        let _ = tx.send(AgentEvent::MessageDelta { kind: DeltaKind::Text, text: c.to_string() }).await;
                    }
                }
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                had_tool_calls = true;
                for tc in tcs {
                    let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    while acc.len() <= idx {
                        acc.push((String::new(), String::new(), String::new()));
                    }
                    let e = &mut acc[idx];
                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                        e.0 = id.to_string();
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").and_then(Value::as_str) {
                            e.1 = n.to_string();
                        }
                        if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                            e.2.push_str(a);
                        }
                    }
                }
            }
        }
    }

    // Flush the splitter's held tail (end of stream).
    if let Some(sp) = splitter.as_mut() {
        let (nt, na) = sp.finish();
        if !nt.is_empty() {
            thinking.push_str(&nt);
            let _ = tx.send(AgentEvent::MessageDelta { kind: DeltaKind::Thinking, text: nt }).await;
        }
        if !na.is_empty() {
            text.push_str(&na);
            let _ = tx.send(AgentEvent::MessageDelta { kind: DeltaKind::Text, text: na }).await;
        }
    }

    // Fallback: local models (e.g. Gemma via a tool template) often emit the tool
    // call as text JSON rather than native tool_calls. If tools were offered and
    // the whole reply is a tool-call object, recover it.
    let text_tool_call = if !had_tool_calls && !tools.is_empty() {
        extract_text_tool_call(&text)
    } else {
        None
    };

    let mut content: Vec<ContentBlock> = Vec::new();
    // Thinking precedes the answer, mirroring how it was produced.
    if !thinking.is_empty() {
        content.push(ContentBlock::Thinking(thinking));
    }
    if let Some((name, args)) = text_tool_call.clone() {
        content.push(ContentBlock::ToolCall { id: format!("call_{name}"), name, arguments: args });
    } else if !text.is_empty() {
        content.push(ContentBlock::Text(text));
    }
    for (id, name, args) in acc.into_iter().filter(|(_, n, _)| !n.is_empty()) {
        let arguments: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
        content.push(ContentBlock::ToolCall {
            id: if id.is_empty() { format!("call_{name}") } else { id },
            name,
            arguments,
        });
    }

    // Derive cost from token counts (works even when the gateway reports none).
    if usage.cost_usd == 0.0 {
        let (ci, co) = model_cost_per_mtok(&spec.model_id);
        usage.cost_usd = (usage.input as f64 / 1e6) * ci + (usage.output as f64 / 1e6) * co;
    }

    let stop_reason = if had_tool_calls || text_tool_call.is_some() {
        StopReason::ToolUse
    } else if finish_reason.as_deref() == Some("length") {
        StopReason::Length // response was truncated by max_tokens
    } else {
        StopReason::Stop
    };
    Ok(AssistantMessage { content, stop_reason, error_message: None, usage })
}

/// Splits Muse Glimmer's dual-channel output into (reasoning, answer).
/// Muse emits `to=self<|message|>…reasoning…<|start|>assistant to=user<|message|>…answer…`;
/// we recompute the split over the accumulated text each push (immune to markers
/// split across chunks) and hold back a small tail so a half-arrived marker is
/// never shown. `finish()` flushes the tail.
struct ChannelSplitter {
    raw: String,
    sent_think: usize,
    sent_ans: usize,
    /// End of stream: classify even short/ambiguous text (never swallow it).
    done: bool,
}

const HOLD: usize = 44; // > longest marker: "<|start|>assistant to=user<|message|>"

impl ChannelSplitter {
    fn new() -> Self {
        Self { raw: String::new(), sent_think: 0, sent_ans: 0, done: false }
    }

    fn scrub(s: &str) -> String {
        s.replace("<|eot|>", "").replace("<|eom|>", "")
    }

    /// Current (thinking, answer) parse of the accumulated text.
    fn parse(&self) -> (String, String) {
        let s = self.raw.trim_start();
        if let Some(rest) = s.strip_prefix("to=self<|message|>") {
            // Reasoning first; the answer begins at the next assistant header.
            if let Some(i) = rest.find("<|start|>assistant") {
                let think = Self::scrub(&rest[..i]);
                let after = &rest[i..];
                let ans = after
                    .find("<|message|>")
                    .map(|j| Self::scrub(&after[j + "<|message|>".len()..]))
                    .unwrap_or_default();
                (think, ans)
            } else if let Some(i) = rest.find("<|eom|>") {
                (Self::scrub(&rest[..i]), Self::scrub(&rest[i + "<|eom|>".len()..]))
            } else {
                (Self::scrub(rest), String::new())
            }
        } else if let Some(rest) = s.strip_prefix("to=user<|message|>") {
            (String::new(), Self::scrub(rest))
        } else if let Some(rest) = s.strip_prefix("<|message|>") {
            (String::new(), Self::scrub(rest))
        } else if self.done || s.len() >= 20 || s.contains("<|message|>") {
            // No channel header — plain answer. (At end-of-stream this branch
            // is unconditional so short answers are never swallowed.)
            (String::new(), Self::scrub(s))
        } else {
            (String::new(), String::new()) // undecided: hold
        }
    }

    /// True when the accumulated text can't be channel-marked (plain output from
    /// a native-template model) — no hold-back needed, stream in real time.
    fn is_plain(&self) -> bool {
        let s = self.raw.trim_start();
        (self.done || s.len() >= 20) && !s.starts_with("to=") && !s.contains("<|")
    }

    /// Push a chunk; returns (new_thinking, new_answer) safe to emit now.
    fn push(&mut self, chunk: &str) -> (String, String) {
        self.raw.push_str(chunk);
        self.drain(HOLD)
    }

    /// Flush everything (end of stream).
    fn finish(&mut self) -> (String, String) {
        self.done = true;
        self.drain(0)
    }

    fn drain(&mut self, hold: usize) -> (String, String) {
        let hold = if self.is_plain() { 0 } else { hold };
        let (think, ans) = self.parse();
        // Hold back the tail of whichever channel is currently growing.
        let floor = |s: &str, mut i: usize| {
            while i > 0 && !s.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let (t_avail, a_avail) = if ans.is_empty() {
            (floor(&think, think.len().saturating_sub(hold)), 0)
        } else {
            (think.len(), floor(&ans, ans.len().saturating_sub(hold)))
        };
        let t_avail = t_avail.max(self.sent_think.min(think.len()));
        let a_avail = a_avail.max(self.sent_ans.min(ans.len()));
        let new_t = if t_avail > self.sent_think {
            let s = think[self.sent_think..t_avail].to_string();
            self.sent_think = t_avail;
            s
        } else {
            String::new()
        };
        let new_a = if a_avail > self.sent_ans {
            let s = ans[self.sent_ans..a_avail].to_string();
            self.sent_ans = a_avail;
            s
        } else {
            String::new()
        };
        (new_t, new_a)
    }
}

/// Recover a tool call emitted as text/JSON (fenced or `<tool_call>`-wrapped),
/// for local models that don't return native `tool_calls`. Returns the tool name
/// and arguments only if the reply is unambiguously a single tool-call object.
fn extract_text_tool_call(text: &str) -> Option<(String, Value)> {
    let mut t = text.trim();
    // Strip a <tool_call>…</tool_call> wrapper.
    if let Some(inner) = t.strip_prefix("<tool_call>") {
        t = inner.strip_suffix("</tool_call>").unwrap_or(inner).trim();
    }
    // Strip a ```json … ``` (or bare ```) fence.
    if let Some(inner) = t.strip_prefix("```") {
        let inner = inner.strip_prefix("json").unwrap_or(inner);
        t = inner.trim_end_matches('`').trim();
    }
    let v: Value = serde_json::from_str(t.trim()).ok()?;
    let obj = v.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let args = obj.get("arguments").or_else(|| obj.get("parameters")).cloned().unwrap_or_else(|| json!({}));
    // Arguments may themselves be a JSON string.
    let args = match args {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Object(Default::default())),
        other => other,
    };
    Some((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_line_keeps_split_multibyte_chars_intact() {
        let line = "data: {\"c\":\"héllo — 世界\"}\n".as_bytes();
        // Split inside the 3-byte '世' and inside the 2-byte 'é'.
        let cut = line.iter().position(|&b| b == 0xE4).unwrap() + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&line[..8]);
        assert!(take_line(&mut buf).is_none());
        buf.extend_from_slice(&line[8..cut]);
        assert!(take_line(&mut buf).is_none());
        buf.extend_from_slice(&line[cut..]);
        let got = take_line(&mut buf).unwrap();
        assert_eq!(got, "data: {\"c\":\"héllo — 世界\"}\n");
        assert!(!got.contains('\u{FFFD}'));
        assert!(buf.is_empty());
    }

    #[test]
    fn keys_only_go_to_the_official_host_or_user_base() {
        assert!(key_allowed_for("openai", "https://api.openai.com/v1", None));
        assert!(key_allowed_for("anthropic", "https://api.anthropic.com/v1", None));
        assert!(!key_allowed_for("openai", "https://api.anthropic.com/v1", None));
        assert!(!key_allowed_for("openai", "https://evil.example/v1", None));
        assert!(!key_allowed_for("openai", "http://api.openai.com/v1", None));
        assert!(!key_allowed_for("openai", "https://api.openai.com.evil.example/v1", None));
        assert!(!key_allowed_for("openai", "https://evil.example/v1?x=api.openai.com", None));
        assert!(!key_allowed_for("ollama", "http://localhost:11434/v1", None));
        // An endpoint the user configured explicitly is trusted.
        assert!(key_allowed_for("openai", "https://gw.corp/v1/", Some("https://gw.corp/v1")));
        assert!(!key_allowed_for("openai", "https://other.corp/v1", Some("https://gw.corp/v1")));
    }

    #[test]
    fn resolve_model_uses_one_key_per_provider() {
        std::env::remove_var("GITAGENT_MODEL_BASE_URL");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "ak");
        // No cross-provider fallback.
        assert_eq!(resolve_model("openai:gpt-4o").api_key, "");
        assert_eq!(resolve_model("anthropic:claude-x").api_key, "ak");
        std::env::set_var("OPENAI_API_KEY", "ok");
        assert_eq!(resolve_model("openai:gpt-4o").api_key, "ok");
        // A @baseurl override never receives the key.
        assert_eq!(resolve_model("openai:gpt-4o@https://evil.example/v1").api_key, "");
        assert_eq!(resolve_model("ollama:gemma3:4b").api_key, "");
        // The user-configured endpoint does.
        std::env::set_var("GITAGENT_MODEL_BASE_URL", "https://gw.corp/v1");
        let s = resolve_model("openai:gpt-4o");
        assert_eq!((s.base_url.as_str(), s.api_key.as_str()), ("https://gw.corp/v1", "ok"));
        std::env::remove_var("GITAGENT_MODEL_BASE_URL");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn loopback_base_urls() {
        assert!(is_loopback_base_url("http://localhost:11434/v1"));
        assert!(is_loopback_base_url("http://127.0.0.1:8080/v1"));
        assert!(is_loopback_base_url("http://[::1]:8080/v1"));
        assert!(!is_loopback_base_url("https://evil.example/v1"));
        assert!(!is_loopback_base_url("http://localhost.evil.example/v1"));
        assert!(!is_loopback_base_url("http://user@evil.example/v1"));
        assert!(!is_loopback_base_url("file:///etc/passwd"));
    }
}
