use super::*;
use crate::test_server::{body_of, serve_once};
use crate::{CachePlan, ToolSpec};

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: "gemini-test".into(),
        system: "SYSTEM".into(),
        messages,
        tools: vec![ToolSpec {
            name: "exec".into(),
            description: "run".into(),
            input_schema: json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        }],
        effort: Effort::High,
        temperature: None,
        max_output_tokens: 32000,
        cache: CachePlan::default(),
        server_masking: None,
    }
}

fn data(v: Value) -> String {
    format!("data: {v}\r\n\r\n")
}

fn stream() -> String {
    [
        data(json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "Plan: list files.", "thought": true}]}}]})),
        data(json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "Listing."}]}}]})),
        data(json!({"candidates": [{"content": {"role": "model", "parts": [
            {"functionCall": {"id": "g1", "name": "exec", "args": {"cmd": "ls"}}, "thoughtSignature": "SIG"}
        ]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 900, "cachedContentTokenCount": 512, "candidatesTokenCount": 40, "thoughtsTokenCount": 60},
            "modelVersion": "gemini-test-001"})),
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
fn assembles_a_streamed_response_and_keeps_signatures() {
    for size in [1, 17, 4096] {
        let r = parse(&stream(), size).unwrap();
        assert_eq!(r.text, "Listing.");
        assert_eq!(r.reasoning.as_deref(), Some("Plan: list files."));
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_calls[0].id, "g1");
        assert_eq!(r.tool_calls[0].arguments["cmd"], "ls");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 900,
                output_tokens: 100,
                cache_read_tokens: 512,
                cache_write_tokens: 0
            }
        );
        let raw = r.raw.unwrap();
        assert_eq!(raw.blocks[2]["thoughtSignature"], "SIG");
        assert_eq!(r.model.as_deref(), Some("gemini-test-001"));
    }
}

#[test]
fn request_replays_parts_and_names_function_responses() {
    let r = parse(&stream(), 4096).unwrap();
    let history = vec![
        Message::user_text("task"),
        r.to_message(),
        Message::tool_results(vec![Content::ToolResult {
            tool_use_id: "g1".into(),
            content: "a.txt".into(),
            is_error: false,
            images: vec![],
        }]),
    ];
    let body = request_body(&request(history));
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "SYSTEM");
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "high"
    );
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 32000);
    assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "exec");
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(
        contents[0],
        json!({"role": "user", "parts": [{"text": "task"}]})
    );
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][2]["thoughtSignature"], "SIG");
    assert_eq!(
        contents[2],
        json!({"role": "user", "parts": [{"functionResponse": {"id": "g1", "name": "exec", "response": {"result": "a.txt"}}}]})
    );
}

#[test]
fn calls_without_ids_get_ids_and_responses_match_by_name() {
    let s = data(
        json!({"candidates": [{"content": {"role": "model", "parts": [
        {"functionCall": {"name": "exec", "args": {"cmd": "pwd"}}}
    ]}, "finishReason": "STOP"}]}),
    );
    let r = parse(&s, 64).unwrap();
    assert_eq!(r.tool_calls[0].id, "gemini_call_1");
    let history = vec![
        Message::user_text("t"),
        r.to_message(),
        Message::tool_results(vec![Content::ToolResult {
            tool_use_id: "gemini_call_1".into(),
            content: "/".into(),
            is_error: true,
            images: vec![],
        }]),
    ];
    let body = request_body(&request(history));
    assert_eq!(
        body["contents"][2]["parts"][0],
        json!({"functionResponse": {"name": "exec", "response": {"error": "/"}}})
    );
}

#[test]
fn safety_blocks_errors_and_truncation() {
    let s = data(json!({"promptFeedback": {"blockReason": "SAFETY"}}));
    assert_eq!(parse(&s, 64).unwrap().stop_reason, StopReason::Refusal);
    let s = data(
        json!({"candidates": [{"content": {"parts": [{"text": "partial"}]}, "finishReason": "MAX_TOKENS"}]}),
    );
    assert_eq!(parse(&s, 64).unwrap().stop_reason, StopReason::MaxTokens);
    let cut = data(json!({"candidates": [{"content": {"parts": [{"text": "partial"}]}}]}));
    assert!(matches!(parse(&cut, 64), Err(ProviderError::Transient(_))));
    let err = |status: u16, state: &str, msg: &str| {
        classify_http_error(
            status,
            &json!({"error": {"code": status, "status": state, "message": msg}}).to_string(),
        )
    };
    assert!(matches!(
        err(429, "RESOURCE_EXHAUSTED", "quota"),
        ProviderError::RateLimited(_)
    ));
    assert!(matches!(
        err(
            400,
            "INVALID_ARGUMENT",
            "The input token count exceeds the maximum number of tokens allowed"
        ),
        ProviderError::ContextOverflow(_)
    ));
    assert!(matches!(
        err(403, "PERMISSION_DENIED", "bad key"),
        ProviderError::Fatal(_)
    ));
}

#[tokio::test]
async fn completes_against_a_local_server() {
    let (url, server) = serve_once("200 OK", "text/event-stream", stream()).await;
    let p = GoogleProvider::new("g-key".into(), Some(url)).unwrap();
    let r = p
        .complete(&request(vec![Message::user_text("task")]))
        .await
        .unwrap();
    assert_eq!(r.tool_calls[0].name, "exec");
    let raw = server.await.unwrap();
    assert!(
        raw.starts_with("POST /v1beta/models/gemini-test:streamGenerateContent?alt=sse HTTP/1.1")
    );
    assert!(raw.to_lowercase().contains("x-goog-api-key: g-key"));
    assert_eq!(body_of(&raw)["contents"][0]["parts"][0]["text"], "task");
}
