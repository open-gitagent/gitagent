use super::*;
use crate::{CachePlan, ToolSpec};
use gitagent_loader::Effort;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn cfg() -> AnthropicCfg {
    AnthropicCfg {
        thinking_display: ThinkingDisplay::Summarized,
        thinking_binding: ThinkingBinding::Error,
        refusal_fallback: true,
    }
}

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: "claude-opus-5-5".into(),
        system: "SYSTEM".into(),
        messages,
        tools: vec![ToolSpec {
            name: "exec".into(),
            description: "run".into(),
            input_schema: json!({"type": "object"}),
        }],
        effort: Effort::High,
        temperature: None,
        max_output_tokens: 64000,
        cache: CachePlan {
            system: true,
            last_messages: 2,
            one_hour: false,
        },
        server_masking: None,
    }
}

fn assistant_with_thinking() -> Message {
    let input = json!({"cmd": "ls"}).as_object().unwrap().clone();
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolUse {
            id: "toolu_1".into(),
            name: "exec".into(),
            input: input.clone(),
        }],
        raw: Some(RawContent {
            provider: PROVIDER.into(),
            blocks: vec![
                json!({"type": "thinking", "thinking": "plan", "signature": "SIG"}),
                json!({"type": "tool_use", "id": "toolu_1", "name": "exec", "input": input}),
            ],
        }),
    }
}

fn tool_result() -> Message {
    Message::tool_results(vec![Content::ToolResult {
        tool_use_id: "toolu_1".into(),
        content: "a\nb".into(),
        is_error: false,
        images: vec![],
    }])
}

#[test]
fn request_body_replays_raw_blocks_and_places_breakpoints() {
    let req = request(vec![
        Message::user_text("task"),
        assistant_with_thinking(),
        tool_result(),
    ]);
    let body = request_body(&req, &cfg());

    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 64000);
    assert_eq!(body["output_config"]["effort"], "high");
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["thinking"]["display"], "summarized");
    assert_eq!(
        body["thinking"]["block_binding"]["prefix_mismatch_behavior"],
        "error"
    );
    assert_eq!(body["fallbacks"], "default");
    assert!(body.get("temperature").is_none());
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["tools"][0]["name"], "exec");

    let msgs = body["messages"].as_array().unwrap();
    // First message is outside the last two: no breakpoint.
    assert!(msgs[0]["content"][0].get("cache_control").is_none());
    // The thinking block is replayed exactly and never marked.
    assert_eq!(
        msgs[1]["content"][0],
        json!({"type": "thinking", "thinking": "plan", "signature": "SIG"})
    );
    assert_eq!(msgs[1]["content"][1]["cache_control"]["type"], "ephemeral");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["cache_control"]["type"], "ephemeral");
    assert!(msgs[2]["content"][0].get("is_error").is_none());
}

#[test]
fn shared_prefix_bytes_are_stable_between_turns() {
    // Turn N's messages must reappear byte-for-byte in turn N+1, apart from
    // the moving cache markers.
    let strip = |mut v: Value| {
        for m in v["messages"].as_array_mut().unwrap() {
            for b in m["content"].as_array_mut().unwrap() {
                b.as_object_mut().unwrap().remove("cache_control");
            }
        }
        v
    };
    let turn1 = strip(request_body(
        &request(vec![Message::user_text("task"), assistant_with_thinking()]),
        &cfg(),
    ));
    let turn2 = strip(request_body(
        &request(vec![
            Message::user_text("task"),
            assistant_with_thinking(),
            tool_result(),
        ]),
        &cfg(),
    ));
    let m1 = turn1["messages"].as_array().unwrap();
    let m2 = turn2["messages"].as_array().unwrap();
    assert_eq!(&m2[..m1.len()], &m1[..]);
    assert_eq!(turn1["system"], turn2["system"]);
    assert_eq!(turn1["tools"], turn2["tools"]);
}

