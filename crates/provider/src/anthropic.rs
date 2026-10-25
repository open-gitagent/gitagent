//! Anthropic Messages API adapter: raw HTTP with streaming responses.
//!
//! Assistant turns keep the API's own content blocks (`Message::raw`) and are
//! replayed verbatim, because thinking blocks are bound to their signature
//! and to the exact conversation that produced them. The history is append-only.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use gitagent_loader::{AnthropicCfg, Family, ThinkingBinding, ThinkingDisplay};
use serde_json::{json, Map, Value};

use crate::sse::SseDecoder;
use crate::{
    ChatRequest, ChatResponse, Content, Message, Provider, ProviderError, RawContent, Role,
    StopReason, ToolCall, Usage,
};

pub const PROVIDER: &str = "anthropic";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const BETA_THINKING_BINDING: &str = "thinking-binding-controls-2026-08-01";
const BETA_FALLBACK: &str = "server-side-fallback-2026-07-01";
const BETA_CONTEXT_EDITING: &str = "context-management-2025-06-27";
/// Block types that may carry a `cache_control` marker.
const CACHEABLE: &[&str] = &["text", "tool_use", "tool_result", "image", "document"];

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    cfg: AnthropicCfg,
}

impl AnthropicProvider {
    pub fn new(
        api_key: String,
        base_url: Option<String>,
        cfg: AnthropicCfg,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("gitagent/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            // The stream sends pings, so a long silence means a dead connection.
            .read_timeout(Duration::from_secs(300))
            .build()?;
        Ok(AnthropicProvider {
            client,
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            api_key,
            cfg,
        })
    }

    /// Reads `ANTHROPIC_API_KEY` and, if set, `ANTHROPIC_BASE_URL`.
    pub fn from_env(cfg: AnthropicCfg) -> anyhow::Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("ANTHROPIC_API_KEY is not set"))?;
        Self::new(key, std::env::var("ANTHROPIC_BASE_URL").ok(), cfg)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = request_body(req, &self.cfg);
        let mut http = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        let mut betas = betas(&self.cfg);
        if req.server_masking.is_some() {
            betas.push(BETA_CONTEXT_EDITING);
        }
        if !betas.is_empty() {
            http = http.header("anthropic-beta", betas.join(","));
        }
        let body = serde_json::to_vec(&body).map_err(|e| ProviderError::Fatal(e.to_string()))?;
        let mut resp = http
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::Transient(format!("request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(status.as_u16(), &text));
        }
        let mut stream = StreamParser::default();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ProviderError::Transient(format!("stream interrupted: {e}")))?
        {
            stream.feed(&chunk)?;
        }
        stream.finish()
    }

    fn family(&self) -> Family {
        Family::Anthropic
    }
}

fn betas(cfg: &AnthropicCfg) -> Vec<&'static str> {
    let mut betas = vec![BETA_THINKING_BINDING];
    if cfg.refusal_fallback {
        betas.push(BETA_FALLBACK);
    }
    betas
}

/// Builds the request JSON. Serialization is deterministic (sorted keys), so
/// the bytes of the shared prefix are identical from one turn to the next.
pub(crate) fn request_body(req: &ChatRequest, cfg: &AnthropicCfg) -> Value {
    let marker = if req.cache.one_hour {
        json!({"type": "ephemeral", "ttl": "1h"})
    } else {
        json!({"type": "ephemeral"})
    };
    let mut system = json!({"type": "text", "text": req.system});
    if req.cache.system {
        system["cache_control"] = marker.clone();
    }

    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
        .collect();

    let mut messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": message_blocks(m),
            })
        })
        .collect();
    let n = messages.len();
    for msg in messages
        .iter_mut()
        .skip(n.saturating_sub(req.cache.last_messages as usize))
    {
        mark_last_cacheable(msg, &marker);
    }

    let mut thinking = json!({
        "type": "adaptive",
        "block_binding": {
            "prefix_mismatch_behavior": match cfg.thinking_binding {
                ThinkingBinding::Error => "error",
                ThinkingBinding::DropBlock => "drop_block",
            }
        },
    });
    // `omitted` is the API default, so only `summarized` needs sending.
    if cfg.thinking_display == ThinkingDisplay::Summarized {
        thinking["display"] = json!("summarized");
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_output_tokens,
        "stream": true,
        "system": [system],
        "tools": tools,
        "messages": messages,
        "thinking": thinking,
        "output_config": {"effort": req.effort.as_str()},
    });
    if let Some(t) = req.temperature {
        body["temperature"] = crate::temperature_json(t);
    }
    if cfg.refusal_fallback {
        body["fallbacks"] = json!("default");
    }
    if let Some(m) = req.server_masking {
        body["context_management"] = json!({"edits": [{
            "type": "clear_tool_uses_20250919",
            "trigger": {"type": "input_tokens", "value": m.trigger_tokens},
            "keep": {"type": "tool_uses", "value": m.keep},
            "clear_at_least": {"type": "input_tokens", "value": m.clear_at_least},
        }]});
    }
    body
}

