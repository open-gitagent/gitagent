//! Google Gemini adapter (`streamGenerateContent`, server-sent events).
//!
//! Model turns keep Gemini's own parts (`Message::raw`) and are replayed
//! unchanged: parts can carry a `thoughtSignature` that must come back with
//! them, or the request fails.

use std::collections::HashMap;

use async_trait::async_trait;
use gitagent_loader::{Effort, Family};
use serde_json::{json, Map, Value};

use crate::sse::SseDecoder;
use crate::{
    ChatRequest, ChatResponse, Content, Message, Provider, ProviderError, RawContent, Role,
    StopReason, ToolCall, Usage,
};

pub const PROVIDER: &str = "google";
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

pub struct GoogleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl GoogleProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> anyhow::Result<Self> {
        Ok(GoogleProvider {
            client: crate::http_client()?,
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            api_key,
        })
    }

    /// Reads `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) and, if set, `GEMINI_BASE_URL`.
    pub fn from_env() -> anyhow::Result<Self> {
        let key = ["GEMINI_API_KEY", "GOOGLE_API_KEY"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
            .ok_or_else(|| anyhow::anyhow!("GEMINI_API_KEY is not set"))?;
        Self::new(key, std::env::var("GEMINI_BASE_URL").ok())
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = serde_json::to_vec(&request_body(req))
            .map_err(|e| ProviderError::Fatal(e.to_string()))?;
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.base_url, req.model
        );
        let mut resp = self
            .client
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
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
        Family::Google
    }
}

pub(crate) fn request_body(req: &ChatRequest) -> Value {
    let declarations: Vec<Value> = req
        .tools
        .iter()
        .map(
            |t| json!({"name": t.name, "description": t.description, "parameters": t.input_schema}),
        )
        .collect();
    let level = match req.effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::XHigh | Effort::Max => "high",
    };
    let mut generation = json!({
        "maxOutputTokens": req.max_output_tokens,
        "thinkingConfig": {"thinkingLevel": level, "includeThoughts": true},
    });
    if let Some(t) = req.temperature {
        generation["temperature"] = crate::temperature_json(t);
    }
    json!({
        "systemInstruction": {"parts": [{"text": req.system}]},
        "contents": contents(&req.messages),
        "tools": [{"functionDeclarations": declarations}],
        "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
        "generationConfig": generation,
    })
}

/// Converts the history. Function responses need the function's name, which
/// is looked up from the call with the same id.
fn contents(messages: &[Message]) -> Vec<Value> {
    // Our call id -> (function name, Gemini's own id if it gave one).
    let mut calls: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::Assistant => {
                let parts = match m.raw.as_ref().filter(|r| r.provider == PROVIDER) {
                    Some(raw) => raw.blocks.clone(),
                    None => assistant_parts(m),
                };
                // Map ids for the responses that follow.
                let mut gemini_ids = parts
                    .iter()
                    .filter(|p| p.get("functionCall").is_some())
                    .map(|p| p["functionCall"]["id"].as_str().map(str::to_string));
                for c in &m.content {
                    if let Content::ToolUse { id, name, .. } = c {
                        let gid = gemini_ids.next().flatten();
                        calls.insert(id.clone(), (name.clone(), gid));
                    }
                }
                out.push(json!({"role": "model", "parts": parts}));
            }
            Role::User => {
                let mut parts = Vec::new();
                for c in &m.content {
                    match c {
                        Content::Text { text } => parts.push(json!({"text": text})),
                        Content::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                            images,
                        } => {
                            let (name, gid) = calls
                                .get(tool_use_id)
                                .cloned()
                                .unwrap_or_else(|| (String::from("unknown"), None));
                            let key = if *is_error { "error" } else { "result" };
                            let mut response = json!({"name": name, "response": {key: content}});
                            if let Some(gid) = gid {
                                response["id"] = json!(gid);
                            }
                            parts.push(json!({"functionResponse": response}));
                            parts.extend(images.iter().map(|i| {
                                json!({"inlineData": {"mimeType": i.media_type, "data": i.data}})
                            }));
                        }
                        Content::ToolUse { .. } => {}
                    }
                }
                out.push(json!({"role": "user", "parts": parts}));
            }
        }
    }
    out
}

fn assistant_parts(m: &Message) -> Vec<Value> {
    m.content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(json!({"text": text})),
            Content::ToolUse { name, input, .. } => {
                Some(json!({"functionCall": {"name": name, "args": input}}))
            }
            Content::ToolResult { .. } => None,
        })
        .collect()
}

