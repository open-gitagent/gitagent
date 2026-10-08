//! The agent loop — a faithful port of pi-agent-core's `runLoop` +
//! `executeToolCalls`. Tool calls run concurrently (`join_all`) unless the
//! batch contains any Sequential tool (or the agent is configured Sequential),
//! in which case the whole batch is serialized — exactly pi's rule.

use crate::pi::compact::Compactor;
use crate::pi::event::AgentEvent;
use crate::pi::gate::{GateDecision, ToolGate};
use crate::pi::message::{AgentMessage, StopReason, ToolResultMessage};
use crate::pi::provider::{self, GenParams, ModelSpec};
use crate::pi::tool::{AgentTool, ExecutionMode};
use futures_util::future::join_all;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

pub struct LoopContext {
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    /// Starting transcript, including the freshly-added user prompt.
    pub messages: Vec<AgentMessage>,
}

pub struct LoopConfig {
    pub client: reqwest::Client,
    /// Preferred model first, then fallbacks (tried in order on error).
    pub specs: Vec<ModelSpec>,
    pub tool_execution: ExecutionMode,
    pub steering: Arc<Mutex<VecDeque<AgentMessage>>>,
    pub follow_up: Arc<Mutex<VecDeque<AgentMessage>>>,
    /// Context-window management (transformContext). Applied to the messages
    /// sent to the model each turn, not to the stored transcript.
    pub compactor: Option<Compactor>,
    /// Sampling constraints forwarded to the provider.
    pub params: GenParams,
    /// beforeToolCall gates (permissions, script hooks). Checked in order.
    pub gates: Vec<Arc<dyn ToolGate>>,
    /// Hard cap on assistant turns per run (0 = unlimited).
    pub max_turns: u32,
    /// Transient-error retries per model before falling back.
    pub max_retries: u32,
    /// Per-tool execution timeout.
    pub tool_timeout: Duration,
}

async fn emit(tx: &Sender<AgentEvent>, ev: AgentEvent) {
    let _ = tx.send(ev).await;
}

fn drain(q: &Arc<Mutex<VecDeque<AgentMessage>>>) -> Vec<AgentMessage> {
    q.lock().unwrap().drain(..).collect()
}

/// Run the loop; returns the messages appended during this run.
pub async fn run_loop(
    mut ctx: LoopContext,
    config: LoopConfig,
    cancel: CancellationToken,
    tx: Sender<AgentEvent>,
) -> Vec<AgentMessage> {
    let mut new_messages: Vec<AgentMessage> = Vec::new();

    emit(&tx, AgentEvent::AgentStart).await;
    emit(&tx, AgentEvent::TurnStart).await;

    let mut first_turn = true;
    let mut turns = 0u32; // total assistant turns this run (for max_turns)
    let mut continues = 0u32; // auto-continues after max_tokens cuts (bounded)
    let mut pending = drain(&config.steering);

    'outer: loop {
        let mut has_more = true;
        while has_more || !pending.is_empty() {
            if !first_turn {
                emit(&tx, AgentEvent::TurnStart).await;
            } else {
                first_turn = false;
            }

            // Inject queued steering messages before the next assistant turn.
            for m in std::mem::take(&mut pending) {
                if let AgentMessage::User(s) = &m {
                    emit(&tx, AgentEvent::UserMessage(s.clone())).await;
                }
                ctx.messages.push(m.clone());
                new_messages.push(m);
            }

            if cancel.is_cancelled() {
                emit(&tx, AgentEvent::AgentEnd).await;
                return new_messages;
            }

            // Enforce the per-run turn cap (prevents runaway loops).
            if config.max_turns > 0 && turns >= config.max_turns {
                emit(&tx, AgentEvent::TurnEnd).await;
                emit(&tx, AgentEvent::AgentEnd).await;
                return new_messages;
            }
            turns += 1;

            // transformContext: compact the messages SENT to the model (the
            // stored transcript is untouched). Summarizes the aged head with the
            // preferred model when over budget; truncates as a fallback.
            let llm_messages = match &config.compactor {
                Some(c) => c.compact(&config.client, config.specs.first(), &ctx.messages, &cancel).await,
                None => ctx.messages.clone(),
            };
            let assistant = provider::stream_assistant_resilient(
                &config.client,
                &config.specs,
                &ctx.system_prompt,
                &llm_messages,
                &ctx.tools,
                &config.params,
                config.max_retries,
                &cancel,
                &tx,
            )
            .await;
            emit(&tx, AgentEvent::MessageEnd(assistant.clone())).await;

            let stop = assistant.stop_reason.clone();
            let tool_calls = assistant.tool_calls();
            ctx.messages.push(AgentMessage::Assistant(assistant.clone()));
            new_messages.push(AgentMessage::Assistant(assistant));

            if stop == StopReason::Error || stop == StopReason::Aborted {
                emit(&tx, AgentEvent::TurnEnd).await;
                emit(&tx, AgentEvent::AgentEnd).await;
                return new_messages;
            }

            has_more = false;
            if !tool_calls.is_empty() {
                let (results, terminate) =
                    execute_tool_calls(&ctx.tools, &tool_calls, &config, &cancel, &tx).await;
                has_more = !terminate;
                for r in results {
                    ctx.messages.push(r.clone());
                    new_messages.push(r);
                }
            } else if stop == StopReason::Length && continues < 3 {
                // The output was cut by max_tokens mid-generation — auto-continue
                // (bounded) so long answers don't strand the user mid-line.
                continues += 1;
                let nudge = AgentMessage::User(
                    "Continue EXACTLY where you stopped. Output only the remainder — no repetition, no preamble.".into(),
                );
                ctx.messages.push(nudge.clone());
                new_messages.push(nudge);
                has_more = true;
            }

            emit(&tx, AgentEvent::TurnEnd).await;
            pending = drain(&config.steering);
        }

        // Agent would stop — check for follow-up messages.
        let follow = drain(&config.follow_up);
        if !follow.is_empty() {
            pending = follow;
            continue 'outer;
        }
        break;
    }

    emit(&tx, AgentEvent::AgentEnd).await;
    new_messages
}

