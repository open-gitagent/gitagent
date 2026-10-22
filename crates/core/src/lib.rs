//! The controller loop: model call → tool dispatch → record, until the agent
//! completes the task or a limit is hit.

mod approval;
mod fallback;
mod snapshot;

pub use approval::{Approval, ApprovalRequest, Approver, KillSwitch};

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gitagent_context::{
    compacted_message, estimate_tokens, mechanical_summary, message_tokens, Masker,
    COMPACTION_REQUEST,
};
use gitagent_guards::{LoopDetector, StopCheck, StopVerdict, TimeBudget, TimeEvent, LOOP_MESSAGE};
use gitagent_hooks::{AgentHooks, Decision, Hooks};
use gitagent_loader::{AgentSpec, HookEvent, Masking, ToolCalling};
use gitagent_provider::{
    CachePlan, ChatRequest, ChatResponse, Content, Message, Provider, ProviderError, ServerMasking,
    StopReason, Usage,
};
use gitagent_record::atif::{
    Agent, FinalMetrics, Metrics, Observation, ObservationResult, Step, ToolCall as AtifCall,
    TrajectoryWriter,
};
use gitagent_record::{Counters, Events};
use gitagent_shaper::{ShapeCfg, Shaper};
use gitagent_terminal::{ShellConfig, Terminal};
use gitagent_tools::{ToolContext, ToolOutput};
use serde_json::{json, Map, Value};

/// Variables never passed to the agent's shell.
const SECRET_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENROUTER_API_KEY",
    "OLLAMA_API_KEY",
    "VLLM_API_KEY",
    "GITAGENT_API_KEY",
    "GITAGENT_GIT_TOKEN",
    "GITHUB_TOKEN",
];

/// Stop calling the model when less than this much time is left.
const MIN_TIME_FOR_TURN: Duration = Duration::from_secs(5);
/// Consecutive responses without a tool call before giving up.
const MAX_NO_TOOL_TURNS: u32 = 3;
const PROVIDER_RETRIES: u32 = 5;

pub struct RunOptions {
    pub instruction: String,
    /// The shell starts here.
    pub work_dir: PathBuf,
    /// Trajectory, counters and command scripts go here.
    pub out_dir: PathBuf,
    pub timeout: Option<Duration>,
    pub session_id: Option<String>,
    /// `--events jsonl` sink; disabled unless asked for.
    pub events: Events,
    /// Continue an earlier run (interactive mode): its history and its open
    /// terminal. The new instruction is added as the next task.
    pub resume: Option<Resume>,
    /// Run the shell through the sandbox launcher (`gitagent __sandbox`),
    /// allowing writes beneath these directories only.
    pub sandbox: Option<Vec<PathBuf>>,
    /// Offer only these tools (plus `task_complete`); used for sub-agents.
    pub tool_allowlist: Option<Vec<String>>,
    /// Never offer these tools (`task_complete` stays); applied after the allowlist.
    pub tool_denylist: Vec<String>,
    /// Hooks given in code, asked after the file hooks.
    pub hook_callbacks: Option<std::sync::Arc<dyn gitagent_hooks::ToolHooks>>,
    /// Connected MCP servers, whose tools are offered after the others. The
    /// caller connects them once for a whole session.
    pub mcp: Option<std::sync::Arc<gitagent_mcp::Servers>>,
    /// Asked when a call or the final response needs a person's approval
    /// (the agent's `compliance` section); without one such requests are
    /// denied.
    pub approver: Option<std::sync::Arc<dyn Approver>>,
    /// Stops the run before its next step once tripped.
    pub kill: Option<KillSwitch>,
    /// Messages from the user to add while the run works.
    pub steer: Option<Steering>,
    /// Tools the caller runs itself.
    pub external_tools: Option<std::sync::Arc<dyn ExternalTools>>,
}

/// What a follow-up task continues from.
pub struct Resume {
    pub messages: Vec<Message>,
    /// The shell to keep using; `None` starts a new one (a conversation
    /// restored from storage).
    pub terminal: Option<Terminal>,
}

