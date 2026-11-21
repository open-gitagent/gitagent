//! Asks the model for lesson operations from one or two finished attempts.

use std::time::Duration;

use anyhow::{bail, Result};
use gitagent_loader::AgentSpec;
use gitagent_provider::{CachePlan, ChatRequest, Message, Provider, ProviderError};
use llm_resilience::CircuitBreaker;
use serde::Deserialize;

use crate::store::{Op, Store};
use crate::trace::Trial;

const LEARNER_PROMPT: &str = include_str!("../../../prompts/learner.md");
const RETRIES: u32 = 3;

/// Consecutive transient or rate-limit failures, across every call of one
/// learning pass, that mark the provider as down: two units in a row that
/// used up all their retries.
const BREAKER_FAILURES: usize = (2 * RETRIES) as usize;
/// How long the provider counts as down before one call probes it again.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(60);

/// The breaker one learning pass shares across its model calls, so an
/// outage costs the retries of two units instead of every unit's.
pub fn breaker() -> CircuitBreaker {
    CircuitBreaker::new(BREAKER_FAILURES, 1.0, BREAKER_COOLDOWN)
}

/// The provider kept failing with transient errors and the breaker is open:
/// the call was not made.
#[derive(Debug)]
pub struct ProviderDown;

impl std::fmt::Display for ProviderDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the provider keeps failing; skipped without calling it")
    }
}

impl std::error::Error for ProviderDown {}

/// The learner's instructions without their header comment.
pub fn learner_prompt() -> &'static str {
    let text = LEARNER_PROMPT.trim_start();
    match text.strip_prefix("<!--").and_then(|t| t.split_once("-->")) {
        Some((_, rest)) => rest.trim(),
        None => text.trim(),
    }
}

/// The user message: the current lessons, then each attempt.
pub fn request_text(store: &Store, attempts: &[&Trial]) -> String {
    let mut out = String::from("# Current lessons\n\n");
    if store.lessons.is_empty() {
        out.push_str("(none yet)\n");
    }
    for l in &store.lessons {
        out.push_str(&format!("[{}] {}\n", l.id, l.text));
    }
    if attempts.len() > 1 {
        out.push_str("\nThese are attempts at the same task.\n");
    }
    for (i, t) in attempts.iter().enumerate() {
        let verdict = match t.reward {
            Some(_) if t.passed() => "accepted by the checker",
            Some(_) => "rejected by the checker",
            None => "not checked by anyone: judge it only from what the trajectory shows",
        };
        out.push_str(&format!(
            "\n# Attempt {} ({verdict})\n\n{}\n",
            (b'A' + i as u8) as char,
            t.trace.trim_end()
        ));
    }
    out
}

pub async fn propose(
    spec: &AgentSpec,
    provider: &dyn Provider,
    breaker: &CircuitBreaker,
    store: &Store,
    attempts: &[&Trial],
) -> Result<Vec<Op>> {
    let req = ChatRequest {
        model: spec.model.model.clone(),
        system: learner_prompt().to_string(),
        messages: vec![Message::user_text(request_text(store, attempts))],
        tools: Vec::new(),
        effort: spec.profile.effort,
        temperature: spec.profile.temperature,
        sampling: spec.profile.sampling.clone(),
        max_output_tokens: spec.profile.max_output_tokens,
        cache: CachePlan::default(),
        server_masking: None,
    };
    let mut attempt = 0;
    let resp = loop {
        attempt += 1;
        let Some(permit) = breaker.try_acquire() else {
            return Err(ProviderDown.into());
        };
        match provider.complete(&req).await {
            Ok(r) => {
                permit.success();
                break r;
            }
            Err(e @ (ProviderError::Transient(_) | ProviderError::RateLimited(_))) => {
                permit.failure();
                if attempt >= RETRIES {
                    return Err(e.into());
                }
                eprintln!("gitagent learn: retrying after a provider error: {e}");
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
            }
            // Fatal errors and context overflows say nothing about whether
            // the provider is up, so the breaker records nothing.
            Err(e) => return Err(e.into()),
        }
    };
    parse_ops(&resp.text)
}

#[derive(Deserialize)]
struct Reply {
    ops: Vec<Op>,
}

/// The `{"ops": [...]}` object in the reply, allowing text around it.
pub fn parse_ops(text: &str) -> Result<Vec<Op>> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        bail!("the learner's reply has no JSON object");
    };
    if end < start {
        bail!("the learner's reply has no JSON object");
    }
    let reply: Reply = serde_json::from_str(&text[start..=end])
        .map_err(|e| anyhow::anyhow!("the learner's reply is not valid: {e}"))?;
    Ok(reply.ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ops_with_text_around_them() {
        let ops = parse_ops(
            "Here you go:\n```json\n{\"ops\": [{\"op\": \"add\", \"text\": \"x y\"}, {\"op\": \"upvote\", \"id\": 3}]}\n```",
        )
        .unwrap();
        assert_eq!(
            ops,
            vec![Op::Add { text: "x y".into() }, Op::Upvote { id: 3 }]
        );
        assert_eq!(parse_ops("{\"ops\": []}").unwrap(), vec![]);
        assert!(parse_ops("nothing").is_err());
        assert!(parse_ops("{\"ops\": [{\"op\": \"delete\", \"id\": 1}]}").is_err());
    }

    #[test]
    fn the_prompt_has_no_header_comment() {
        assert!(learner_prompt().starts_with("You keep a short list"));
        assert!(!learner_prompt().contains("-->"));
    }
}