fn message_blocks(m: &Message) -> Vec<Value> {
    if let Some(raw) = m.raw.as_ref().filter(|r| r.provider == PROVIDER) {
        return raw.blocks.clone();
    }
    m.content
        .iter()
        .map(|c| match c {
            Content::Text { text } => json!({"type": "text", "text": text}),
            Content::ToolUse { id, name, input } => {
                json!({"type": "tool_use", "id": id, "name": name, "input": input})
            }
            Content::ToolResult {
                tool_use_id,
                content,
                is_error,
                images,
            } => {
                let content = if images.is_empty() {
                    json!(content)
                } else {
                    let mut parts = vec![json!({"type": "text", "text": content})];
                    parts.extend(images.iter().map(|i| {
                        json!({"type": "image", "source": {"type": "base64", "media_type": i.media_type, "data": i.data}})
                    }));
                    Value::Array(parts)
                };
                let mut block =
                    json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
                if *is_error {
                    block["is_error"] = json!(true);
                }
                block
            }
        })
        .collect()
}

/// Puts a cache breakpoint on the last block of `msg` that can carry one.
fn mark_last_cacheable(msg: &mut Value, marker: &Value) {
    let Some(blocks) = msg["content"].as_array_mut() else {
        return;
    };
    if let Some(block) = blocks
        .iter_mut()
        .rev()
        .find(|b| CACHEABLE.contains(&b["type"].as_str().unwrap_or("")))
    {
        block["cache_control"] = marker.clone();
    }
}

pub(crate) fn classify_http_error(status: u16, body: &str) -> ProviderError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let kind = parsed["error"]["type"].as_str().unwrap_or("");
    let message = parsed["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(500).collect());
    let detail = format!("HTTP {status} {kind}: {message}");
    classify(status, kind, &message, detail)
}

fn classify(status: u16, kind: &str, message: &str, detail: String) -> ProviderError {
    let lower = message.to_lowercase();
    if matches!(status, 429 | 529) || matches!(kind, "rate_limit_error" | "overloaded_error") {
        ProviderError::RateLimited(detail)
    } else if matches!(status, 408 | 500 | 502 | 503 | 504) || kind == "api_error" {
        ProviderError::Transient(detail)
    } else if kind == "request_too_large"
        || lower.contains("prompt is too long")
        || lower.contains("context window")
        || lower.contains("too many tokens")
    {
        ProviderError::ContextOverflow(detail)
    } else {
        ProviderError::Fatal(detail)
    }
}

/// Assembles a streamed response from server-sent events.
#[derive(Default)]
pub(crate) struct StreamParser {
    sse: SseDecoder,
    model: Option<String>,
    blocks: Vec<Value>,
    partial_json: BTreeMap<usize, String>,
    usage: Map<String, Value>,
    stop_reason: Option<String>,
    cleared_tool_results: u64,
    done: bool,
}

