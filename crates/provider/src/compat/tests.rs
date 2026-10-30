use super::*;
use crate::test_server::{body_of, serve_once};
use crate::{CachePlan, ToolSpec};
use gitagent_loader::Effort;

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: "qwen-test".into(),
        system: "SYSTEM".into(),
        messages,
        tools: vec![ToolSpec {
            name: "exec".into(),
            description: "run".into(),
            input_schema: json!({"type": "object"}),
        }],
        effort: Effort::Medium,
        temperature: Some(0.7),
        sampling: Default::default(),
        max_output_tokens: 16000,
        cache: CachePlan::default(),
        server_masking: None,
    }
}

fn data(v: Value) -> String {
    format!("data: {v}\n\n")
}

fn stream() -> String {
    [
        data(json!({"model": "qwen-test", "choices": [{"delta": {"role": "assistant", "reasoning_content": "Think."}}]})),
        data(json!({"choices": [{"delta": {"content": "Run it."}}]})),
        data(json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "type": "function", "function": {"name": "exec", "arguments": "{\"cmd\": "}}]}}]})),
        data(json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"ls\"}"}}]}, "finish_reason": "tool_calls"}]})),
        data(json!({"choices": [], "usage": {"prompt_tokens": 700, "completion_tokens": 30, "prompt_tokens_details": {"cached_tokens": 600}}})),
        "data: [DONE]\n\n".to_string(),
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
fn assembles_streamed_text_reasoning_and_tool_calls() {
    for size in [1, 23, 4096] {
        let r = parse(&stream(), size).unwrap();
        assert_eq!(r.text, "Run it.");
        assert_eq!(r.reasoning.as_deref(), Some("Think."));
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_calls[0].id, "c1");
        assert_eq!(r.tool_calls[0].arguments["cmd"], "ls");
        assert_eq!(r.usage.input_tokens, 700);
        assert_eq!(r.usage.cache_read_tokens, 600);
        assert!(r.raw.is_none());
    }
}

#[test]
fn request_uses_system_tool_and_tool_result_messages() {
    let r = parse(&stream(), 4096).unwrap();
    let history = vec![
        Message::user_text("task"),
        r.to_message(),
        Message::tool_results(vec![
            Content::ToolResult {
                tool_use_id: "c1".into(),
                content: "a.txt".into(),
                is_error: false,
                images: vec![],
            },
            Content::Text {
                text: "Time check".into(),
            },
        ]),
    ];
    let body = request_body(&request(history));
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[0], json!({"role": "system", "content": "SYSTEM"}));
    assert_eq!(msgs[1], json!({"role": "user", "content": "task"}));
    assert_eq!(
        msgs[2]["tool_calls"][0]["function"]["arguments"],
        "{\"cmd\":\"ls\"}"
    );
    assert_eq!(
        msgs[3],
        json!({"role": "tool", "tool_call_id": "c1", "content": "a.txt"})
    );
    assert_eq!(msgs[4], json!({"role": "user", "content": "Time check"}));
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["tools"][0]["function"]["name"], "exec");
}

#[test]
fn text_only_and_truncated_replies() {
    let s = [
        data(json!({"choices": [{"delta": {"content": "<tool_call>{}</tool_call>"}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    let r = parse(&s, 64).unwrap();
    assert_eq!(r.stop_reason, StopReason::EndTurn);
    assert!(r.tool_calls.is_empty());
    let s = data(json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "length"}]}));
    assert_eq!(parse(&s, 64).unwrap().stop_reason, StopReason::MaxTokens);
    let cut = data(json!({"choices": [{"delta": {"content": "x"}}]}));
    assert!(matches!(parse(&cut, 64), Err(ProviderError::Transient(_))));
}

#[tokio::test]
async fn completes_against_a_local_server_without_a_key() {
    let (url, server) = serve_once("200 OK", "text/event-stream", stream()).await;
    let p = CompatProvider::new(format!("{url}/v1"), None).unwrap();
    let r = p
        .complete(&request(vec![Message::user_text("task")]))
        .await
        .unwrap();
    assert_eq!(r.tool_calls.len(), 1);
    let raw = server.await.unwrap();
    assert!(raw.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(!raw.to_lowercase().contains("authorization:"));
    assert_eq!(body_of(&raw)["model"], "qwen-test");
}

#[test]
fn parses_a_recorded_ollama_stream() {
    let r = parse(include_str!("ollama-stream.txt"), 4096).unwrap();
    assert_eq!(r.tool_calls.len(), 1, "{r:?}");
    assert_eq!(r.tool_calls[0].name, "exec");
}

#[test]
fn sampling_settings_are_sent_when_set() {
    let mut req = request(vec![Message::user_text("x")]);
    assert!(request_body(&req).get("top_p").is_none());
    req.sampling = gitagent_loader::Sampling {
        top_p: Some(0.9),
        top_k: Some(40),
        stop_sequences: vec!["END".into()],
    };
    let body = request_body(&req);
    assert_eq!(body["top_p"].to_string(), "0.9");
    assert_eq!(body["top_k"], 40);
    assert_eq!(body["stop"], json!(["END"]));
}
