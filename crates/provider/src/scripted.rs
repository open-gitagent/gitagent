//! A provider that replays responses from a YAML script. Used by tests and by
//! `--model scripted:<path>` for dry runs without network access.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;
use gitagent_loader::Family;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{ChatRequest, ChatResponse, Provider, ProviderError, StopReason, ToolCall, Usage};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptedTurn {
    #[serde(default)]
    text: String,
    #[serde(default)]
    tool_calls: Vec<ScriptedCall>,
    /// Simulates a safety-classifier refusal.
    #[serde(default)]
    refusal: bool,
    /// Simulates a provider error: `transient`, `rate_limited`,
    /// `context_overflow` or `fatal`.
    #[serde(default)]
    error: Option<String>,
    /// How long the reply takes, in milliseconds, so a demo shows the
    /// model thinking.
    #[serde(default)]
    delay_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptedCall {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

pub struct ScriptedProvider {
    turns: Vec<ScriptedTurn>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next: usize,
    requests: Vec<ChatRequest>,
}

impl ScriptedProvider {
    /// Parses a script: a YAML list of turns, each with optional `text`,
    /// `tool_calls: [{name, arguments}]`, `refusal: true` or `error: <kind>`.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let turns: Vec<ScriptedTurn> =
            serde_saphyr::from_str(yaml).context("parsing scripted provider fixture")?;
        Ok(ScriptedProvider {
            turns,
            state: Mutex::new(State::default()),
        })
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let yaml =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_yaml(&yaml)
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.state.lock().unwrap().requests.clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let (index, turn) = {
            let mut state = self.state.lock().unwrap();
            state.requests.push(req.clone());
            let index = state.next;
            let turn = self
                .turns
                .get(index)
                .ok_or_else(|| ProviderError::Fatal(format!("script ended after {index} turns")))?;
            state.next += 1;
            (index, turn)
        };
        // The lock is released, so other callers (parallel sub-agents) go on.
        if turn.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(turn.delay_ms)).await;
        }
        if let Some(kind) = &turn.error {
            return Err(match kind.as_str() {
                "transient" => ProviderError::Transient("scripted".into()),
                "rate_limited" => ProviderError::RateLimited("scripted".into()),
                "context_overflow" => ProviderError::ContextOverflow("scripted".into()),
                _ => ProviderError::Fatal("scripted".into()),
            });
        }

        let tool_calls: Vec<ToolCall> = turn
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, c)| ToolCall {
                id: format!("call_{}_{}", index + 1, i + 1),
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            })
            .collect();
        let prompt_bytes: usize = req.system.len()
            + req
                .messages
                .iter()
                .map(|m| serde_json::to_string(m).map_or(0, |s| s.len()))
                .sum::<usize>();
        Ok(ChatResponse {
            model: None,
            text: turn.text.clone(),
            reasoning: None,
            stop_reason: if turn.refusal {
                StopReason::Refusal
            } else if tool_calls.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            },
            tool_calls,
            usage: Usage {
                input_tokens: (prompt_bytes / 4) as u64,
                output_tokens: (turn.text.len() / 4) as u64 + 10,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            raw: None,
            cleared_tool_results: 0,
        })
    }

    fn family(&self) -> Family {
        Family::Compat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CachePlan, Message};
    use gitagent_loader::Effort;

    fn request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![Message::user_text("task")],
            tools: vec![],
            effort: Effort::High,
            temperature: None,
            max_output_tokens: 100,
            cache: CachePlan::default(),
            server_masking: None,
        }
    }

    #[tokio::test]
    async fn replays_turns_then_errors() {
        let p = ScriptedProvider::from_yaml(
            "- text: looking\n  tool_calls:\n    - name: exec\n      arguments: {cmd: echo hi, yield_ms: 500}\n- tool_calls:\n    - name: task_complete\n",
        )
        .unwrap();
        let r1 = p.complete(&request()).await.unwrap();
        assert_eq!(r1.text, "looking");
        assert_eq!(r1.tool_calls[0].name, "exec");
        assert_eq!(r1.tool_calls[0].arguments["cmd"], "echo hi");
        assert_eq!(r1.tool_calls[0].arguments["yield_ms"], 500);
        assert_eq!(r1.stop_reason, StopReason::ToolUse);
        let r2 = p.complete(&request()).await.unwrap();
        assert_eq!(r2.tool_calls[0].name, "task_complete");
        assert!(matches!(
            p.complete(&request()).await,
            Err(ProviderError::Fatal(_))
        ));
        assert_eq!(p.requests().len(), 3);
    }
}
