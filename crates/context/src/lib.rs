//! Context management (BLUEPRINT §5): batched observation masking and
//! structured compaction.
//!
//! Masking edits history, which resets the prompt cache from the first edited
//! message onward, so it only happens at checkpoints (every
//! `mask_batch_tokens` of growth), never between them.

use std::collections::HashMap;

use gitagent_provider::{Content, Message, Role};

pub const MASK_PREFIX: &str = "[output hidden to save context";

/// Rough token estimate: about four bytes per token.
pub fn estimate_tokens(bytes: usize) -> u64 {
    bytes.div_ceil(4) as u64
}

/// Approximate size of messages as sent to a provider.
pub fn message_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|m| estimate_tokens(serde_json::to_string(m).map_or(0, |s| s.len())))
        .sum()
}

/// Replaces old tool results with short stubs, in batches.
pub struct Masker {
    batch_tokens: u64,
    keep_last: usize,
    checkpoint: u64,
    /// Stub text per tool call id, recorded when the result was produced.
    stubs: HashMap<String, String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MaskReport {
    pub masked: usize,
    pub tokens_saved: u64,
}

impl Masker {
    pub fn new(batch_tokens: u64, keep_last: usize) -> Self {
        Masker {
            batch_tokens: batch_tokens.max(1),
            keep_last,
            checkpoint: 0,
            stubs: HashMap::new(),
        }
    }

    /// Remembers what a result's stub should say (its status line and where
    /// the full output is).
    pub fn record(&mut self, tool_use_id: &str, stub: String) {
        self.stubs.insert(tool_use_id.to_string(), stub);
    }

    /// Masks once the prompt has grown by `batch_tokens` since the last
    /// checkpoint. `prompt_tokens` is the current size of the whole prompt.
    pub fn maybe_mask(&mut self, messages: &mut [Message], prompt_tokens: u64) -> MaskReport {
        if prompt_tokens < self.checkpoint + self.batch_tokens {
            return MaskReport::default();
        }
        let report = self.mask(messages);
        self.checkpoint = prompt_tokens.saturating_sub(report.tokens_saved);
        report
    }

    fn mask(&self, messages: &mut [Message]) -> MaskReport {
        let total_results: usize = messages
            .iter()
            .filter(|m| m.role == Role::User)
            .flat_map(|m| &m.content)
            .filter(|c| matches!(c, Content::ToolResult { .. }))
            .count();
        let to_mask = total_results.saturating_sub(self.keep_last);
        let mut seen = 0;
        let mut report = MaskReport::default();
        for msg in messages.iter_mut().filter(|m| m.role == Role::User) {
            for c in msg.content.iter_mut() {
                let Content::ToolResult {
                    tool_use_id,
                    content,
                    images,
                    ..
                } = c
                else {
                    continue;
                };
                if seen >= to_mask {
                    return report;
                }
                seen += 1;
                if content.starts_with(MASK_PREFIX) {
                    continue;
                }
                let stub = self
                    .stubs
                    .get(tool_use_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| {
                        format!("{MASK_PREFIX}; run the call again if you need it]")
                    });
                let image_bytes: usize = images.iter().map(|i| i.data.len()).sum();
                if content.len() + image_bytes <= stub.len() {
                    continue;
                }
                report.tokens_saved += estimate_tokens(content.len() + image_bytes - stub.len());
                report.masked += 1;
                *content = stub;
                images.clear();
            }
        }
        report
    }
}

/// Asks the model for the structured summary that replaces the history.
pub const COMPACTION_REQUEST: &str = "The conversation is about to be shortened to save context. \
Write a summary that lets you continue the task from it alone. Do not call any tools. \
Use exactly these sections:\n\
DONE: what has been completed and verified.\n\
PENDING: what is left to do, in order.\n\
KEY FILES: paths created or changed, and important paths you found, one per line.\n\
LAST FAILURE: the most recent error or failing check, with the exact message.\n\
RULED OUT: approaches that did not work and why.\n\
The original task will be repeated verbatim above your summary, so do not restate it.";

/// The first user message after compaction: the original first message
/// (task, and snapshot if any) followed by the summary.
pub fn compacted_message(first_user: &str, summary: &str) -> String {
    format!(
        "{}\n# Progress so far\n\nEarlier turns were replaced by this summary to save context. Full outputs are still in the spill files it mentions.\n\n{}\n",
        first_user.trim_end(),
        summary.trim()
    )
}

/// A summary built without the model, used when the summary request itself
/// fails: the most recent tool calls with the first line of their results.
pub fn mechanical_summary(messages: &[Message], last: usize) -> String {
    let mut calls: Vec<(String, String)> = Vec::new();
    let mut results: HashMap<&str, &str> = HashMap::new();
    for m in messages {
        for c in &m.content {
            match c {
                Content::ToolUse { id, name, input } => calls.push((
                    id.clone(),
                    format!("{name} {}", serde_json::Value::Object(input.clone())),
                )),
                Content::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    results.insert(tool_use_id, content);
                }
                _ => {}
            }
        }
    }
    let start = calls.len().saturating_sub(last);
    let mut out = String::from(
        "RECENT ACTIONS (a summary could not be written; these are the last calls):\n",
    );
    for (id, call) in &calls[start..] {
        let call: String = call.chars().take(300).collect();
        let result = results
            .get(id.as_str())
            .and_then(|r| r.lines().last())
            .unwrap_or("");
        out.push_str(&format!("- {call}\n  → {result}\n"));
    }
    out
}

#[cfg(test)]
mod tests;