#[test]
fn optional_features_can_be_switched_off() {
    let mut c = cfg();
    c.refusal_fallback = false;
    c.thinking_display = ThinkingDisplay::Omitted;
    let mut req = request(vec![Message::user_text("x")]);
    req.temperature = Some(0.5);
    req.cache = CachePlan::default();
    let body = request_body(&req, &c);
    assert!(body.get("fallbacks").is_none());
    assert!(body["thinking"].get("display").is_none());
    assert_eq!(body["temperature"].to_string(), "0.5");
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(betas(&c), vec![BETA_THINKING_BINDING]);
    assert_eq!(betas(&cfg()), vec![BETA_THINKING_BINDING, BETA_FALLBACK]);
}

const STREAM: &str = "event: message_start\r\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"cache_creation_input_tokens\":200,\"cache_read_input_tokens\":3000,\"output_tokens\":1}}}\r\n\r\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Look at \"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"files.\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SIG==\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: ping\n\
data: {\"type\":\"ping\"}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Listing.\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"exec\",\"input\":{}}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\": \\\"ls \"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"-la\\\"}\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":2}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":57}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

fn parse_in_chunks(stream: &str, size: usize) -> Result<ChatResponse, ProviderError> {
    let mut p = StreamParser::default();
    for chunk in stream.as_bytes().chunks(size) {
        p.feed(chunk)?;
    }
    p.finish()
}

#[test]
fn assembles_a_streamed_response_from_any_chunking() {
    for size in [1, 7, 64, 4096] {
        let r = parse_in_chunks(STREAM, size).unwrap();
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.text, "Listing.");
        assert_eq!(r.reasoning.as_deref(), Some("Look at files."));
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "toolu_9");
        assert_eq!(r.tool_calls[0].arguments["cmd"], "ls -la");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 3210,
                output_tokens: 57,
                cache_read_tokens: 3000,
                cache_write_tokens: 200,
            }
        );
        let raw = r.raw.unwrap();
        assert_eq!(raw.provider, PROVIDER);
        assert_eq!(
            raw.blocks[0],
            json!({"type": "thinking", "thinking": "Look at files.", "signature": "SIG=="})
        );
        assert_eq!(raw.blocks[2]["input"], json!({"cmd": "ls -la"}));
    }
}

#[test]
fn incomplete_streams_and_stream_errors_are_retryable() {
    let cut = &STREAM[..STREAM.find("event: message_delta").unwrap()];
    assert!(matches!(
        parse_in_chunks(cut, 64),
        Err(ProviderError::Transient(_))
    ));
    let overloaded = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    assert!(matches!(
        parse_in_chunks(overloaded, 64),
        Err(ProviderError::RateLimited(_))
    ));
}

#[test]
fn truncated_tool_input_and_empty_turns_are_repaired() {
    let stream = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":5}}}\n\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"exec\",\"input\":{}}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\": \\\"cat <<EOF\"}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":64000}}\n\n\
data: {\"type\":\"message_stop\"}\n\n";
    let r = parse_in_chunks(stream, 16).unwrap();
    assert_eq!(r.stop_reason, StopReason::MaxTokens);
    assert!(r.tool_calls[0].arguments.is_empty());

    let refusal = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{}}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"output_tokens\":0}}\n\n\
data: {\"type\":\"message_stop\"}\n\n";
    let r = parse_in_chunks(refusal, 16).unwrap();
    assert_eq!(r.stop_reason, StopReason::Refusal);
    assert_eq!(
        r.raw.unwrap().blocks,
        vec![json!({"type": "text", "text": "(empty response)"})]
    );
}

#[test]
fn classifies_http_errors() {
    let body =
        |t: &str, m: &str| json!({"type": "error", "error": {"type": t, "message": m}}).to_string();
    assert!(matches!(
        classify_http_error(529, &body("overloaded_error", "Overloaded")),
        ProviderError::RateLimited(_)
    ));
    assert!(matches!(
        classify_http_error(503, &body("api_error", "down")),
        ProviderError::Transient(_)
    ));
    assert!(matches!(
        classify_http_error(429, &body("rate_limit_error", "slow down")),
        ProviderError::RateLimited(_)
    ));
    assert!(matches!(
        classify_http_error(
            400,
            &body(
                "invalid_request_error",
                "prompt is too long: 1000001 tokens > 1000000 maximum"
            )
        ),
        ProviderError::ContextOverflow(_)
    ));
    let fatal = classify_http_error(401, &body("authentication_error", "invalid x-api-key"));
    assert!(matches!(&fatal, ProviderError::Fatal(m) if m.contains("401")));
    assert!(matches!(
        classify_http_error(502, "<html>bad gateway</html>"),
        ProviderError::Transient(_)
    ));
}