/// Messages from the user that arrive while a run is working; each is
/// added to the conversation before the next model call. Clones share the
/// queue.
#[derive(Clone, Default)]
pub struct Steering(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl Steering {
    pub fn push(&self, text: impl Into<String>) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text.into());
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Tools the caller runs itself (an IPC client's own tools). They are
/// offered after all others, and a call is handed to [`ExternalTools::call`]
/// after the hooks and approvals.
#[async_trait::async_trait]
pub trait ExternalTools: Send + Sync {
    fn specs(&self) -> Vec<gitagent_provider::ToolSpec>;
    /// The result's text and whether it is an error.
    async fn call(&self, name: &str, arguments: &Map<String, Value>) -> (String, bool);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    MaxTurns,
    TimeUp,
    NoToolCalls,
    /// The model's safety classifiers declined to continue.
    Refused,
    ProviderFailed(String),
    /// An agent hook (`on_session_start`, `pre_query`) stopped the run
    /// before its first turn.
    HookBlocked(String),
    /// The kill switch stopped the run.
    Killed(String),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Completed => f.write_str("completed"),
            Outcome::MaxTurns => f.write_str("max_turns"),
            Outcome::TimeUp => f.write_str("time_up"),
            Outcome::NoToolCalls => f.write_str("no_tool_calls"),
            Outcome::Refused => f.write_str("refused"),
            Outcome::ProviderFailed(e) => write!(f, "provider_failed: {e}"),
            Outcome::HookBlocked(r) => write!(f, "hook_blocked: {r}"),
            Outcome::Killed(r) => write!(f, "killed: {r}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub outcome: Outcome,
    pub turns: u32,
    pub usage: Usage,
    pub counters: Counters,
    pub trajectory_path: PathBuf,
    /// The `summary` the agent gave with its accepted `task_complete`.
    pub summary: Option<String>,
}

/// A finished run. The terminal is still open: drop it to stop everything the
/// agent started, or keep it alive (see [`Terminal::linger`]).
pub struct Finished {
    pub report: RunReport,
    pub terminal: Terminal,
    /// The conversation, for a follow-up task.
    pub messages: Vec<Message>,
}

impl Finished {
    /// Hands the history and the open terminal to a follow-up task.
    pub fn into_resume(self) -> Resume {
        Resume {
            messages: self.messages,
            terminal: Some(self.terminal),
        }
    }
}

pub async fn run(
    spec: &AgentSpec,
    provider: &dyn Provider,
    mut opts: RunOptions,
) -> Result<Finished> {
    // The shell runs in the work directory and reads its command scripts
    // from the output directory, so neither may be relative.
    opts.work_dir = std::path::absolute(&opts.work_dir)
        .with_context(|| format!("resolving {}", opts.work_dir.display()))?;
    opts.out_dir = std::path::absolute(&opts.out_dir)
        .with_context(|| format!("resolving {}", opts.out_dir.display()))?;
    let started = Instant::now();
    let deadline = opts.timeout.map(|t| started + t);
    let events = opts.events.clone();
    let mut summary: Option<String> = None;
    let remaining = || deadline.map(|d| d.saturating_duration_since(Instant::now()));

    std::fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("creating {}", opts.out_dir.display()))?;
    let counters_path = opts.out_dir.join("counters.json");
    let mut counters = Counters::new();

    let mut tools = offered_tools(
        spec,
        opts.mcp.as_deref(),
        opts.external_tools.as_deref(),
        opts.tool_allowlist.as_deref(),
        &opts.tool_denylist,
    );
    let sdk_names = spec.profile.tool_naming == gitagent_loader::ToolNaming::Sdk;
    if sdk_names {
        tools = gitagent_tools::naming::sdk_specs(tools);
    }
    let mut traj = TrajectoryWriter::new(
        opts.out_dir.join("trajectory.json"),
        opts.session_id.clone(),
        Agent {
            name: "gitagent".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            model_name: Some(spec.model.to_string()),
            tool_definitions: Some(tools.iter().map(|t| json!(t)).collect()),
            extra: Some(
                json!({"profile": spec.profile_source})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
        },
    );
    traj.push(Step::system(spec.system_prompt.clone()));
    let session_id = opts
        .session_id
        .clone()
        .unwrap_or_else(|| format!("run-{}", gitagent_record::utc_now_rfc3339()));
    let hooks = discover_hooks(spec, &opts.work_dir, &session_id);
    // Session-start and prompt hooks run before anything is sent; a block
    // ends the run before its first turn.
    let mut instruction = opts.instruction.clone();
    let mut blocked: Option<String> = None;
    if let Some(h) = hooks.as_ref() {
        // A new session: not a follow-up in a running one.
        if opts.resume.as_ref().is_none_or(|r| r.terminal.is_none()) {
            if let Decision::Block { reason } =
                h.event(HookEvent::SessionStart, None, Map::new()).await
            {
                blocked = Some(reason);
            }
        }
        if blocked.is_none() {
            match h
                .event(
                    HookEvent::PreQuery,
                    None,
                    object(json!({"prompt": instruction})),
                )
                .await
            {
                Decision::Allow => {}
                Decision::Block { reason } => blocked = Some(reason),
                Decision::Modify(d) => {
                    if let Some(p) = d.get("prompt").and_then(Value::as_str) {
                        counters.inc("hook_modifies");
                        instruction = p.to_string();
                    }
                }
            }
        }
        if let Some(reason) = &blocked {
            counters.inc("hook_blocks");
            events.emit(
                "guard",
                json!({"turn": 0, "kind": "hook_block", "reason": reason}),
            );
        }
    }
    let mut gate = approval::Gate::new(
        spec,
        opts.approver.as_ref(),
        opts.mcp.as_deref(),
        opts.kill.as_ref(),
    );
    let snapshot = if spec.profile.snapshot {
        let snap = snapshot::probe(&opts.work_dir, SECRET_ENV).await;
        counters.inc(if snap.is_some() {
            "snapshot_ok"
        } else {
            "snapshot_failed"
        });
        snap
    } else {
        None
    };
    let first_user = first_user_message(&instruction, snapshot.as_deref());
    traj.push(Step::user(first_user.clone()));
    traj.flush()?;

    let mut shell_cfg = ShellConfig::new(&opts.work_dir, opts.out_dir.join("commands"));
    shell_cfg.remove_env = SECRET_ENV.iter().map(|s| s.to_string()).collect();
    // The key of an OpenAI-compatible provider is named after it.
    let provider_key = format!(
        "{}_API_KEY",
        spec.model.provider.to_uppercase().replace('-', "_")
    );
    shell_cfg.remove_env.push(provider_key);
    if let Some(writable) = &opts.sandbox {
        let exe = std::env::current_exe().context("locating the gitagent binary")?;
        let mut launcher = vec![exe.to_string_lossy().into_owned(), "__sandbox".into()];
        for dir in writable {
            launcher.push("--write".into());
            launcher.push(dir.to_string_lossy().into_owned());
        }
        shell_cfg.launcher = launcher;
    }
    // Keep enough in memory that the spill file holds the full output of all
    // but extreme commands.
    shell_cfg.max_buffer_bytes = 8 << 20;
    let (mut terminal, mut messages) = match opts.resume {
        Some(Resume {
            messages,
            terminal: Some(t),
        }) => (t, messages),
        Some(Resume {
            messages,
            terminal: None,
        }) => (Terminal::start(shell_cfg).await?, messages),
        None => (Terminal::start(shell_cfg).await?, Vec::new()),
    };
    let shape = &spec.profile.shape;
    let mut shaper = Shaper::new(
        ShapeCfg {
            cap_tokens: shape.cap_tokens,
            head_ratio: shape.head_ratio,
            collapse_min_run: shape.collapse_min_run,
        },
        opts.out_dir.join("out"),
    );

    let stop_cfg = &spec.profile.stop;
    let mut stop_check = if stop_cfg.enabled {
        Some(StopCheck::new(
            &stop_cfg.edit_patterns,
            &stop_cfg.verify_patterns,
            stop_cfg.max_rejections,
        )?)
    } else {
        None
    };
    let mut call_seq: u64 = 0;
    let mut subagent_runs: u32 = 0;

    let guards = &spec.profile.guards;
    let mut time_budget = match (guards.time_budget, opts.timeout) {
        (true, Some(total)) => Some(TimeBudget::new(
            started,
            total,
            guards.nudge_at,
            guards.force_stop_at,
        )),
        _ => None,
    };
    let mut loop_detector = guards
        .loop_detector
        .then(|| LoopDetector::new(guards.loop_repeats));
    let mut bump_effort = false;

    let ctx_cfg = &spec.profile.context;
    let mut masker = (ctx_cfg.masking == Masking::Client)
        .then(|| Masker::new(ctx_cfg.mask_batch_tokens.into(), ctx_cfg.keep_last as usize));
    let server_masking = (ctx_cfg.masking == Masking::Server).then_some(ServerMasking {
        trigger_tokens: ctx_cfg.mask_batch_tokens,
        keep: ctx_cfg.keep_last,
        clear_at_least: ctx_cfg.mask_batch_tokens / 2,
    });
    let compact_above =
        u64::from(spec.profile.context_window).saturating_sub(ctx_cfg.compact_at_tokens.into());

    let mut cache_plan = CachePlan::from_breakpoints(&spec.profile.cache.breakpoints);
    cache_plan.one_hour = spec.profile.cache.ttl == "1h";
    let base_tokens = estimate_tokens(
        spec.system_prompt.len() + serde_json::to_string(&tools).map_or(0, |s| s.len()),
    );
    if messages.is_empty() {
        messages.push(Message::user_text(first_user.clone()));
    } else {
        // A follow-up task: added to the last user turn if there is one, so
        // roles keep alternating.
        let next = format!("# Next task\n\n{}\n", instruction.trim());
        match messages.last_mut() {
            Some(last) if last.role == gitagent_provider::Role::User => {
                last.content.push(Content::Text { text: next })
            }
            _ => messages.push(Message::user_text(next)),
        }
    }
    // Current prompt size: exact after each response, estimated in between.
    let mut prompt_tokens = base_tokens + message_tokens(&messages);
    let mut overflow_compactions = 0;
    let mut total = Usage::default();
    let mut turns = 0;
    let mut no_tool_turns = 0;
    let outcome = loop {
        if let Some(reason) = blocked.take() {
            break Outcome::HookBlocked(reason);
        }
        if let Some(reason) = gate.halted() {
            break Outcome::Killed(reason);
        }
        let steered = opts.steer.as_ref().map(Steering::take).unwrap_or_default();
        if !steered.is_empty() {
            counters.add("steer_messages", steered.len() as u64);
            let text = format!(
                "# Message from the user while you were working\n\n{}\n",
                steered.join("\n\n")
            );
            events.emit("steer", json!({"turn": turns, "text": text}));
            traj.push(Step::user(text.clone()));
            prompt_tokens += estimate_tokens(text.len());
            match messages.last_mut() {
                Some(last) if last.role == gitagent_provider::Role::User => {
                    last.content.push(Content::Text { text })
                }
                _ => messages.push(Message::user_text(text)),
            }
        }
        if turns >= spec.config.max_turns {
            break Outcome::MaxTurns;
        }
        if remaining().is_some_and(|r| r < MIN_TIME_FOR_TURN) {
            break Outcome::TimeUp;
        }
        if ctx_cfg.compaction && prompt_tokens > compact_above && messages.len() > 1 {
            let c = Compaction {
                provider,
                spec,
                tools: &tools,
                first_user: &first_user,
                cache: cache_plan,
                deadline,
                events: &events,
            };
            prompt_tokens = base_tokens
                + c.run(&mut messages, &mut counters, &mut traj, &mut total)
                    .await;
        }
        turns += 1;
        counters.inc("turns");
        events.emit("turn_start", json!({"turn": turns}));

        let effort = if bump_effort {
            bump_effort = false;
            counters.inc("effort_bumps");
            spec.profile.effort.next()
        } else {
            spec.profile.effort
        };
        let req = ChatRequest {
            model: spec.model.model.clone(),
            system: spec.system_prompt.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            effort,
            temperature: spec.profile.temperature,
            max_output_tokens: spec.profile.max_output_tokens,
            cache: cache_plan,
            server_masking,
        };
        let mut resp = match complete_with_retry(provider, &req, deadline).await {
            Ok(Some(r)) => r,
            Ok(None) => break Outcome::TimeUp,
            Err(ProviderError::ContextOverflow(_))
                if ctx_cfg.compaction && overflow_compactions < 2 && messages.len() > 1 =>
            {
                // Shrink the history and try the turn again.
                overflow_compactions += 1;
                counters.inc("overflow_compactions");
                let c = Compaction {
                    provider,
                    spec,
                    tools: &tools,
                    first_user: &first_user,
                    cache: cache_plan,
                    deadline,
                    events: &events,
                };
                prompt_tokens = base_tokens
                    + c.run(&mut messages, &mut counters, &mut traj, &mut total)
                        .await;
                continue;
            }
            Err(e) => break Outcome::ProviderFailed(e.to_string()),
        };
        prompt_tokens = resp.usage.input_tokens.max(prompt_tokens);
        counters.add("masks_applied", resp.cleared_tool_results);
        if resp.tool_calls.is_empty() && spec.profile.tool_calling != ToolCalling::Native {
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            let calls = fallback::parse_text_calls(&resp.text, &names, turns);
            if !calls.is_empty() {
                counters.add("repairs", calls.len() as u64);
                resp.tool_calls = calls;
                // The history is rebuilt from text plus these calls, so the
                // provider's raw blocks (which lack them) are not replayed.
                resp.raw = None;
            }
        }
        let cut_off = resp.stop_reason == StopReason::MaxTokens;
        if cut_off {
            counters.inc("truncated_turns");
        }
        add_usage(&mut total, &resp.usage, &mut counters);
        let served_by = resp.model.clone().unwrap_or_else(|| spec.model.to_string());
        if let Some(h) = hooks.as_ref() {
            h.set_turn(turns, resp.model.as_deref().unwrap_or(&spec.model.model));
        }
        events.emit(
            "model_text",
            json!({
                "turn": turns,
                "model": served_by,
                "text": resp.text,
                "reasoning": resp.reasoning,
                "usage": resp.usage,
                "stop_reason": resp.stop_reason,
                "tool_calls": resp.tool_calls.len(),
            }),
        );
        if resp.model.as_deref().is_some_and(|m| m != spec.model.model) {
            counters.inc("fallback_turns");
        }
        let mut assistant = resp.to_message();
        if assistant.content.is_empty() {
            // Providers reject empty assistant turns in the history.
            assistant.content.push(Content::Text {
                text: "(empty response)".into(),
            });
        }
        prompt_tokens += message_tokens(std::slice::from_ref(&assistant));
        messages.push(assistant);

        let mut step = Step::agent(resp.text.clone());
        step.model_name = Some(served_by);
        step.reasoning_content = resp.reasoning.clone();
        step.metrics = Some(Metrics {
            prompt_tokens: Some(resp.usage.input_tokens),
            completion_tokens: Some(resp.usage.output_tokens),
            cached_tokens: Some(resp.usage.cache_read_tokens),
            ..Default::default()
        });

        if resp.stop_reason == StopReason::Refusal {
            counters.inc("refusals");
            traj.push(step);
            flush(&traj, &counters, &counters_path);
            break Outcome::Refused;
        }

        if resp.tool_calls.is_empty()
            && spec.conversation
            && !cut_off
            && !resp.text.trim().is_empty()
        {
            // Conversation mode: a plain reply ends the turn.
            counters.inc("conversation_replies");
            traj.push(step);
            let check = FinalCheck {
                hooks: hooks.as_ref(),
                gate: &mut gate,
                spec,
                events: &events,
                counters: &mut counters,
                turn: turns,
            };
            match check.run(resp.text.trim().to_string()).await {
                Ok(r) => {
                    summary = Some(r);
                    flush(&traj, &counters, &counters_path);
                    events.emit(
                        "turn_end",
                        json!({"turn": turns, "usage": resp.usage, "prompt_tokens": prompt_tokens}),
                    );
                    break Outcome::Completed;
                }
                Err(message) => {
                    messages.push(Message::user_text(message.clone()));
                    traj.push(Step::user(message));
                    flush(&traj, &counters, &counters_path);
                    continue;
                }
            }
        }
        if resp.tool_calls.is_empty() {
            no_tool_turns += 1;
            counters.inc("no_tool_turns");
            traj.push(step);
            let nudge = if resp.stop_reason == StopReason::MaxTokens {
                "Your response was cut off because it was too long. Keep messages short and act through tool calls."
            } else {
                "Every response must call a tool. Continue the task, or call task_complete once you have verified it."
            };
            messages.push(Message::user_text(nudge));
            traj.push(Step::user(nudge));
            flush(&traj, &counters, &counters_path);
            if no_tool_turns >= MAX_NO_TOOL_TURNS {
                break Outcome::NoToolCalls;
            }
            continue;
        }
        no_tool_turns = 0;

        let mut results = Vec::new();
        let mut observations = Vec::new();
        let mut completed = false;
        // Messages for the agent, appended after the tool results.
        let mut notes: Vec<String> = Vec::new();
        // Sub-agent calls in one reply run at the same time, a few at once.
        // Their hooks are asked first, as for any call; the loop below picks
        // up each result by call id.
        let mut subagent_results: std::collections::HashMap<String, SubagentRun> =
            std::collections::HashMap::new();
        if !spec.agents.is_empty() && !completed {
            let mut pending: Vec<(String, gitagent_provider::ToolCall)> = Vec::new();
            for call in &resp.tool_calls {
                let mut run_call = call.clone();
                if sdk_names {
                    let (n, a) =
                        gitagent_loader::sdk::gitagent_tool_for_call(&call.name, &call.arguments);
                    run_call.name = n;
                    run_call.arguments = a;
                }
                if run_call.name != gitagent_tools::SUBAGENT {
                    continue;
                }
                match pre_hooks(
                    hooks.as_ref(),
                    opts.hook_callbacks.as_ref(),
                    &run_call.name,
                    &run_call.arguments,
                )
                .await
                {
                    PreHooks::Blocked(reason) => {
                        counters.inc("hook_blocks");
                        events.emit(
                            "guard",
                            json!({"turn": turns, "kind": "hook_block", "reason": reason}),
                        );
                        subagent_results.insert(
                            call.id.clone(),
                            SubagentRun::failed(error_output(format!(
                                "Blocked by a hook: {reason}"
                            ))),
                        );
                    }
                    PreHooks::Run {
                        arguments,
                        modified,
                    } => {
                        if modified {
                            counters.inc("hook_modifies");
                        }
                        run_call.arguments = arguments;
                        pending.push((call.id.clone(), run_call));
                    }
                }
            }
            if pending.len() > 1 {
                counters.inc("subagent_parallel_turns");
            }
            let at_once = spec.profile.tools.max_concurrent_subagents.max(1) as usize;
            for batch in pending.chunks(at_once) {
                let mut futures = Vec::new();
                for (_, run_call) in batch {
                    subagent_runs += 1;
                    events.emit(
                        "guard",
                        json!({"turn": turns, "kind": "subagent_start", "n": subagent_runs}),
                    );
                    futures.push(run_subagent(
                        spec,
                        provider,
                        run_call,
                        ChildRun {
                            cwd: terminal.cwd().to_path_buf(),
                            out_dir: opts.out_dir.join(format!("subagents/{subagent_runs}")),
                            remaining: remaining(),
                            sandbox: opts.sandbox.clone(),
                            hooks: opts.hook_callbacks.clone(),
                            mcp: opts.mcp.clone(),
                            approver: opts.approver.clone(),
                            kill: opts.kill.clone(),
                        },
                    ));
                }
                for ((id, _), result) in batch.iter().zip(join_all(futures).await) {
                    subagent_results.insert(id.clone(), result);
                }
            }
        }
        // Set when the kill switch is found tripped; the remaining calls are
        // answered without running.
        let mut halted: Option<String> = None;
        for call in &resp.tool_calls {
            // Under `.claude/` names the model called `Bash`; the loop runs `exec`.
            let mut run_call = call.clone();
            if sdk_names {
                let (name, args) =
                    gitagent_loader::sdk::gitagent_tool_for_call(&call.name, &call.arguments);
                if name != call.name {
                    counters.inc("sdk_named_calls");
                }
                run_call.name = name;
                run_call.arguments = args;
            }
            counters.inc_labelled("tool_calls", &run_call.name);
            events.emit(
                "tool_call",
                json!({"turn": turns, "id": call.id, "name": call.name, "arguments": call.arguments}),
            );
            let mut subagent_ref: Option<String> = None;
            if halted.is_none() {
                halted = gate.halted();
            }
            let mut output = if let Some(r) = &halted {
                error_output(format!("Not run: the agent was halted: {r}."))
            } else if completed {
                error_output("Not run: the task was already marked complete.".into())
            } else if let Some(r) = subagent_results.remove(&call.id) {
                counters.inc("subagent_calls");
                if r.partial {
                    counters.inc("subagent_partial");
                }
                if r.model_override {
                    counters.inc("subagent_model_overrides");
                }
                if r.effort_override {
                    counters.inc("subagent_effort_overrides");
                }
                if let Some(rep) = r.report {
                    counters.add("subagent_turns", rep.turns.into());
                    add_usage(&mut total, &rep.usage, &mut counters);
                    subagent_ref = Some(rep.trajectory_path.display().to_string());
                }
                r.out
            } else {
                match pre_hooks(
                    hooks.as_ref(),
                    opts.hook_callbacks.as_ref(),
                    &run_call.name,
                    &run_call.arguments,
                )
                .await
                {
                    PreHooks::Blocked(reason) => {
                        counters.inc("hook_blocks");
                        events.emit(
                            "guard",
                            json!({"turn": turns, "kind": "hook_block", "reason": reason}),
                        );
                        error_output(format!("Blocked by a hook: {reason}"))
                    }
                    PreHooks::Run {
                        arguments,
                        modified,
                    } => {
                        if modified {
                            counters.inc("hook_modifies");
                        }
                        run_call.arguments = arguments;
                        let mut denied = None;
                        if let Some(reason) = gate.reason(&run_call, total.output_tokens) {
                            let request = ApprovalRequest {
                                tool_name: run_call.name.clone(),
                                arguments: run_call.arguments.clone(),
                                reason,
                            };
                            events.emit(
                                "approval_request",
                                json!({"turn": turns, "id": call.id, "name": run_call.name, "reason": request.reason}),
                            );
                            let answer = gate.ask(&request).await;
                            events.emit(
                                "approval",
                                json!({"turn": turns, "id": call.id, "approved": answer.approved, "reason": answer.reason}),
                            );
                            if answer.approved {
                                counters.inc("approvals_granted");
                            } else {
                                counters.inc("approvals_denied");
                                denied = Some(approval::denial_message(&request, &answer));
                            }
                        } else if gate.advisory(&run_call) {
                            counters.inc("advisory_notices");
                            events.emit(
                                "guard",
                                json!({"turn": turns, "kind": "advisory", "name": run_call.name}),
                            );
                        }
                        let external = opts
                            .external_tools
                            .as_ref()
                            .filter(|e| e.specs().iter().any(|t| t.name == run_call.name));
                        let mut out = if let Some(message) = denied {
                            error_output(message)
                        } else if let Some(ext) = external {
                            counters.inc_labelled("external_tool_calls", &run_call.name);
                            let (content, is_error) =
                                ext.call(&run_call.name, &run_call.arguments).await;
                            ToolOutput {
                                is_error,
                                ..error_output(content)
                            }
                        } else {
                            let mut ctx = ToolContext {
                                terminal: &mut terminal,
                                shaper: &mut shaper,
                                counters: &mut counters,
                                tools: &spec.profile.tools,
                                edit_tool: spec.profile.edit_tool,
                                skills: &spec.skills,
                                memory: spec.memory.as_ref(),
                                declared: &spec.declared_tools,
                                mcp: opts.mcp.as_deref(),
                                default_yield_ms: spec.profile.exec_default_yield_ms,
                                remaining: remaining(),
                                minutes_left: time_budget
                                    .as_ref()
                                    .map(|t| t.minutes_left(Instant::now())),
                                sdk_names,
                            };
                            gitagent_tools::dispatch(&run_call, &mut ctx).await
                        };
                        let mut post_decisions = Vec::new();
                        if let Some(h) = hooks.as_ref() {
                            post_decisions.push(
                                h.post(
                                    &run_call.name,
                                    &run_call.arguments,
                                    &out.content,
                                    out.is_error,
                                )
                                .await,
                            );
                        }
                        if let Some(h) = opts.hook_callbacks.as_ref() {
                            let content = match post_decisions.last() {
                                Some(Decision::Modify(Value::String(t))) => t.clone(),
                                _ => out.content.clone(),
                            };
                            post_decisions.push(
                                h.post(&run_call.name, &run_call.arguments, &content, out.is_error)
                                    .await,
                            );
                        }
                        for decision in post_decisions {
                            match decision {
                                Decision::Allow => {}
                                Decision::Modify(Value::String(text)) => {
                                    counters.inc("hook_modifies");
                                    out.content = text;
                                    out.images.clear();
                                }
                                Decision::Modify(_) | Decision::Block { .. } => {
                                    counters.inc("hook_blocks");
                                    out.content = "Output withheld by a hook.".into();
                                    out.is_error = true;
                                    out.images.clear();
                                    break;
                                }
                            }
                        }
                        if let Some(h) = hooks.as_ref() {
                            notify_tool_hooks(h, &run_call, &out).await;
                        }
                        out
                    }
                }
            };
            if cut_off && output.is_error {
                output.content = format!(
                    "Your reply hit the output limit, so this call was cut off. Send a shorter call (for example, write a large file in parts). {}",
                    output.content
                );
            }
            call_seq += 1;
            if let Some(detector) = loop_detector.as_mut() {
                let args = Value::Object(run_call.arguments.clone()).to_string();
                if detector.observe(&run_call.name, &args, &output.content) {
                    counters.inc("loop_triggers");
                    events.emit("guard", json!({"turn": turns, "kind": "loop"}));
                    notes.push(LOOP_MESSAGE.to_string());
                    bump_effort |= guards.loop_effort_bump;
                }
            }
            if let (Some(m), Some(stub)) = (masker.as_mut(), output.stub.take()) {
                m.record(&call.id, stub);
            }
            let out_of_time = time_budget.as_ref().is_some_and(TimeBudget::forced);
            if let Some(check) = stop_check.as_mut().filter(|_| !out_of_time) {
                let edits = [
                    gitagent_tools::WRITE_FILE,
                    gitagent_tools::STR_REPLACE,
                    gitagent_tools::APPLY_PATCH,
                ];
                if edits.contains(&run_call.name.as_str()) && !output.is_error {
                    let path = run_call
                        .arguments
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("files");
                    check.record_edit(call_seq, &format!("{} {path}", run_call.name));
                }
                if run_call.name == gitagent_tools::EXEC {
                    if let Some(cmd) = run_call.arguments.get("cmd").and_then(Value::as_str) {
                        check.observe_command(call_seq, cmd, output.finished);
                    }
                }
                if output.complete {
                    match check.on_complete() {
                        StopVerdict::Accept => {
                            counters.inc("stop_accepts");
                            events.emit("stop_check", json!({"turn": turns, "verdict": "accept"}));
                        }
                        StopVerdict::Reject { last_edit } => {
                            counters.inc("stop_rejections");
                            events.emit(
                                "stop_check",
                                json!({"turn": turns, "verdict": "reject", "unverified": last_edit}),
                            );
                            output.complete = false;
                            output.content = rejection_message(&last_edit, &instruction);
                        }
                    }
                }
            }
            if output.complete {
                let response = run_call
                    .arguments
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let check = FinalCheck {
                    hooks: hooks.as_ref(),
                    gate: &mut gate,
                    spec,
                    events: &events,
                    counters: &mut counters,
                    turn: turns,
                };
                match check.run(response).await {
                    Ok(r) => summary = Some(r).filter(|r| !r.is_empty()),
                    Err(message) => {
                        output.complete = false;
                        output.content = message;
                        summary = None;
                    }
                }
            }
            gate.observe(output.is_error);
            completed |= output.complete;
            events.emit("tool_result", {
                let preview = truncate_chars(&output.content, 2000);
                let mut e = json!({
                    "turn": turns,
                    "id": call.id,
                    "name": call.name,
                    "is_error": output.is_error,
                    "content": preview,
                });
                // The whole result, when the preview is cut.
                if preview.len() != output.content.len() {
                    e["output"] = json!(output.content);
                }
                e
            });
            observations.push(ObservationResult {
                source_call_id: Some(call.id.clone()),
                content: Some(output.content.clone()),
                subagent_trajectory_ref: subagent_ref.map(|path| {
                    vec![gitagent_record::atif::SubagentTrajectoryRef {
                        trajectory_id: None,
                        session_id: None,
                        trajectory_path: Some(path),
                    }]
                }),
                extra: None,
            });
            results.push(Content::ToolResult {
                tool_use_id: call.id.clone(),
                content: output.content,
                is_error: output.is_error,
                images: output.images,
            });
        }
        step.tool_calls = Some(
            resp.tool_calls
                .iter()
                .map(|c| AtifCall {
                    tool_call_id: c.id.clone(),
                    function_name: c.name.clone(),
                    arguments: c.arguments.clone(),
                })
                .collect(),
        );
        step.observation = Some(Observation {
            results: observations,
        });
        traj.push(step);
        if !completed {
            if let Some(tb) = time_budget.as_mut() {
                let now = Instant::now();
                if let Some(event) = tb.check(now) {
                    let kind = match event {
                        TimeEvent::Nudge => "nudges",
                        TimeEvent::ForceStop => "forced_stops",
                    };
                    counters.inc(kind);
                    events.emit("guard", json!({"turn": turns, "kind": kind}));
                    notes.push(tb.message(event, now));
                }
            }
        }
        if !notes.is_empty() {
            let text = notes.join("\n\n");
            traj.push(Step::user(text.clone()));
            results.push(Content::Text { text });
        }
        let results = Message::tool_results(results);
        prompt_tokens += message_tokens(std::slice::from_ref(&results));
        messages.push(results);
        if let Some(m) = masker.as_mut() {
            let report = m.maybe_mask(&mut messages, prompt_tokens);
            if report.masked > 0 {
                counters.add("masks_applied", report.masked as u64);
                counters.inc("mask_checkpoints");
                events.emit(
                    "guard",
                    json!({"turn": turns, "kind": "mask", "masked": report.masked}),
                );
                prompt_tokens = prompt_tokens.saturating_sub(report.tokens_saved);
            }
        }
        flush(&traj, &counters, &counters_path);
        events.emit(
            "turn_end",
            json!({"turn": turns, "usage": resp.usage, "prompt_tokens": prompt_tokens}),
        );

        if let Some(reason) = halted {
            break Outcome::Killed(reason);
        }
        if completed {
            break Outcome::Completed;
        }
    };

    if let Some(h) = hooks.as_ref() {
        match &outcome {
            Outcome::Completed if h.has(HookEvent::PostResponse) => {
                let data = object(json!({"response": summary.clone().unwrap_or_default()}));
                h.event(HookEvent::PostResponse, None, data).await;
            }
            Outcome::ProviderFailed(e) if h.has(HookEvent::OnError) => {
                let data = object(json!({"error": {"message": e, "code": "provider_failed"}}));
                h.event(HookEvent::OnError, None, data).await;
            }
            _ => {}
        }
    }

    traj.set_final_metrics(FinalMetrics {
        total_prompt_tokens: Some(total.input_tokens),
        total_completion_tokens: Some(total.output_tokens),
        total_cached_tokens: Some(total.cache_read_tokens),
        total_steps: Some(traj.trajectory().steps.len() as u32),
        ..Default::default()
    });
    traj.set_extra("outcome", Value::String(outcome.to_string()));
    traj.set_extra("counters", counters.to_json());
    traj.set_extra(
        "elapsed_s",
        json!((started.elapsed().as_secs_f64() * 10.0).round() / 10.0),
    );
    traj.flush()?;
    counters.write(&counters_path)?;
    events.emit(
        "run_end",
        json!({
            "outcome": outcome.to_string(),
            "turns": turns,
            "usage": total,
            "counters": counters.to_json(),
            "summary": summary,
            "trajectory": traj.path(),
        }),
    );

    let report = RunReport {
        outcome,
        turns,
        usage: total,
        counters,
        trajectory_path: traj.path().to_path_buf(),
        summary,
    };
    Ok(Finished {
        report,
        terminal,
        messages,
    })
}

/// The task as the first user message. Everything task-specific lives here,
/// never in the system prompt.
fn first_user_message(instruction: &str, snapshot: Option<&str>) -> String {
    let mut msg = format!("# Task\n\n{}\n", instruction.trim());
    if let Some(snap) = snapshot {
        msg.push_str("\n# Environment snapshot\n\nCollected before your first turn.\n\n```\n");
        msg.push_str(snap);
        msg.push_str("\n```\n");
    }
    msg
}

/// Sent instead of accepting `task_complete` when nothing was verified after
/// the last change. Re-shows the task so the agent can check it against it.
/// Runs a sub-agent: a nested loop with a fresh history, its own shell in
/// `cwd`, half the remaining time, no sub-agents of its own, and the
/// definition's tools and turn limit. Returns what the parent sees.
#[allow(clippy::too_many_arguments)]
/// The tools a run offers, in order, under gitagent's names: the profile's
/// tools, then `skill`, `subagent`, `memory`, the agent's declared tools, the
/// MCP servers' tools and the caller's own tools when there are any, after
/// the allow and deny lists (`task_complete` stays, except in conversation
/// mode).
pub fn offered_tools(
    spec: &AgentSpec,
    mcp: Option<&gitagent_mcp::Servers>,
    external: Option<&dyn ExternalTools>,
    allow: Option<&[String]>,
    deny: &[String],
) -> Vec<gitagent_provider::ToolSpec> {
    let mut tools = gitagent_tools::specs(&spec.profile.tools, spec.profile.edit_tool);
    if !spec.skills.is_empty() {
        tools.push(gitagent_tools::skill_spec(&spec.skills));
    }
    if !spec.agents.is_empty() {
        tools.push(gitagent_tools::subagent_spec(&spec.agents));
    }
    if let Some(memory) = &spec.memory {
        tools.push(gitagent_tools::memory_spec(memory));
    }
    tools.extend(
        spec.declared_tools
            .iter()
            .map(gitagent_tools::declared_spec),
    );
    tools.extend(mcp.into_iter().flat_map(|m| m.specs()));
    tools.extend(external.into_iter().flat_map(|e| e.specs()));
    if let Some(allow) = allow {
        tools.retain(|t| allow.contains(&t.name) || t.name == gitagent_tools::TASK_COMPLETE);
    }
    tools.retain(|t| !deny.contains(&t.name) || t.name == gitagent_tools::TASK_COMPLETE);
    if spec.conversation {
        // A plain reply ends the turn.
        tools.retain(|t| t.name != gitagent_tools::TASK_COMPLETE);
    }
    tools
}

/// Connects the spec's MCP servers for a session; `None` when it has none.
/// Servers that cannot be used come back as warnings.
pub async fn connect_mcp(
    spec: &AgentSpec,
) -> (Option<std::sync::Arc<gitagent_mcp::Servers>>, Vec<String>) {
    if spec.mcp_servers.is_empty() {
        return (None, Vec::new());
    }
    let taken: Vec<String> = offered_tools(spec, None, None, None, &[])
        .into_iter()
        .map(|t| t.name)
        .collect();
    let (servers, warnings) = gitagent_mcp::Servers::connect(&spec.mcp_servers, &taken).await;
    (Some(std::sync::Arc::new(servers)), warnings)
}

/// Runs the agent's `on_session_end` hooks. Call it once when a session
/// (one run, or a conversation of several) is over; their answers do not
/// matter.
pub async fn end_session(spec: &AgentSpec, work_dir: &Path, session_id: &str) {
    if let Some(h) = discover_hooks(spec, work_dir, session_id) {
        if h.has(HookEvent::SessionEnd) {
            h.event(HookEvent::SessionEnd, None, Map::new()).await;
        }
    }
}

/// The hook scripts and settings-file hooks of the config directories (when
/// hooks are on) and the agent's `hooks/hooks.yaml` (only its compliance
/// hooks when hooks are off).
fn discover_hooks(spec: &AgentSpec, work_dir: &Path, session_id: &str) -> Option<Hooks> {
    let agent = spec.agent.as_ref().and_then(|a| {
        AgentHooks::new(
            &spec.agent_hooks,
            &a.root,
            session_id,
            &a.manifest.name,
            &spec.model.model,
            spec.config.hooks,
        )
    });
    let dirs: &[PathBuf] = if spec.config.hooks {
        &spec.config_dirs
    } else {
        &[]
    };
    Hooks::discover(dirs, work_dir, agent)
}

/// The notification hooks after a tool call: `post_tool_failure` when it
/// failed, `file_changed` for each file a file tool changed.
async fn notify_tool_hooks(h: &Hooks, call: &gitagent_provider::ToolCall, out: &ToolOutput) {
    let data = || object(json!({"tool_name": call.name, "arguments": call.arguments}));
    if out.is_error {
        if h.has(HookEvent::PostToolFailure) {
            let mut d = data();
            d.insert("error".into(), json!(out.content));
            h.event(HookEvent::PostToolFailure, Some(&call.name), d)
                .await;
        }
        return;
    }
    if !h.has(HookEvent::FileChanged) {
        return;
    }
    let arg = |k: &str| call.arguments.get(k).and_then(Value::as_str);
    let paths: Vec<String> = match call.name.as_str() {
        gitagent_tools::WRITE_FILE | gitagent_tools::STR_REPLACE => {
            arg("path").map(str::to_string).into_iter().collect()
        }
        gitagent_tools::APPLY_PATCH => arg("patch")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                [
                    "*** Add File: ",
                    "*** Update File: ",
                    "*** Delete File: ",
                    "*** Move to: ",
                ]
                .iter()
                .find_map(|p| l.strip_prefix(p))
            })
            .map(|p| p.trim().to_string())
            .collect(),
        _ => Vec::new(),
    };
    for path in paths {
        let mut d = data();
        d.insert("file_path".into(), json!(path));
        h.event(HookEvent::FileChanged, Some(&call.name), d).await;
    }
}

fn object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The checks a final response passes before it is delivered: the
/// `pre_response` hooks (which may block or rewrite it), then approval when
/// responses are reviewed.
struct FinalCheck<'a, 'g> {
    hooks: Option<&'a Hooks>,
    gate: &'a mut approval::Gate<'g>,
    spec: &'a AgentSpec,
    events: &'a Events,
    counters: &'a mut Counters,
    turn: u32,
}

impl FinalCheck<'_, '_> {
    /// The response to deliver, or what to tell the model instead.
    async fn run(self, mut response: String) -> Result<String, String> {
        let again = if self.spec.conversation {
            "send your reply again"
        } else {
            "call task_complete again"
        };
        if let Some(h) = self.hooks.filter(|h| h.has(HookEvent::PreResponse)) {
            let data = object(json!({ "response": response }));
            match h.event(HookEvent::PreResponse, None, data).await {
                Decision::Allow => {}
                Decision::Modify(d) => {
                    if let Some(r) = d.get("response").and_then(Value::as_str) {
                        self.counters.inc("hook_modifies");
                        response = r.to_string();
                    }
                }
                Decision::Block { reason } => {
                    self.counters.inc("hook_blocks");
                    self.events.emit(
                        "guard",
                        json!({"turn": self.turn, "kind": "hook_block", "reason": reason}),
                    );
                    return Err(format!(
                        "Your final response was blocked by a hook: {reason}\nAddress this, then {again}."
                    ));
                }
            }
        }
        if self.spec.supervision.review_responses {
            let request = ApprovalRequest {
                tool_name: gitagent_tools::TASK_COMPLETE.into(),
                arguments: object(json!({ "summary": response })),
                reason: "the final response needs review (communications.pre_review_required)"
                    .into(),
            };
            let answer = self.gate.ask(&request).await;
            self.events.emit(
                "approval",
                json!({"turn": self.turn, "approved": answer.approved, "reason": answer.reason}),
            );
            if answer.approved {
                self.counters.inc("approvals_granted");
            } else {
                self.counters.inc("approvals_denied");
                return Err(format!(
                    "Your final response was not approved: {}\nRevise it, then {again}.",
                    answer.reason.as_deref().unwrap_or("no reason given")
                ));
            }
        }
        Ok(response)
    }
}

/// What the hooks decided about a call before it runs.
enum PreHooks {
    Run {
        arguments: Map<String, Value>,
        modified: bool,
    },
    Blocked(String),
}

/// Asks the file hooks, then the programmatic ones; the first block wins and
/// each change is what the next hook sees.
async fn pre_hooks(
    file_hooks: Option<&Hooks>,
    callbacks: Option<&std::sync::Arc<dyn gitagent_hooks::ToolHooks>>,
    name: &str,
    arguments: &Map<String, Value>,
) -> PreHooks {
    let mut args = arguments.clone();
    let mut modified = false;
    let mut decisions = Vec::new();
    if let Some(h) = file_hooks {
        decisions.push(h.pre(name, &args).await);
    }
    if let Some(h) = callbacks {
        if let Some(Decision::Modify(Value::Object(a))) = decisions.last() {
            args = a.clone();
        }
        if !matches!(decisions.last(), Some(Decision::Block { .. })) {
            decisions.push(h.pre(name, &args).await);
        }
    }
    for decision in decisions {
        match decision {
            Decision::Allow => {}
            Decision::Block { reason } => return PreHooks::Blocked(reason),
            Decision::Modify(Value::Object(a)) => {
                args = a;
                modified = true;
            }
            Decision::Modify(_) => {
                return PreHooks::Blocked("hook returned invalid arguments".into())
            }
        }
    }
    PreHooks::Run {
        arguments: args,
        modified,
    }
}

/// Polls every future until all are done, keeping their order.
async fn join_all<'a, T>(
    mut futures: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>>,
) -> Vec<T> {
    let mut done: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for (slot, fut) in done.iter_mut().zip(futures.iter_mut()) {
            if slot.is_none() {
                match fut.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => *slot = Some(v),
                    std::task::Poll::Pending => all_done = false,
                }
            }
        }
        if all_done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    done.into_iter()
        .map(|d| d.expect("polled to completion"))
        .collect()
}

