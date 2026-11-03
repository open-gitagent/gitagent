use super::*;
use serde_json::json;

fn call(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolUse {
            id: id.into(),
            name: "exec".into(),
            input: json!({"cmd": format!("run {id}")})
                .as_object()
                .unwrap()
                .clone(),
        }],
        raw: None,
    }
}

fn result(id: &str, size: usize) -> Message {
    Message::tool_results(vec![Content::ToolResult {
        tool_use_id: id.into(),
        content: format!("{}\n[exit 0 · 1.0s]", "x".repeat(size)),
        is_error: false,
        images: vec![],
    }])
}

fn history(n: usize, size: usize) -> Vec<Message> {
    let mut msgs = vec![Message::user_text("task")];
    for i in 0..n {
        msgs.push(call(&format!("c{i}")));
        msgs.push(result(&format!("c{i}"), size));
    }
    msgs
}

fn contents(msgs: &[Message]) -> Vec<String> {
    msgs.iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn masks_all_but_the_last_results_once_the_batch_is_reached() {
    let mut m = Masker::new(1000, 2);
    for i in 0..5 {
        m.record(
            &format!("c{i}"),
            format!("{MASK_PREFIX}; was exit 0; full output: out/{i}.log]"),
        );
    }
    let mut msgs = history(5, 2000);
    // Below the batch size: nothing changes.
    assert_eq!(m.maybe_mask(&mut msgs, 900), MaskReport::default());

    let report = m.maybe_mask(&mut msgs, 3000);
    assert_eq!(report.masked, 3);
    assert!(report.tokens_saved > 1400);
    let c = contents(&msgs);
    assert_eq!(
        c[0],
        format!("{MASK_PREFIX}; was exit 0; full output: out/0.log]")
    );
    assert!(c[2].starts_with(MASK_PREFIX));
    assert!(c[3].starts_with("xxx"));
    assert!(c[4].starts_with("xxx"));
    // Tool calls and the task are untouched.
    assert_eq!(msgs[0], Message::user_text("task"));
    assert_eq!(msgs[1], call("c0"));

    // The next checkpoint is a full batch away from the reduced size.
    assert_eq!(m.maybe_mask(&mut msgs, 3100), MaskReport::default());
}

#[test]
fn short_results_and_already_masked_ones_are_left_alone() {
    let mut m = Masker::new(10, 0);
    let mut msgs = history(2, 3);
    assert_eq!(m.maybe_mask(&mut msgs, 100).masked, 0);
    let mut msgs = history(2, 500);
    assert_eq!(m.maybe_mask(&mut msgs, 1000).masked, 2);
    let before = msgs.clone();
    assert_eq!(m.maybe_mask(&mut msgs, 5000).masked, 0);
    assert_eq!(msgs, before);
    // Without a recorded stub, a generic one is used.
    assert!(contents(&msgs)[0].contains("run the call again"));
}

#[test]
fn compaction_message_and_mechanical_summary() {
    let msg = compacted_message("# Task\n\nDo X.\n", "DONE: a\nPENDING: b");
    assert!(msg.starts_with("# Task\n\nDo X.\n# Progress so far\n"));
    assert!(msg.ends_with("DONE: a\nPENDING: b\n"));

    let s = mechanical_summary(&history(4, 10), 2);
    assert!(s.contains("run c2"));
    assert!(s.contains("run c3"));
    assert!(!s.contains("run c1"));
    assert!(s.contains("→ [exit 0 · 1.0s]"));
}

#[test]
fn estimates_grow_with_content() {
    assert!(message_tokens(&history(3, 4000)) > message_tokens(&history(3, 10)) + 2000);
}