/// Serves one HTTP response and returns the raw request it received.
async fn serve_once(
    status: &'static str,
    content_type: &'static str,
    body: String,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= head_end + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let resp = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n{body}"
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    (url, handle)
}

#[tokio::test]
async fn completes_against_a_local_server() {
    let (url, server) = serve_once("200 OK", "text/event-stream", STREAM.to_string()).await;
    let provider = AnthropicProvider::new("test-key".into(), Some(url), cfg()).unwrap();
    let resp = provider
        .complete(&request(vec![Message::user_text("task")]))
        .await
        .unwrap();
    assert_eq!(resp.tool_calls[0].arguments["cmd"], "ls -la");

    let raw = server.await.unwrap();
    let lower = raw.to_lowercase();
    assert!(raw.starts_with("POST /v1/messages HTTP/1.1"));
    assert!(lower.contains("x-api-key: test-key"));
    assert!(lower.contains("anthropic-version: 2023-06-01"));
    assert!(lower.contains(
        "anthropic-beta: thinking-binding-controls-2026-08-01,server-side-fallback-2026-07-01"
    ));
    let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["messages"][0]["content"][0]["text"], "task");
}

#[tokio::test]
async fn http_errors_are_classified_without_leaking_the_key() {
    let body = json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}).to_string();
    let (url, server) = serve_once("401 Unauthorized", "application/json", body).await;
    let provider = AnthropicProvider::new("secret-key-123".into(), Some(url), cfg()).unwrap();
    let err = provider
        .complete(&request(vec![Message::user_text("task")]))
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(&err, ProviderError::Fatal(_)));
    assert!(!err.to_string().contains("secret-key-123"));
}

#[test]
fn tool_results_with_images_become_content_arrays() {
    let msg = Message::tool_results(vec![Content::ToolResult {
        tool_use_id: "t".into(),
        content: "screenshot".into(),
        is_error: false,
        images: vec![crate::ImageData {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        }],
    }]);
    let blocks = message_blocks(&msg);
    assert_eq!(
        blocks[0]["content"],
        json!([
            {"type": "text", "text": "screenshot"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
        ])
    );
}

#[test]
fn server_masking_and_one_hour_cache_are_requested() {
    let mut req = request(vec![Message::user_text("task")]);
    req.cache.one_hour = true;
    req.server_masking = Some(crate::ServerMasking {
        trigger_tokens: 50000,
        keep: 8,
        clear_at_least: 25000,
    });
    let body = request_body(&req, &cfg());
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["ttl"],
        "1h"
    );
    assert_eq!(
        body["context_management"]["edits"][0],
        json!({
            "type": "clear_tool_uses_20250919",
            "trigger": {"type": "input_tokens", "value": 50000},
            "keep": {"type": "tool_uses", "value": 8},
            "clear_at_least": {"type": "input_tokens", "value": 25000}
        })
    );
    // Without masking there is no context_management field.
    let plain = request_body(&request(vec![Message::user_text("task")]), &cfg());
    assert!(plain.get("context_management").is_none());
}

#[test]
fn reports_tool_results_cleared_by_the_server() {
    let stream = STREAM.replace(
        "\"delta\":{\"stop_reason\":\"tool_use\"},",
        "\"delta\":{\"stop_reason\":\"tool_use\"},\"context_management\":{\"applied_edits\":[{\"type\":\"clear_tool_uses_20250919\",\"cleared_tool_uses\":6,\"cleared_input_tokens\":41000}]},",
    );
    let r = parse_in_chunks(&stream, 64).unwrap();
    assert_eq!(r.cleared_tool_results, 6);
}
