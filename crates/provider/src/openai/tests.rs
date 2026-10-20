use super::*;
use crate::{CachePlan, ToolSpec};
use gitagent_loader::Effort;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn cfg() -> OpenAiCfg {
    OpenAiCfg {
        reasoning_summary: "auto".into(),
        prompt_cache_key: Some("gitagent-v2".into()),
    }
}

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: "gpt-test".into(),
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
        cache: CachePlan::default(),
        server_masking: None,
    }
}

fn reasoning_item() -> Value {
    json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"})
}

fn call_item() -> Value {
    json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "exec", "arguments": "{\"cmd\":\"ls\"}", "status": "completed"})
}

#[test]
fn request_body_replays_output_items_and_converts_the_rest() {
    let assistant = Message {
        role: Role::Assistant,
        content: vec![],
        raw: Some(RawContent {
            provider: PROVIDER.into(),
            blocks: vec![reasoning_item(), call_item()],
        }),
    };
    let results = Message::tool_results(vec![Content::ToolResult {
        tool_use_id: "call_1".into(),
        content: "a\nb".into(),
        is_error: false,
        images: vec![],
    }]);
    let body = request_body(
        &request(vec![Message::user_text("task"), assistant, results]),
        &cfg(),
    );

    assert_eq!(body["instructions"], "SYSTEM");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["prompt_cache_key"], "gitagent-v2");
    assert_eq!(body["max_output_tokens"], 64000);
    assert!(body.get("temperature").is_none());
    assert_eq!(
        body["tools"][0],
        json!({"type": "function", "name": "exec", "description": "run", "parameters": {"type": "object"}, "strict": false})
    );
    let input = body["input"].as_array().unwrap();
    assert_eq!(
        input[0],
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "task"}]})
    );
    assert_eq!(input[1], reasoning_item());
    assert_eq!(input[2], call_item());
    assert_eq!(
        input[3],
        json!({"type": "function_call_output", "call_id": "call_1", "output": "a\nb"})
    );
}

#[test]
fn converts_history_from_other_providers() {
    let input = json!({"cmd": "ls"}).as_object().unwrap().clone();
    let assistant = Message {
        role: Role::Assistant,
        content: vec![
            Content::Text {
                text: "Listing.".into(),
            },
            Content::ToolUse {
                id: "c9".into(),
                name: "exec".into(),
                input,
            },
        ],
        raw: Some(RawContent {
            provider: "anthropic".into(),
            blocks: vec![json!({"type": "thinking"})],
        }),
    };
    let items = message_items(&assistant);
    assert_eq!(items[0]["content"][0]["type"], "output_text");
    assert_eq!(items[1]["type"], "function_call");
    assert_eq!(items[1]["arguments"], "{\"cmd\":\"ls\"}");
}

fn ev(v: Value) -> String {
    format!("event: {}\ndata: {}\n\n", v["type"].as_str().unwrap(), v)
}

fn stream() -> String {
    let message = json!({"type": "message", "id": "msg_1", "role": "assistant", "content": [{"type": "output_text", "text": "Listing."}]});
    let reasoning = json!({"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "Check files."}], "encrypted_content": "ENC"});
    let call = call_item();
    [
        ev(json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}})),
        ev(json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning})),
        ev(json!({"type": "response.output_text.delta", "delta": "List"})),
        ev(json!({"type": "response.output_item.done", "output_index": 1, "item": message})),
        ev(json!({"type": "response.function_call_arguments.delta", "delta": "{\"cmd\""})),
        ev(json!({"type": "response.output_item.done", "output_index": 2, "item": call})),
        ev(json!({"type": "response.completed", "response": {
            "id": "resp_1", "model": "gpt-test-2026", "status": "completed",
            "output": [reasoning, message, call],
            "usage": {"input_tokens": 5000, "output_tokens": 120,
                      "input_tokens_details": {"cached_tokens": 4096},
                      "output_tokens_details": {"reasoning_tokens": 80}}
        }})),
    ]
    .concat()
}

fn parse(s: &str, chunk: usize) -> Result<ChatResponse, ProviderError> {
    let mut p = StreamParser::default();
    for c in s.as_bytes().chunks(chunk) {
        p.feed(c)?;
    }
    p.finish()
}

#[test]
fn assembles_a_streamed_response() {
    for size in [1, 13, 4096] {
        let r = parse(&stream(), size).unwrap();
        assert_eq!(r.model.as_deref(), Some("gpt-test-2026"));
        assert_eq!(r.text, "Listing.");
        assert_eq!(r.reasoning.as_deref(), Some("Check files."));
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_calls[0].id, "call_1");
        assert_eq!(r.tool_calls[0].arguments["cmd"], "ls");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 5000,
                output_tokens: 120,
                cache_read_tokens: 4096,
                cache_write_tokens: 0
            }
        );
        let raw = r.raw.unwrap();
        assert_eq!(raw.blocks.len(), 3);
        assert_eq!(raw.blocks[0]["encrypted_content"], "ENC");
    }
}