pub(crate) fn classify_http_error(status: u16, body: &str) -> ProviderError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    // Errors come as an object or as a one-element array of objects.
    let err = if parsed.is_array() {
        &parsed[0]["error"]
    } else {
        &parsed["error"]
    };
    let state = err["status"].as_str().unwrap_or("");
    let message = err["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(500).collect());
    let detail = format!("HTTP {status} {state}: {message}");
    let lower = message.to_lowercase();
    if lower.contains("exceeds the maximum number of tokens") || lower.contains("too long") {
        ProviderError::ContextOverflow(detail)
    } else if matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
        || matches!(
            state,
            "RESOURCE_EXHAUSTED" | "UNAVAILABLE" | "INTERNAL" | "DEADLINE_EXCEEDED"
        )
    {
        ProviderError::Transient(detail)
    } else {
        ProviderError::Fatal(detail)
    }
}

#[derive(Default)]
pub(crate) struct StreamParser {
    sse: SseDecoder,
    parts: Vec<Value>,
    finish: Option<String>,
    blocked: Option<String>,
    usage: Value,
    model: Option<String>,
    events: usize,
}

impl StreamParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        for data in self.sse.feed(bytes) {
            let ev: Value = serde_json::from_str(&data)
                .map_err(|e| ProviderError::Transient(format!("bad stream event: {e}")))?;
            if ev.get("error").is_some() {
                return Err(classify_http_error(0, &data));
            }
            self.events += 1;
            if let Some(reason) = ev["promptFeedback"]["blockReason"].as_str() {
                self.blocked = Some(reason.to_string());
            }
            if let Some(cand) = ev["candidates"].get(0) {
                for p in cand["content"]["parts"].as_array().into_iter().flatten() {
                    self.parts.push(p.clone());
                }
                if let Some(f) = cand["finishReason"].as_str() {
                    self.finish = Some(f.to_string());
                }
            }
            if ev["usageMetadata"].is_object() {
                self.usage = ev["usageMetadata"].clone();
            }
            if let Some(m) = ev["modelVersion"].as_str() {
                self.model = Some(m.to_string());
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<ChatResponse, ProviderError> {
        if self.events == 0 || (self.finish.is_none() && self.blocked.is_none()) {
            return Err(ProviderError::Transient(
                "stream ended before the response finished".into(),
            ));
        }
        let mut text = Vec::new();
        let mut reasoning = Vec::new();
        let mut tool_calls = Vec::new();
        for p in &self.parts {
            if let Some(call) = p.get("functionCall") {
                let n = tool_calls.len() + 1;
                tool_calls.push(ToolCall {
                    // Gemini may omit ids; ours only need to be unique.
                    id: call["id"]
                        .as_str()
                        .map_or_else(|| format!("gemini_call_{n}"), str::to_string),
                    name: call["name"].as_str().unwrap_or("").to_string(),
                    arguments: call["args"].as_object().cloned().unwrap_or_else(Map::new),
                });
            } else if let Some(t) = p["text"].as_str() {
                if p["thought"].as_bool() == Some(true) {
                    reasoning.push(t.to_string());
                } else {
                    text.push(t.to_string());
                }
            }
        }
        let mut parts = self.parts;
        if !parts.iter().any(|p| {
            p.get("functionCall").is_some() || (p.get("text").is_some() && p["thought"] != true)
        }) {
            parts.push(json!({"text": "(empty response)"}));
        }
        let count = |k: &str| self.usage[k].as_u64().unwrap_or(0);
        let usage = Usage {
            input_tokens: count("promptTokenCount"),
            output_tokens: count("candidatesTokenCount") + count("thoughtsTokenCount"),
            cache_read_tokens: count("cachedContentTokenCount"),
            cache_write_tokens: 0,
        };
        let refusal = matches!(
            self.finish.as_deref(),
            Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" | "RECITATION")
        ) || self.blocked.is_some();
        let stop_reason = if refusal {
            StopReason::Refusal
        } else if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else {
            match self.finish.as_deref() {
                Some("STOP") => StopReason::EndTurn,
                Some("MAX_TOKENS") => StopReason::MaxTokens,
                other => StopReason::Other(other.unwrap_or("unknown").to_lowercase()),
            }
        };
        Ok(ChatResponse {
            model: self.model,
            text: text.concat(),
            reasoning: (!reasoning.is_empty()).then(|| reasoning.concat()),
            tool_calls,
            usage,
            stop_reason,
            raw: Some(RawContent {
                provider: PROVIDER.to_string(),
                blocks: parts,
            }),
            cleared_tool_results: 0,
        })
    }
}

#[cfg(test)]
mod tests;