/// Where and within what limits a sub-agent runs.
struct ChildRun {
    cwd: PathBuf,
    out_dir: PathBuf,
    remaining: Option<Duration>,
    sandbox: Option<Vec<PathBuf>>,
    hooks: Option<std::sync::Arc<dyn gitagent_hooks::ToolHooks>>,
    mcp: Option<std::sync::Arc<gitagent_mcp::Servers>>,
    approver: Option<std::sync::Arc<dyn Approver>>,
    kill: Option<KillSwitch>,
}

/// What one sub-agent run came to.
struct SubagentRun {
    out: ToolOutput,
    report: Option<RunReport>,
    /// It ran out of turns; what it had is returned as a partial result.
    partial: bool,
    model_override: bool,
    effort_override: bool,
}

impl SubagentRun {
    fn failed(out: ToolOutput) -> Self {
        SubagentRun {
            out,
            report: None,
            partial: false,
            model_override: false,
            effort_override: false,
        }
    }
}

/// Boxed and `Send` by declaration: `run` awaits this and this awaits `run`,
/// and the compiler cannot prove `Send` through that cycle on its own.
fn run_subagent<'a>(
    spec: &'a AgentSpec,
    provider: &'a dyn Provider,
    call: &'a gitagent_provider::ToolCall,
    child_run: ChildRun,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = SubagentRun> + Send + 'a>> {
    Box::pin(async move {
        let ChildRun {
            cwd,
            out_dir,
            remaining,
            sandbox,
            hooks: opts_hooks,
            mcp,
            approver,
            kill,
        } = child_run;
        let arg = |k: &str| call.arguments.get(k).and_then(Value::as_str);
        let name = arg("agent").unwrap_or(gitagent_loader::GENERAL_AGENT);
        let Some(def) = spec.agents.iter().find(|a| a.name == name) else {
            let names: Vec<&str> = spec.agents.iter().map(|a| a.name.as_str()).collect();
            return SubagentRun::failed(error_output(format!(
                "No sub-agent named {name:?}. Agents: {}.",
                names.join(", ")
            )));
        };
        let Some(prompt) = arg("prompt").filter(|p| !p.trim().is_empty()) else {
            return SubagentRun::failed(error_output(
                "Invalid arguments for subagent: missing `prompt`.".into(),
            ));
        };
        let timeout = remaining.map(|r| r.saturating_sub(Duration::from_secs(30)) / 2);
        if timeout.is_some_and(|t| t < Duration::from_secs(60)) {
            return SubagentRun::failed(error_output(
                "Not enough time left to start a sub-agent; do the work yourself.".into(),
            ));
        }

        let mut child = spec.clone();
        // A sub-agent's calls go through the agent's tool hooks; the session
        // and response hooks belong to the session that started it.
        child.agent_hooks.retain(|h| h.event.is_tool_event());
        child.profile.snapshot = false;
        child.config.max_turns = def.max_turns;
        // Nesting: one level less for the child; at the last level it has no
        // sub-agents of its own.
        let depth = spec.profile.tools.subagent_depth;
        if depth > 1 {
            child.profile.tools.subagent_depth = depth - 1;
        } else {
            child.agents.clear();
        }
        let mut effort_override = false;
        if let Some(e) = &def.effort {
            match serde_json::from_value::<gitagent_loader::Effort>(Value::String(e.clone())) {
                Ok(level) => {
                    child.profile.effort = level;
                    effort_override = true;
                }
                Err(_) => {
                    return SubagentRun::failed(error_output(format!(
                        "Sub-agent `{name}` asks for an unknown effort {e:?}."
                    )))
                }
            }
        }
        let mut model_override = false;
        if let Some(m) = &def.model {
            let (provider_name, model_id) = match m.split_once(':') {
                Some((p, id)) => (p, id),
                None => (spec.model.provider.as_str(), m.as_str()),
            };
            if provider_name != spec.model.provider {
                return SubagentRun::failed(error_output(format!(
                    "Sub-agent `{name}` asks for model {m:?}, but a sub-agent must use the parent's provider ({}).",
                    spec.model.provider
                )));
            }
            child.model.model = gitagent_loader::sdk::model_alias(model_id).to_string();
            model_override = child.model.model != spec.model.model;
        }
        let own_provider: Option<Box<dyn Provider>> = if model_override {
            match gitagent_provider::for_spec(&child) {
                Ok(p) => Some(p),
                Err(e) => {
                    return SubagentRun::failed(error_output(format!(
                        "The sub-agent's model could not be set up: {e:#}"
                    )))
                }
            }
        } else {
            None
        };
        let provider: &dyn Provider = own_provider.as_deref().unwrap_or(provider);
        let finish = if spec.conversation {
            "When done, reply without calling a tool, with a summary of what you found or did, \
             including the paths, commands and results the other agent needs; that reply is all \
             it sees."
        } else {
            "When done, call task_complete with a summary of what you found or did, including the \
             paths, commands and results the other agent needs; that summary is all it sees."
        };
        child.system_prompt = format!(
        "{}\n# Sub-agent\n\nYou were started by another agent for one piece of work: the task below. \
         You cannot ask it questions. {finish}\n{}",
        spec.system_prompt,
        if def.instructions.is_empty() {
            String::new()
        } else {
            format!("\n{}\n", def.instructions)
        }
    );
        let opts = RunOptions {
            instruction: prompt.to_string(),
            work_dir: cwd,
            out_dir,
            timeout,
            session_id: None,
            events: Events::disabled(),
            resume: None,
            sandbox,
            tool_allowlist: def.tools.clone(),
            tool_denylist: def.disallowed_tools.clone(),
            hook_callbacks: opts_hooks,
            mcp,
            approver,
            kill,
            steer: None,
            external_tools: None,
        };
        let nested: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Finished>> + Send + '_>,
        > = Box::pin(run(&child, provider, opts));
        match nested.await {
            Ok(finished) => {
                let report = finished.report;
                let answer = report.summary.clone().unwrap_or_else(|| {
                    finished
                        .messages
                        .iter()
                        .rev()
                        .filter(|m| m.role == gitagent_provider::Role::Assistant)
                        .flat_map(|m| &m.content)
                        .find_map(|c| match c {
                            Content::Text { text } if !text.trim().is_empty() => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "(no summary)".into())
                });
                let partial = report.outcome == Outcome::MaxTurns;
                let status = if partial {
                    format!(
                        "ran out of turns after {} turns; its result is partial",
                        report.turns
                    )
                } else {
                    format!("{} after {} turns", report.outcome, report.turns)
                };
                let mut out = error_output(format!(
                    "Sub-agent `{name}` ({}) ended: {status}.\n\n{answer}",
                    arg("description").unwrap_or("no description"),
                ));
                out.is_error = report.outcome != Outcome::Completed && !partial;
                SubagentRun {
                    out,
                    report: Some(report),
                    partial,
                    model_override,
                    effort_override,
                }
            }
            Err(e) => {
                SubagentRun::failed(error_output(format!("The sub-agent could not run: {e:#}")))
            }
        }
    })
}

