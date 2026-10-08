//! The gitagent SDK surface: `query(opts) -> QueryStream` (a `Stream<Event>`
//! mapped from the engine's lifecycle events), with `steer`/`abort`. Dropping
//! the stream aborts the run.

use crate::pi::agent::Agent;
use crate::pi::event::{AgentEvent, DeltaKind};
use crate::pi::gate::ToolGate;
use crate::pi::message::StopReason;
use crate::pi::provider::GenParams;
use crate::sdk::hooks::load_hook_gate;
use crate::sdk::manifest::PermissionConfig;
use crate::sdk::permissions::PermissionGate;
use crate::sdk::session::{init_local_session, LocalSession, RepoOptions};
use crate::sdk::telemetry::Telemetry;
use crate::sdk::proc::blocking;
use crate::sdk::tools::{builtin_tools_with, DelegationPolicy, ToolOptions};
use crate::sdk::loader;
use futures_util::Stream;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

/// What the SDK surfaces to consumers.
#[derive(Debug, Clone)]
pub enum Event {
    /// A streamed chunk of the assistant's answer.
    Delta(String),
    /// A streamed chunk of the assistant's reasoning (reasoning models).
    Thinking(String),
    ToolCall { name: String, args: Value },
    ToolResult { name: String, content: String, is_error: bool },
    Done,
    Error(String),
}

pub struct QueryOptions {
    /// Local agent directory (used when `repo` is None).
    pub dir: PathBuf,
    pub model: Option<String>,
    pub prompt: String,
    /// Clone + run against a GitHub repo on a session branch.
    pub repo: Option<RepoOptions>,
    /// Override the permission mode (plan | acceptEdits | bypass | default).
    pub permission_mode: Option<String>,
}

/// Cumulative token + cost totals for a run.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

pub struct QueryStream {
    inner: ReceiverStream<AgentEvent>,
    agent: Arc<Agent>,
    /// This run's own cancel token.
    cancel: CancellationToken,
    /// Present in repo mode; finalized (commit/push + PAT scrub) once, on Done or Drop.
    session: Option<LocalSession>,
    usage: RunUsage,
    telemetry: Option<Telemetry>,
}

impl QueryStream {
    /// Inject a message mid-run (real steering — reaches the engine queue).
    pub fn steer(&self, message: impl Into<String>) {
        self.agent.steer(message.into());
    }
    /// Cancel the run.
    pub fn abort(&self) {
        self.agent.abort();
    }
    /// Cumulative token + cost totals observed so far.
    pub fn usage(&self) -> RunUsage {
        self.usage
    }
    fn finalize_session(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = blocking(|| s.finalize());
        }
    }
}

impl Drop for QueryStream {
    fn drop(&mut self) {
        // Breaking out of the stream cancels the underlying run (no runaway spend),
        // then commits/pushes (repo mode).
        self.cancel.cancel();
        self.finalize_session();
    }
}

