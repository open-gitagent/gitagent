//! Stateful wrapper around the loop — owns the transcript + steering/follow-up
//! queues + the active abort token. Faithful to pi-agent-core's `Agent`.

use crate::pi::agent_loop::{run_loop, LoopConfig, LoopContext};
use crate::pi::compact::Compactor;
use crate::pi::event::AgentEvent;
use crate::pi::gate::ToolGate;
use crate::pi::message::{AgentMessage, AssistantMessage, StopReason};
use crate::pi::provider::{model_context_window, resolve_model, GenParams, ModelSpec};
use crate::pi::tool::{AgentTool, ExecutionMode};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub struct Agent {
    system_prompt: String,
    tools: Vec<Arc<dyn AgentTool>>,
    messages: Arc<Mutex<Vec<AgentMessage>>>,
    steering: Arc<Mutex<VecDeque<AgentMessage>>>,
    follow_up: Arc<Mutex<VecDeque<AgentMessage>>>,
    /// The in-flight turn's (id, cancel token).
    cancel: Arc<Mutex<Option<(u64, CancellationToken)>>>,
    /// Set while a run is in flight; a second `prompt` is rejected.
    busy: Arc<AtomicBool>,
    turn_ids: AtomicU64,
    client: reqwest::Client,
    /// Preferred model first, then fallbacks (tried in order on error).
    specs: Vec<ModelSpec>,
    tool_execution: ExecutionMode,
    context_window: usize,
    params: GenParams,
    gates: Vec<Arc<dyn ToolGate>>,
    max_turns: u32,
    max_retries: u32,
    tool_timeout: Duration,
}

impl Agent {
    pub fn new(system_prompt: String, tools: Vec<Arc<dyn AgentTool>>, model: &str) -> Self {
        // A dead connection must never hang the agent — bound connect time. No
        // total-request timeout: that would abort long legitimate streams.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        // Tool call ceiling (env-overridable) — defends against a hung tool.
        let tool_timeout = std::env::var("GITAGENT_TOOL_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(600));
        let spec = resolve_model(model);
        // Size the compaction window to the actual model.
        let context_window = model_context_window(&spec.model_id);
        Self {
            system_prompt,
            tools,
            messages: Arc::new(Mutex::new(Vec::new())),
            steering: Arc::new(Mutex::new(VecDeque::new())),
            follow_up: Arc::new(Mutex::new(VecDeque::new())),
            cancel: Arc::new(Mutex::new(None)),
            busy: Arc::new(AtomicBool::new(false)),
            turn_ids: AtomicU64::new(0),
            client,
            specs: vec![spec],
            // Default parallel (like pi); per-tool Sequential still serializes
            // any batch containing a mutating tool.
            tool_execution: ExecutionMode::Parallel,
            context_window,
            params: GenParams::default(),
            gates: Vec::new(),
            max_turns: 50,
            max_retries: 3,
            tool_timeout,
        }
    }

    /// Override the assumed model context window (used for compaction).
    pub fn with_context_window(mut self, window: usize) -> Self {
        self.context_window = window;
        self
    }

    /// Append fallback models, tried in order when the preferred one errors.
    pub fn with_fallbacks(mut self, models: &[String]) -> Self {
        for m in models.iter().filter(|m| !m.is_empty()) {
            self.specs.push(resolve_model(m));
        }
        self
    }

    /// Hard cap on assistant turns per run (0 = unlimited).
    pub fn with_max_turns(mut self, n: u32) -> Self {
        self.max_turns = n;
        self
    }

    /// Sampling constraints forwarded to the provider.
    pub fn with_params(mut self, params: GenParams) -> Self {
        self.params = params;
        self
    }

    /// beforeToolCall gates (permissions, script hooks).
    pub fn with_gates(mut self, gates: Vec<Arc<dyn ToolGate>>) -> Self {
        self.gates = gates;
        self
    }

    /// Start a run from a user prompt. Returns the lifecycle event stream. The
    /// run executes on a background task and appends to the transcript on finish.
    pub fn prompt(&self, user: String) -> ReceiverStream<AgentEvent> {
        self.prompt_with_cancel(user).0
    }

    /// Like [`prompt`](Self::prompt), also returning this turn's own cancel
    /// token (cancelling it can never affect a later turn). Only one run may
    /// be in flight per Agent: a concurrent call gets an immediate error turn
    /// and leaves the transcript untouched.
    pub fn prompt_with_cancel(&self, user: String) -> (ReceiverStream<AgentEvent>, CancellationToken) {
        let cancel = CancellationToken::new();
        let (tx, rx) = mpsc::channel(64);

        if self.busy.swap(true, Ordering::SeqCst) {
            let msg = AssistantMessage::failure(
                StopReason::Error,
                "agent is busy: a previous message is still running (wait for it or abort it)",
            );
            let _ = tx.try_send(AgentEvent::AgentStart);
            let _ = tx.try_send(AgentEvent::MessageEnd(msg));
            let _ = tx.try_send(AgentEvent::AgentEnd);
            return (ReceiverStream::new(rx), cancel);
        }
        let turn_id = self.turn_ids.fetch_add(1, Ordering::SeqCst);
        *self.cancel.lock().unwrap() = Some((turn_id, cancel.clone()));

        // Persist the user turn to the transcript, then seed the run from it.
        // (run_loop returns only the messages IT appends — assistant/tool/steer —
        // so the user message must be recorded here or multi-turn loses it.)
        let start = {
            let mut m = self.messages.lock().unwrap();
            m.push(AgentMessage::User(user));
            m.clone()
        };

        let ctx = LoopContext {
            system_prompt: self.system_prompt.clone(),
            tools: self.tools.clone(),
            messages: start,
        };
        let config = LoopConfig {
            client: self.client.clone(),
            specs: self.specs.clone(),
            tool_execution: self.tool_execution,
            steering: self.steering.clone(),
            follow_up: self.follow_up.clone(),
            compactor: Some(Compactor::new(self.context_window)),
            params: self.params.clone(),
            gates: self.gates.clone(),
            max_turns: self.max_turns,
            max_retries: self.max_retries,
            tool_timeout: self.tool_timeout,
        };
        let messages_arc = self.messages.clone();
        let (busy, slot, run_cancel) = (self.busy.clone(), self.cancel.clone(), cancel.clone());

        // Keep a second sender alive so the channel (and thus the returned
        // stream) stays open until AFTER the transcript is committed — otherwise
        // a follow-up turn could read stale messages (multi-turn Session).
        let hold = tx.clone();
        tokio::spawn(async move {
            let new_messages = run_loop(ctx, config, run_cancel, tx).await;
            messages_arc.lock().unwrap().extend(new_messages);
            {
                let mut s = slot.lock().unwrap();
                if s.as_ref().map(|(id, _)| *id) == Some(turn_id) {
                    *s = None;
                }
            }
            busy.store(false, Ordering::SeqCst);
            drop(hold);
        });

        (ReceiverStream::new(rx), cancel)
    }

    /// Queue a message to be injected after the current assistant turn.
    pub fn steer(&self, message: String) {
        self.steering.lock().unwrap().push_back(AgentMessage::User(message));
    }

    /// Queue a message to run only after the agent would otherwise stop.
    pub fn follow_up(&self, message: String) {
        self.follow_up.lock().unwrap().push_back(AgentMessage::User(message));
    }

    /// Abort the active run, if any.
    pub fn abort(&self) {
        if let Some((_, c)) = self.cancel.lock().unwrap().as_ref() {
            c.cancel();
        }
    }
}