fn error_output(content: String) -> ToolOutput {
    ToolOutput {
        content,
        is_error: true,
        complete: false,
        images: Vec::new(),
        finished: true,
        stub: None,
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn rejection_message(last_edit: &str, instruction: &str) -> String {
    format!(
        "Not marked complete: nothing was verified after your last change (`{last_edit}`). \
         Check the result now (run the program, the tests or a request against it), then call \
         task_complete again.\n\nOriginal task:\n{}",
        instruction.trim()
    )
}

/// Writes the trajectory and counters; a failed write must not end the run.
fn flush(traj: &TrajectoryWriter, counters: &Counters, counters_path: &std::path::Path) {
    if let Err(e) = traj.flush() {
        eprintln!("gitagent: writing trajectory failed: {e:#}");
    }
    if let Err(e) = counters.write(counters_path) {
        eprintln!("gitagent: writing counters failed: {e:#}");
    }
}

fn add_usage(total: &mut Usage, u: &Usage, counters: &mut Counters) {
    total.input_tokens += u.input_tokens;
    total.output_tokens += u.output_tokens;
    total.cache_read_tokens += u.cache_read_tokens;
    total.cache_write_tokens += u.cache_write_tokens;
    counters.add("input_tokens", u.input_tokens);
    counters.add("output_tokens", u.output_tokens);
    counters.add("cache_read_tokens", u.cache_read_tokens);
    counters.add("cache_write_tokens", u.cache_write_tokens);
}

/// Replaces the history with the first message plus a summary written by the
/// model (or built from the recent calls if that fails).
struct Compaction<'a> {
    provider: &'a dyn Provider,
    spec: &'a AgentSpec,
    tools: &'a [gitagent_provider::ToolSpec],
    first_user: &'a str,
    cache: CachePlan,
    deadline: Option<Instant>,
    events: &'a Events,
}

impl Compaction<'_> {
    /// Returns the estimated size of the new history.
    async fn run(
        &self,
        messages: &mut Vec<Message>,
        counters: &mut Counters,
        traj: &mut TrajectoryWriter,
        total: &mut Usage,
    ) -> u64 {
        let mut ask = messages.clone();
        match ask.last_mut() {
            Some(last) if last.role == gitagent_provider::Role::User => {
                last.content.push(Content::Text {
                    text: COMPACTION_REQUEST.into(),
                })
            }
            _ => ask.push(Message::user_text(COMPACTION_REQUEST)),
        }
        let req = ChatRequest {
            model: self.spec.model.model.clone(),
            system: self.spec.system_prompt.clone(),
            messages: ask,
            tools: self.tools.to_vec(),
            effort: self.spec.profile.effort,
            temperature: self.spec.profile.temperature,
            max_output_tokens: self.spec.profile.max_output_tokens,
            cache: self.cache,
            server_masking: None,
        };
        let summary = match complete_with_retry(self.provider, &req, self.deadline).await {
            Ok(Some(r)) if !r.text.trim().is_empty() => {
                add_usage(total, &r.usage, counters);
                let mut step = Step::agent(r.text.clone());
                step.metrics = Some(Metrics {
                    prompt_tokens: Some(r.usage.input_tokens),
                    completion_tokens: Some(r.usage.output_tokens),
                    cached_tokens: Some(r.usage.cache_read_tokens),
                    ..Default::default()
                });
                step.extra = json!({"compaction": true}).as_object().cloned();
                traj.push(step);
                r.text
            }
            _ => {
                counters.inc("compaction_fallbacks");
                mechanical_summary(messages, 12)
            }
        };
        counters.inc("compactions");
        self.events.emit("guard", json!({"kind": "compaction"}));
        let first = compacted_message(self.first_user, &summary);
        traj.push(Step::user(first.clone()));
        *messages = vec![Message::user_text(first)];
        message_tokens(messages)
    }
}

/// Calls the model, retrying transient errors with backoff. Returns `None`
/// when the time budget runs out first.
async fn complete_with_retry(
    provider: &dyn Provider,
    req: &ChatRequest,
    deadline: Option<Instant>,
) -> Result<Option<ChatResponse>, ProviderError> {
    let mut delay = Duration::from_secs(2);
    let mut attempt = 0;
    loop {
        let call = provider.complete(req);
        let result = match deadline {
            Some(d) => match tokio::time::timeout_at(d.into(), call).await {
                Ok(r) => r,
                Err(_) => return Ok(None),
            },
            None => call.await,
        };
        match result {
            Err(ProviderError::Transient(msg)) if attempt < PROVIDER_RETRIES => {
                let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
                if left.is_some_and(|l| l < delay + MIN_TIME_FOR_TURN) {
                    return Err(ProviderError::Transient(msg));
                }
                eprintln!("gitagent: provider error, retrying in {delay:?}: {msg}");
                tokio::time::sleep(delay).await;
                delay *= 2;
                attempt += 1;
            }
            other => return other.map(Some),
        }
    }
}

#[cfg(test)]
mod tests;
