//! OpenAI-compatible Chat Completions adapter, for open-weight models served
//! by vLLM, SGLang, Ollama, OpenRouter and similar endpoints. Profiles for
//! these models usually also turn on the text fallback for tool calls.

use std::collections::BTreeMap;

use async_trait::async_trait;
use gitagent_loader::Family;
use serde_json::{json, Value};

use crate::sse::SseDecoder;
use crate::{
    ChatRequest, ChatResponse, Content, Message, Provider, ProviderError, Role, StopReason,
    ToolCall, Usage,
};

pub struct CompatProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

/// Well-known endpoints by provider name; anything else needs `{NAME}_BASE_URL`.
const KNOWN: &[(&str, &str, &str)] = &[
    (
        "openrouter",
        "https://openrouter.ai/api/v1",
        "OPENROUTER_API_KEY",
    ),
    ("ollama", "http://localhost:11434/v1", "OLLAMA_API_KEY"),
    ("vllm", "http://localhost:8000/v1", "VLLM_API_KEY"),
];

impl CompatProvider {
    pub fn new(base_url: String, api_key: Option<String>) -> anyhow::Result<Self> {
        Ok(CompatProvider {
            client: crate::http_client()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    /// For a model given as `<name>:<model>`: the base URL comes from
    /// `{NAME}_BASE_URL` (or a built-in default for openrouter, ollama and
    /// vllm) and the key from `{NAME}_API_KEY` or `GITAGENT_API_KEY`.
    pub fn from_env(name: &str) -> anyhow::Result<Self> {
        let upper = name.to_uppercase().replace('-', "_");
        let known = KNOWN.iter().find(|(n, _, _)| *n == name);
        let base = std::env::var(format!("{upper}_BASE_URL"))
            .ok()
            .or_else(|| known.map(|(_, url, _)| url.to_string()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "set {upper}_BASE_URL to the OpenAI-compatible endpoint for {name:?}"
                )
            })?;
        let key_var = known.map_or(format!("{upper}_API_KEY"), |(_, _, k)| k.to_string());
        let key = [key_var.as_str(), "GITAGENT_API_KEY"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
        Self::new(base, key)
    }
}

#[async_trait]
impl Provider for CompatProvider {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = serde_json::to_vec(&request_body(req))
            .map_err(|e| ProviderError::Fatal(e.to_string()))?;
        let mut http = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("content-type", "application/json");
        if let Some(key) = &self.api_key {
            http = http.bearer_auth(key);
        }
        let mut resp = http
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::Transient(format!("request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(crate::openai::classify_http_error(status.as_u16(), &text));
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
        Family::Compat
    }
}

pub(crate) fn request_body(req: &ChatRequest) -> Value {
    let mut messages = vec![json!({"role": "system", "content": req.system})];
    for m in &req.messages {
        messages.extend(convert(m));
    }
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({"type": "function", "function": {
                "name": t.name, "description": t.description, "parameters": t.input_schema
            }})
        })
        .collect();
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "max_tokens": req.max_output_tokens,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if let Some(t) = req.temperature {
        body["temperature"] = crate::temperature_json(t);
    }
    body
}

fn convert(m: &Message) -> Vec<Value> {
    let text: Vec<&str> = m
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    match m.role {
        Role::Assistant => {
            let calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::ToolUse { id, name, input } => Some(json!({
                        "id": id, "type": "function",
                        "function": {"name": name, "arguments": Value::Object(input.clone()).to_string()}
                    })),
                    _ => None,
                })
                .collect();
            let mut msg = json!({"role": "assistant", "content": text.join("\n")});
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            vec![msg]
        }
        Role::User => {
            let mut out: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::ToolResult {
                        tool_use_id,
                        content,
                        images,
                        ..
                    } => {
                        let note = if images.is_empty() {
                            String::new()
                        } else {
                            "\n[images are not supported by this endpoint and were left out]".into()
                        };
                        Some(json!({"role": "tool", "tool_call_id": tool_use_id, "content": format!("{content}{note}")}))
                    }
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                out.push(json!({"role": "user", "content": text.join("\n")}));
            }
            out
        }
    }
}

#[derive(Default)]
pub(crate) struct StreamParser {
    sse: SseDecoder,
    text: String,
    reasoning: String,
    /// Tool calls by stream index: id, name, accumulated arguments.
    calls: BTreeMap<u64, (String, String, String)>,
    finish: Option<String>,
    usage: Value,
    model: Option<String>,
    done: bool,
}

impl StreamParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        for data in self.sse.feed(bytes) {
            if data.trim() == "[DONE]" {
                self.done = true;
                continue;
            }
            let ev: Value = serde_json::from_str(&data)
                .map_err(|e| ProviderError::Transient(format!("bad stream event: {e}")))?;
            if ev.get("error").is_some() {
                return Err(crate::openai::classify_http_error(0, &data));
            }
            if let Some(m) = ev["model"].as_str() {
                self.model = Some(m.to_string());
            }
            if ev["usage"].is_object() {
                self.usage = ev["usage"].clone();
            }
            let Some(choice) = ev["choices"].get(0) else {
                continue;
            };
            let delta = &choice["delta"];
            if let Some(t) = delta["content"].as_str() {
                self.text.push_str(t);
            }
            // vLLM/DeepSeek use reasoning_content, OpenRouter uses reasoning.
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta[key].as_str() {
                    self.reasoning.push_str(t);
                }
            }
            for tc in delta["tool_calls"].as_array().into_iter().flatten() {
                let index = tc["index"].as_u64().unwrap_or(self.calls.len() as u64);
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = tc["id"].as_str() {
                    entry.0 = id.to_string();
                }
                if let Some(name) = tc["function"]["name"].as_str() {
                    entry.1.push_str(name);
                }
                if let Some(args) = tc["function"]["arguments"].as_str() {
                    entry.2.push_str(args);
                }
            }
            if let Some(f) = choice["finish_reason"].as_str() {
                self.finish = Some(f.to_string());
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<ChatResponse, ProviderError> {
        if self.finish.is_none() && !self.done {
            return Err(ProviderError::Transient(
                "stream ended before the response finished".into(),
            ));
        }
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_values()
            .enumerate()
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{}", i + 1)
                } else {
                    id
                },
                name,
                arguments: serde_json::from_str::<Value>(&args)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default(),
            })
            .collect();
        let count = |v: &Value| v.as_u64().unwrap_or(0);
        let usage = Usage {
            input_tokens: count(&self.usage["prompt_tokens"]),
            output_tokens: count(&self.usage["completion_tokens"]),
            cache_read_tokens: count(&self.usage["prompt_tokens_details"]["cached_tokens"]),
            cache_write_tokens: 0,
        };
        let stop_reason = match self.finish.as_deref() {
            _ if !tool_calls.is_empty() => StopReason::ToolUse,
            Some("stop") | None => StopReason::EndTurn,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            Some(other) => StopReason::Other(other.to_string()),
        };
        Ok(ChatResponse {
            model: self.model,
            text: self.text,
            reasoning: (!self.reasoning.is_empty()).then_some(self.reasoning),
            tool_calls,
            usage,
            stop_reason,
            // The history is rebuilt from text and calls; reasoning is not
            // sent back, as these endpoints expect.
            raw: None,
            cleared_tool_results: 0,
        })
    }
}

#[cfg(test)]
mod tests;