#[test]
fn falls_back_to_streamed_items_when_the_final_event_has_no_output() {
    let call = call_item();
    let s = [
        ev(json!({"type": "response.output_item.done", "output_index": 0, "item": call})),
        ev(json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 10, "output_tokens": 2}}})),
    ]
    .concat();
    let r = parse(&s, 64).unwrap();
    assert_eq!(r.tool_calls.len(), 1);
}

#[test]
fn incomplete_failed_and_truncated_streams() {
    let s = ev(
        json!({"type": "response.incomplete", "response": {"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "output": [], "usage": {}}}),
    );
    let r = parse(&s, 64).unwrap();
    assert_eq!(r.stop_reason, StopReason::MaxTokens);
    assert_eq!(
        r.raw.unwrap().blocks[0]["content"][0]["text"],
        "(empty response)"
    );

    let s = ev(
        json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "boom"}}}),
    );
    assert!(matches!(parse(&s, 64), Err(ProviderError::Transient(_))));

    let s = ev(json!({"type": "error", "code": "context_length_exceeded", "message": "too long"}));
    assert!(matches!(
        parse(&s, 64),
        Err(ProviderError::ContextOverflow(_))
    ));

    let cut = &stream()[..stream().find("event: response.completed").unwrap()];
    assert!(matches!(parse(cut, 64), Err(ProviderError::Transient(_))));
}

#[test]
fn refusals_are_reported() {
    let message = json!({"type": "message", "role": "assistant", "content": [{"type": "refusal", "refusal": "I can't help with that."}]});
    let s = ev(
        json!({"type": "response.completed", "response": {"status": "completed", "output": [message], "usage": {}}}),
    );
    let r = parse(&s, 64).unwrap();
    assert_eq!(r.stop_reason, StopReason::Refusal);
}

#[test]
fn classifies_http_errors() {
    let body = |t: &str, c: &str, m: &str| {
        json!({"error": {"type": t, "code": c, "message": m}}).to_string()
    };
    assert!(matches!(
        classify_http_error(
            429,
            &body("rate_limit_error", "rate_limit_exceeded", "slow down")
        ),
        ProviderError::Transient(_)
    ));
    assert!(matches!(
        classify_http_error(
            429,
            &body(
                "insufficient_quota",
                "credit_balance_exhausted",
                "no credits"
            )
        ),
        ProviderError::Fatal(_)
    ));
    assert!(matches!(
        classify_http_error(
            400,
            &body(
                "invalid_request_error",
                "context_length_exceeded",
                "too long"
            )
        ),
        ProviderError::ContextOverflow(_)
    ));
    assert!(matches!(
        classify_http_error(
            401,
            &body("invalid_request_error", "invalid_api_key", "bad key")
        ),
        ProviderError::Fatal(_)
    ));
    assert!(matches!(
        classify_http_error(503, "upstream unavailable"),
        ProviderError::Transient(_)
    ));
}

#[tokio::test]
async fn completes_against_a_local_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req).to_string();
            if let Some(h) = text.find("\r\n\r\n") {
                let len: usize = text[..h]
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
            stream()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    let provider = OpenAiProvider::new("sk-test".into(), Some(url), cfg()).unwrap();
    let r = provider
        .complete(&request(vec![Message::user_text("task")]))
        .await
        .unwrap();
    assert_eq!(r.tool_calls[0].name, "exec");
    let raw = server.await.unwrap();
    assert!(raw.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(raw.to_lowercase().contains("authorization: bearer sk-test"));
}

#[test]
fn tool_results_with_images_become_input_parts() {
    let msg = Message::tool_results(vec![Content::ToolResult {
        tool_use_id: "c".into(),
        content: "chart".into(),
        is_error: false,
        images: vec![crate::ImageData {
            media_type: "image/jpeg".into(),
            data: "BBBB".into(),
        }],
    }]);
    let items = message_items(&msg);
    assert_eq!(
        items[0]["output"],
        json!([
            {"type": "input_text", "text": "chart"},
            {"type": "input_image", "image_url": "data:image/jpeg;base64,BBBB"}
        ])
    );
}

#[test]
fn notes_in_a_user_turn_follow_the_tool_outputs() {
    let msg = Message::tool_results(vec![
        Content::ToolResult {
            tool_use_id: "call_1".into(),
            content: "ok".into(),
            is_error: false,
            images: vec![],
        },
        Content::Text {
            text: "Time check".into(),
        },
    ]);
    let items = message_items(&msg);
    assert_eq!(items[0]["type"], "function_call_output");
    assert_eq!(items[1]["role"], "user");
    assert_eq!(items[1]["content"][0]["text"], "Time check");
}