impl Stream for QueryStream {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(ev)) => {
                    let is_end = matches!(ev, AgentEvent::AgentEnd);
                    let mapped = account_and_map(ev, &mut this.usage, &this.telemetry);
                    if is_end {
                        this.finalize_session(); // commit/push the session branch
                    }
                    if let Some(m) = mapped {
                        return Poll::Ready(Some(m));
                    }
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Accumulate usage + emit telemetry for one engine event, then map it to a
/// public `Event` (returns None for internal events).
fn account_and_map(ev: AgentEvent, usage: &mut RunUsage, telemetry: &Option<Telemetry>) -> Option<Event> {
    if let AgentEvent::MessageEnd(a) = &ev {
        usage.input_tokens += a.usage.input;
        usage.output_tokens += a.usage.output;
        usage.cost_usd += a.usage.cost_usd;
        if let Some(t) = telemetry {
            t.record("usage", json!({ "in": a.usage.input, "out": a.usage.output, "cost": a.usage.cost_usd }));
        }
    }
    if let Some(t) = telemetry {
        match &ev {
            AgentEvent::ToolExecutionStart { tool_name, args, .. } => {
                t.record("tool_call", json!({ "name": tool_name, "args": args }));
            }
            AgentEvent::ToolExecutionEnd { tool_name, is_error, .. } => {
                t.record("tool_result", json!({ "name": tool_name, "is_error": is_error }));
            }
            AgentEvent::AgentEnd => t.record("done", json!({ "cost": usage.cost_usd })),
            _ => {}
        }
    }
    map_event(ev)
}

fn map_event(ev: AgentEvent) -> Option<Event> {
    match ev {
        AgentEvent::MessageDelta { kind: DeltaKind::Text, text } => Some(Event::Delta(text)),
        AgentEvent::MessageDelta { kind: DeltaKind::Thinking, text } => Some(Event::Thinking(text)),
        AgentEvent::MessageEnd(a) if a.stop_reason == StopReason::Error => {
            Some(Event::Error(a.error_message.unwrap_or_else(|| "model error".into())))
        }
        AgentEvent::ToolExecutionStart { tool_name, args, .. } => {
            Some(Event::ToolCall { name: tool_name, args })
        }
        AgentEvent::ToolExecutionEnd { tool_name, content, is_error, .. } => {
            Some(Event::ToolResult { name: tool_name, content, is_error })
        }
        AgentEvent::AgentEnd => Some(Event::Done),
        _ => None,
    }
}

/// Build a configured Agent (loading, gates, params, fallbacks) plus the repo
/// session + telemetry. Shared by one-shot `query` and multi-turn `open_session`.
fn build_agent(
    dir: PathBuf,
    model: Option<String>,
    repo: Option<RepoOptions>,
    permission_mode: Option<String>,
) -> anyhow::Result<(Arc<Agent>, Option<LocalSession>, Option<Telemetry>)> {
    // Repo mode: clone + branch, then load the agent from the working copy.
    // A cloned repo is untrusted unless the user opted in (--trust-repo):
    // its mcp.yaml servers, hooks.yaml and tools/*.yaml are not run.
    let (dir, session, trusted) = match repo {
        Some(repo) => {
            let trusted = repo.trust_repo;
            let s = init_local_session(repo)?;
            (s.dir.clone(), Some(s), trusted)
        }
        None => (dir, None, true),
    };
    if !trusted {
        let skipped: Vec<&str> = ["mcp.yaml", "hooks.yaml", "tools"]
            .into_iter()
            .filter(|f| dir.join(f).exists())
            .collect();
        if !skipped.is_empty() {
            eprintln!(
                "note: not running repository-supplied {} (pass --trust-repo or set GITAGENT_TRUST_REPO=1 to allow)",
                skipped.join(", ")
            );
        }
    }

    let loaded = loader::load_agent(&dir)?;
    let model = model.unwrap_or_else(|| loaded.manifest.model.preferred.clone());
    // Tool allow-list: omitted → all; `[]` → none (tool-less models); list → those.
    let delegation = DelegationPolicy { permission_mode: permission_mode.clone(), ..Default::default() };
    let tool_opts = ToolOptions { skip_agent_executables: !trusted, delegation: delegation.clone() };
    let mut tools = builtin_tools_with(&dir, &tool_opts);
    if let Some(allow) = loaded.manifest.tools.as_ref() {
        tools.retain(|t| allow.iter().any(|a| a == t.name()));
    }

    // Sampling constraints (manifest.model.constraints → provider request).
    let params = loaded
        .manifest
        .model
        .constraints
        .as_ref()
        .map(|c| GenParams { temperature: c.temperature, max_tokens: c.max_tokens, top_p: c.top_p })
        .unwrap_or_default();

    // Gates: permission rules/modes + script hooks. Mutating tools = those the
    // engine serializes (Sequential) plus delegation (run_agent).
    let mutating: HashSet<String> =
        tools.iter().filter(|t| t.is_mutating()).map(|t| t.name().to_string()).collect();
    let mut gates: Vec<Arc<dyn ToolGate>> = Vec::new();
    let perm_cfg = loaded.manifest.permissions.clone().unwrap_or_else(PermissionConfig::default);
    let perm = PermissionGate::new(&perm_cfg, mutating, permission_mode.as_deref()).with_root(&dir);
    if !perm.is_noop() {
        gates.push(Arc::new(perm));
    }
    if trusted {
        if let Some(hook) = load_hook_gate(&dir) {
            gates.push(Arc::new(hook));
        }
    }
    // Sub-agents (run_agent) inherit exactly these gates.
    *delegation.gates.write().unwrap_or_else(|e| e.into_inner()) = gates.clone();

    let telemetry = Telemetry::new(&dir);
    let agent = Arc::new(
        Agent::new(loaded.system_prompt, tools, &model)
            .with_params(params)
            .with_gates(gates)
            .with_fallbacks(&loaded.manifest.model.fallback)
            .with_max_turns(loaded.manifest.max_turns()),
    );
    Ok((agent, session, telemetry))
}

/// One-shot: run a single prompt to completion as a `Stream<Event>`.
pub fn query(opts: QueryOptions) -> anyhow::Result<QueryStream> {
    // Cloning, MCP handshakes etc. are blocking — keep them off async workers.
    let (agent, session, telemetry) =
        blocking(|| build_agent(opts.dir, opts.model, opts.repo, opts.permission_mode))?;
    let (inner, cancel) = agent.prompt_with_cancel(opts.prompt);
    Ok(QueryStream { inner, agent, cancel, session, usage: RunUsage::default(), telemetry })
}

/// A persistent multi-turn session: conversation context carries across `send`
/// calls (the underlying Agent keeps its transcript). Repo mode is finalized
/// once, when the Session is dropped.
pub struct Session {
    agent: Arc<Agent>,
    session: Option<LocalSession>,
    telemetry: Option<Telemetry>,
}

/// Open a multi-turn session (loads the agent once; no prompt yet).
pub fn open_session(
    dir: PathBuf,
    model: Option<String>,
    repo: Option<RepoOptions>,
    permission_mode: Option<String>,
) -> anyhow::Result<Session> {
    let (agent, session, telemetry) = blocking(|| build_agent(dir, model, repo, permission_mode))?;
    Ok(Session { agent, session, telemetry })
}

impl Session {
    /// Run one turn on the persistent transcript. Returns this turn's Events.
    pub fn send(&self, prompt: impl Into<String>) -> TurnStream {
        let (inner, cancel) = self.agent.prompt_with_cancel(prompt.into());
        TurnStream { inner, cancel, usage: RunUsage::default(), telemetry: self.telemetry.clone() }
    }
    /// Inject a message into the in-flight turn (steering).
    pub fn steer(&self, message: impl Into<String>) {
        self.agent.steer(message.into());
    }
    /// Cancel the in-flight turn.
    pub fn abort(&self) {
        self.agent.abort();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = blocking(|| s.finalize()); // commit/push, once
        }
    }
}

/// The Event stream for one turn of a [`Session`]. Dropping it aborts only that
/// turn — the Session (and its transcript) stays alive for the next `send`.
pub struct TurnStream {
    inner: ReceiverStream<AgentEvent>,
    /// This turn's own cancel token — dropping a finished turn's stream can't
    /// cancel a later one.
    cancel: CancellationToken,
    usage: RunUsage,
    telemetry: Option<Telemetry>,
}

impl TurnStream {
    pub fn usage(&self) -> RunUsage {
        self.usage
    }
}

impl Drop for TurnStream {
    fn drop(&mut self) {
        self.cancel.cancel(); // interrupting the stream cancels this turn only
    }
}

impl Stream for TurnStream {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(ev)) => {
                    if let Some(m) = account_and_map(ev, &mut this.usage, &this.telemetry) {
                        return Poll::Ready(Some(m));
                    }
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