impl StreamParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        for data in self.sse.feed(bytes) {
            self.handle_event(&data)?;
        }
        Ok(())
    }

    fn handle_event(&mut self, data: &str) -> Result<(), ProviderError> {
        let ev: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Transient(format!("bad stream event: {e}")))?;
        let index = ev["index"].as_u64().unwrap_or(0) as usize;
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                let msg = &ev["message"];
                self.model = msg["model"].as_str().map(str::to_string);
                self.merge_usage(&msg["usage"]);
            }
            "content_block_start" => {
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, Value::Null);
                }
                self.blocks[index] = ev["content_block"].clone();
            }
            "content_block_delta" => {
                let delta = &ev["delta"];
                let Some(block) = self.blocks.get_mut(index) else {
                    return Ok(());
                };
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => append(block, "text", &delta["text"]),
                    "thinking_delta" => append(block, "thinking", &delta["thinking"]),
                    "signature_delta" => block["signature"] = delta["signature"].clone(),
                    "input_json_delta" => self
                        .partial_json
                        .entry(index)
                        .or_default()
                        .push_str(delta["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(json), Some(block)) =
                    (self.partial_json.remove(&index), self.blocks.get_mut(index))
                {
                    if !json.trim().is_empty() {
                        // A call cut off by max_tokens has incomplete JSON; it
                        // goes back to the model as a call with no arguments.
                        block["input"] = serde_json::from_str::<Value>(&json)
                            .ok()
                            .filter(Value::is_object)
                            .unwrap_or_else(|| json!({}));
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = ev["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
                for edit in ev["context_management"]["applied_edits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    self.cleared_tool_results += edit["cleared_tool_uses"].as_u64().unwrap_or(0);
                }
                self.merge_usage(&ev["usage"]);
            }
            "message_stop" => self.done = true,
            "error" => {
                let kind = ev["error"]["type"].as_str().unwrap_or("");
                let message = ev["error"]["message"].as_str().unwrap_or("");
                return Err(classify(
                    0,
                    kind,
                    message,
                    format!("stream error {kind}: {message}"),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn merge_usage(&mut self, usage: &Value) {
        if let Some(obj) = usage.as_object() {
            for (k, v) in obj {
                if v.is_number() {
                    self.usage.insert(k.clone(), v.clone());
                }
            }
        }
    }

    pub fn finish(self) -> Result<ChatResponse, ProviderError> {
        if !self.done {
            return Err(ProviderError::Transient(
                "stream ended before message_stop".into(),
            ));
        }
        let count = |k: &str| self.usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        let cache_read = count("cache_read_input_tokens");
        let cache_write = count("cache_creation_input_tokens");
        let usage = Usage {
            input_tokens: count("input_tokens") + cache_read + cache_write,
            output_tokens: count("output_tokens"),
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        };

        let mut blocks: Vec<Value> = self.blocks.into_iter().filter(|b| !b.is_null()).collect();
        let mut text = Vec::new();
        let mut reasoning = Vec::new();
        let mut tool_calls = Vec::new();
        for b in &blocks {
            match b["type"].as_str().unwrap_or("") {
                "text" => text.push(b["text"].as_str().unwrap_or("").to_string()),
                "thinking" => {
                    let t = b["thinking"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        reasoning.push(t.to_string());
                    }
                }
                "tool_use" => tool_calls.push(ToolCall {
                    id: b["id"].as_str().unwrap_or("").to_string(),
                    name: b["name"].as_str().unwrap_or("").to_string(),
                    arguments: b["input"].as_object().cloned().unwrap_or_default(),
                }),
                _ => {}
            }
        }
        // An assistant turn needs visible content when it is replayed.
        if !blocks
            .iter()
            .any(|b| matches!(b["type"].as_str(), Some("text" | "tool_use")))
        {
            blocks.push(json!({"type": "text", "text": "(empty response)"}));
        }

        let stop_reason = match self.stop_reason.as_deref() {
            Some("end_turn") => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("refusal") => StopReason::Refusal,
            other => StopReason::Other(other.unwrap_or("unknown").to_string()),
        };
        Ok(ChatResponse {
            model: self.model,
            text: text.join("\n"),
            reasoning: (!reasoning.is_empty()).then(|| reasoning.join("\n\n")),
            tool_calls,
            usage,
            stop_reason,
            raw: Some(RawContent {
                provider: PROVIDER.to_string(),
                blocks,
            }),
            cleared_tool_results: self.cleared_tool_results,
        })
    }
}

fn append(block: &mut Value, field: &str, piece: &Value) {
    let add = piece.as_str().unwrap_or("");
    let current = block[field].as_str().unwrap_or("").to_string();
    block[field] = Value::String(current + add);
}

#[cfg(test)]
mod tests;
