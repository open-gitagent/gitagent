//! OpenAI Responses API adapter: raw HTTP with streaming responses.
//!
//! Requests are stateless (`store: false`): the whole history is sent every
//! turn, and the model's output items (including encrypted reasoning) are
//! replayed unchanged from `Message::raw`.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use gitagent_loader::{Family, OpenAiCfg};
use serde_json::{json, Value};

use crate::sse::SseDecoder;
use crate::{
    ChatRequest, ChatResponse, Content, Message, Provider, ProviderError, RawContent, Role,
    StopReason, ToolCall, Usage,
};

pub const PROVIDER: &str = "openai";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// 429 codes that mean billing or quota problems; retrying does not help.
const QUOTA_CODES: &[&str] = &[
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
];

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    cfg: OpenAiCfg,
}

impl OpenAiProvider {
    pub fn new(api_key: String, base_url: Option<String>, cfg: OpenAiCfg) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("gitagent/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(300))
            .build()?;
        Ok(OpenAiProvider {
            client,
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            api_key,
            cfg,
        })
    }

    /// Reads `OPENAI_API_KEY` and, if set, `OPENAI_BASE_URL`.
    pub fn from_env(cfg: OpenAiCfg) -> anyhow::Result<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY is not set"))?;
        Self::new(key, std::env::var("OPENAI_BASE_URL").ok(), cfg)
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = serde_json::to_vec(&request_body(req, &self.cfg))
            .map_err(|e| ProviderError::Fatal(e.to_string()))?;
        let mut resp = self
            .client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
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
        Family::OpenAI
    }
}

/// Builds the request JSON (deterministic: serde_json sorts object keys).
pub(crate) fn request_body(req: &ChatRequest, cfg: &OpenAiCfg) -> Value {
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
                "strict": false,
            })
        })
        .collect();

    let mut input = Vec::new();
    for m in &req.messages {
        input.extend(message_items(m));
    }

    let mut reasoning = json!({"effort": req.effort.as_str()});
    if cfg.reasoning_summary != "none" {
        reasoning["summary"] = json!(cfg.reasoning_summary);
    }
    let mut body = json!({
        "model": req.model,
        "instructions": req.system,
        "input": input,
        "tools": tools,
        "parallel_tool_calls": true,
        "reasoning": reasoning,
        "max_output_tokens": req.max_output_tokens,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(t) = req.temperature {
        body["temperature"] = crate::temperature_json(t);
    }
    if let Some(key) = &cfg.prompt_cache_key {
        body["prompt_cache_key"] = json!(key);
    }
    body
}

fn message_items(m: &Message) -> Vec<Value> {
    if let Some(raw) = m.raw.as_ref().filter(|r| r.provider == PROVIDER) {
        return raw.blocks.clone();
    }
    let mut items = Vec::new();
    let mut text = Vec::new();
    for c in &m.content {
        match c {
            Content::Text { text: t } => text.push(t.as_str()),
            Content::ToolUse { id, name, input } => items.push(json!({
                "type": "function_call",
                "call_id": id,
                "name": name,
                "arguments": Value::Object(input.clone()).to_string(),
            })),
            Content::ToolResult {
                tool_use_id,
                content,
                images,
                ..
            } => {
                let output = if images.is_empty() {
                    json!(content)
                } else {
                    let mut parts = vec![json!({"type": "input_text", "text": content})];
                    parts.extend(images.iter().map(|i| {
                        json!({"type": "input_image", "image_url": format!("data:{};base64,{}", i.media_type, i.data)})
                    }));
                    Value::Array(parts)
                };
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": output,
                }))
            }
        }
    }
    if !text.is_empty() {
        let (role, part) = match m.role {
            Role::User => ("user", "input_text"),
            Role::Assistant => ("assistant", "output_text"),
        };
        let message = json!({
            "type": "message",
            "role": role,
            "content": [{"type": part, "text": text.join("\n")}],
        });
        // An assistant's text precedes its calls; a user's notes follow the
        // tool outputs they comment on.
        match m.role {
            Role::Assistant => items.insert(0, message),
            Role::User => items.push(message),
        }
    }
    items
}

pub(crate) fn classify_http_error(status: u16, body: &str) -> ProviderError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let err = &parsed["error"];
    let kind = err["type"].as_str().unwrap_or("");
    let code = err["code"].as_str().unwrap_or("");
    let message = err["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(500).collect());
    let detail = format!("HTTP {status} {kind} {code}: {message}");
    classify(status, kind, code, &message, detail)
}

fn classify(status: u16, kind: &str, code: &str, message: &str, detail: String) -> ProviderError {
    let lower = message.to_lowercase();
    if QUOTA_CODES.contains(&code) || kind == "insufficient_quota" {
        ProviderError::Fatal(detail)
    } else if code == "context_length_exceeded"
        || lower.contains("context window")
        || lower.contains("maximum context length")
    {
        ProviderError::ContextOverflow(detail)
    } else if matches!(status, 408 | 409 | 429 | 500 | 502 | 503 | 504)
        || matches!(
            kind,
            "server_error" | "rate_limit_error" | "service_unavailable_error"
        )
        || matches!(
            code,
            "server_error" | "rate_limit_exceeded" | "server_is_overloaded"
        )
    {
        ProviderError::Transient(detail)
    } else {
        ProviderError::Fatal(detail)
    }
}