async fn execute_tool_calls(
    tools: &[Arc<dyn AgentTool>],
    calls: &[(String, String, Value)],
    config: &LoopConfig,
    cancel: &CancellationToken,
    tx: &Sender<AgentEvent>,
) -> (Vec<AgentMessage>, bool) {
    let has_sequential = calls.iter().any(|(_, name, _)| {
        tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.execution_mode() == ExecutionMode::Sequential)
            .unwrap_or(false)
    });

    if config.tool_execution == ExecutionMode::Sequential || has_sequential {
        execute_sequential(tools, &config.gates, config.tool_timeout, calls, cancel, tx).await
    } else {
        execute_parallel(tools, &config.gates, config.tool_timeout, calls, cancel, tx).await
    }
}

async fn run_one(
    tools: &[Arc<dyn AgentTool>],
    gates: &[Arc<dyn ToolGate>],
    tool_timeout: Duration,
    id: &str,
    name: &str,
    args: Value,
    cancel: &CancellationToken,
) -> (String, bool, bool) {
    // beforeToolCall: gates may rewrite args or block the call outright.
    let mut args = args;
    for g in gates {
        // A gate (e.g. a script hook) must not outlive an abort; dropping its
        // future kills any process it started.
        let decision = tokio::select! {
            _ = cancel.cancelled() => return ("(aborted)".to_string(), true, false),
            d = g.check(name, &args) => d,
        };
        match decision {
            GateDecision::Allow => {}
            GateDecision::Modify(v) => args = v,
            GateDecision::Deny(msg) => return (format!("Blocked: {msg}"), true, false),
        }
    }
    match tools.iter().find(|t| t.name() == name) {
        None => (format!("Error: tool '{name}' not found"), true, false),
        // A hung tool must never wedge the loop — cap every call. On timeout the
        // future is dropped, so kill_on_drop reaps any child process.
        Some(tool) => match tokio::time::timeout(tool_timeout, tool.execute(id, args, cancel)).await {
            Ok(Ok(r)) => (r.content, false, r.terminate),
            Ok(Err(e)) => (format!("Error: {e}"), true, false),
            Err(_) => (format!("Error: tool '{name}' timed out after {}s", tool_timeout.as_secs()), true, false),
        },
    }
}

async fn execute_sequential(
    tools: &[Arc<dyn AgentTool>],
    gates: &[Arc<dyn ToolGate>],
    tool_timeout: Duration,
    calls: &[(String, String, Value)],
    cancel: &CancellationToken,
    tx: &Sender<AgentEvent>,
) -> (Vec<AgentMessage>, bool) {
    let mut messages = Vec::new();
    let mut all_terminate = !calls.is_empty();
    for (id, name, args) in calls {
        emit(tx, AgentEvent::ToolExecutionStart { tool_call_id: id.clone(), tool_name: name.clone(), args: args.clone() }).await;
        let (content, is_error, terminate) = run_one(tools, gates, tool_timeout, id, name, args.clone(), cancel).await;
        emit(tx, AgentEvent::ToolExecutionEnd { tool_call_id: id.clone(), tool_name: name.clone(), content: content.clone(), is_error }).await;
        if !terminate {
            all_terminate = false;
        }
        messages.push(AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: id.clone(),
            tool_name: name.clone(),
            content,
            is_error,
        }));
    }
    (messages, all_terminate)
}

async fn execute_parallel(
    tools: &[Arc<dyn AgentTool>],
    gates: &[Arc<dyn ToolGate>],
    tool_timeout: Duration,
    calls: &[(String, String, Value)],
    cancel: &CancellationToken,
    tx: &Sender<AgentEvent>,
) -> (Vec<AgentMessage>, bool) {
    // Preflight: announce every call first (pi prepares sequentially, executes concurrently).
    for (id, name, args) in calls {
        emit(tx, AgentEvent::ToolExecutionStart { tool_call_id: id.clone(), tool_name: name.clone(), args: args.clone() }).await;
    }

    let futures = calls.iter().map(|(id, name, args)| {
        let (id, name, args) = (id.clone(), name.clone(), args.clone());
        async move {
            let (content, is_error, terminate) = run_one(tools, gates, tool_timeout, &id, &name, args, cancel).await;
            (id, name, content, is_error, terminate)
        }
    });
    let results = join_all(futures).await;

    let mut messages = Vec::new();
    let mut all_terminate = !results.is_empty();
    for (id, name, content, is_error, terminate) in results {
        emit(tx, AgentEvent::ToolExecutionEnd { tool_call_id: id.clone(), tool_name: name.clone(), content: content.clone(), is_error }).await;
        if !terminate {
            all_terminate = false;
        }
        messages.push(AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: id,
            tool_name: name,
            content,
            is_error,
        }));
    }
    (messages, all_terminate)
}