/// Assembles a streamed response. The final `response.completed` (or
/// `.incomplete`) event carries the whole response; finished output items
/// are also collected as they arrive, in case the final event omits them.
#[derive(Default)]
pub(crate) struct StreamParser {
    sse: SseDecoder,
    items: BTreeMap<u64, Value>,
    last: Option<Value>,
}

impl StreamParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        for data in self.sse.feed(bytes) {
            if data == "[DONE]" {
                continue;
            }
            let ev: Value = serde_json::from_str(&data)
                .map_err(|e| ProviderError::Transient(format!("bad stream event: {e}")))?;
            match ev["type"].as_str().unwrap_or("") {
                "response.output_item.done" => {
                    let index = ev["output_index"]
                        .as_u64()
                        .unwrap_or(self.items.len() as u64);
                    self.items.insert(index, ev["item"].clone());
                }
                "response.completed" | "response.incomplete" => {
                    self.last = Some(ev["response"].clone());
                }
                "response.failed" => {
                    let e = &ev["response"]["error"];
                    let code = e["code"].as_str().unwrap_or("");
                    let message = e["message"].as_str().unwrap_or("");
                    return Err(classify(
                        0,
                        "",
                        code,
                        message,
                        format!("response failed {code}: {message}"),
                    ));
                }
                "error" => {
                    // Shapes differ between versions: fields at the top level
                    // or under `error`.
                    let e = if ev["error"].is_object() {
                        &ev["error"]
                    } else {
                        &ev
                    };
                    let kind = e["type"].as_str().unwrap_or("");
                    let code = e["code"].as_str().unwrap_or("");
                    let message = e["message"].as_str().unwrap_or("");
                    return Err(classify(
                        0,
                        kind,
                        code,
                        message,
                        format!("stream error {code}: {message}"),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<ChatResponse, ProviderError> {
        let Some(resp) = self.last else {
            return Err(ProviderError::Transient(
                "stream ended before the response completed".into(),
            ));
        };
        let mut items: Vec<Value> = match resp["output"].as_array() {
            Some(out) if !out.is_empty() => out.clone(),
            _ => self.items.into_values().collect(),
        };

        let mut text = Vec::new();
        let mut reasoning = Vec::new();
        let mut tool_calls = Vec::new();
        let mut refused = false;
        for item in &items {
            match item["type"].as_str().unwrap_or("") {
                "message" => {
                    for part in item["content"].as_array().into_iter().flatten() {
                        match part["type"].as_str().unwrap_or("") {
                            "output_text" => {
                                text.push(part["text"].as_str().unwrap_or("").to_string())
                            }
                            "refusal" => {
                                refused = true;
                                text.push(part["refusal"].as_str().unwrap_or("").to_string());
                            }
                            _ => {}
                        }
                    }
                }
                "reasoning" => {
                    for s in item["summary"].as_array().into_iter().flatten() {
                        if let Some(t) = s["text"].as_str().filter(|t| !t.is_empty()) {
                            reasoning.push(t.to_string());
                        }
                    }
                }
                "function_call" => {
                    let args = item["arguments"].as_str().unwrap_or("");
                    // Cut-off or invalid JSON goes back as a call with no arguments.
                    let arguments = serde_json::from_str::<Value>(args)
                        .ok()
                        .and_then(|v| v.as_object().cloned())
                        .unwrap_or_default();
                    tool_calls.push(ToolCall {
                        id: item["call_id"].as_str().unwrap_or("").to_string(),
                        name: item["name"].as_str().unwrap_or("").to_string(),
                        arguments,
                    });
                }
                _ => {}
            }
        }
        if !items
            .iter()
            .any(|i| matches!(i["type"].as_str(), Some("message" | "function_call")))
        {
            items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "(empty response)"}],
            }));
        }

        let usage = &resp["usage"];
        let count = |v: &Value| v.as_u64().unwrap_or(0);
        let usage = Usage {
            input_tokens: count(&usage["input_tokens"]),
            output_tokens: count(&usage["output_tokens"]),
            cache_read_tokens: count(&usage["input_tokens_details"]["cached_tokens"]),
            cache_write_tokens: count(&usage["input_tokens_details"]["cache_write_tokens"]),
        };

        let incomplete = resp["incomplete_details"]["reason"].as_str();
        let stop_reason = match (resp["status"].as_str(), incomplete) {
            (_, Some("max_output_tokens")) => StopReason::MaxTokens,
            (_, Some("content_filter")) => StopReason::Refusal,
            _ if refused && tool_calls.is_empty() => StopReason::Refusal,
            _ if !tool_calls.is_empty() => StopReason::ToolUse,
            (Some("completed"), _) => StopReason::EndTurn,
            (status, reason) => {
                StopReason::Other(reason.or(status).unwrap_or("unknown").to_string())
            }
        };
        Ok(ChatResponse {
            model: resp["model"].as_str().map(str::to_string),
            text: text.join("\n"),
            reasoning: (!reasoning.is_empty()).then(|| reasoning.join("\n\n")),
            tool_calls,
            usage,
            stop_reason,
            raw: Some(RawContent {
                provider: PROVIDER.to_string(),
                blocks: items,
            }),
            cleared_tool_results: 0,
        })
    }
}

#[cfg(test)]
mod tests;
