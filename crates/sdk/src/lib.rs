//! A library interface over gitagent.
//!
//! `query(prompt, options)` runs one agent loop and streams messages: a
//! `system` / `init` message, then an `assistant` and a `user` message per
//! turn (tool uses and their results), `system` / `compact_boundary` when
//! the history is compacted, and a final `result` with turns, usage and
//! cost. The stream's field names are `session_id`, `parent_tool_use_id`,
//! `num_turns`, `total_cost_usd`, `modelUsage`, …, and tools appear under
//! the `.claude/` tool and argument names (`Bash` / `command`), as they do
//! in `.claude/settings.json` hooks.
//!
//! The loop underneath is gitagent's own (`gitagent_core::run`): the
//! stream shape is the interface, not the implementation.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gitagent_core::{Outcome, RunOptions, RunReport};
use gitagent_hooks::{Decision, Matcher, ToolHooks};
use gitagent_loader::{sdk as names, AgentDef, AgentSpec, LoadOptions};
use gitagent_record::Events;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

// ---------------------------------------------------------------- options

/// Where settings files are read from. With none, only gitagent's
/// built-in configuration applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    /// `~/.gitagent/` and `~/.claude/`.
    User,
    /// The working directory's `gitagent.yaml`, `.gitagent/` and `.claude/`.
    Project,
}

/// A sub-agent given in code.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentDefinition {
    pub description: String,
    pub prompt: String,
    /// `.claude/` or gitagent tool names; `None` inherits the parent's tools.
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub effort: Option<String>,
}

impl AgentDefinition {
    fn to_def(&self, name: &str) -> AgentDef {
        let general = gitagent_loader::GENERAL_AGENT;
        let _ = general;
        AgentDef {
            name: name.to_string(),
            description: self.description.clone(),
            tools: self.tools.as_ref().map(|t| names::normalize_tool_list(t)),
            disallowed_tools: names::normalize_tool_list(&self.disallowed_tools),
            max_turns: self.max_turns.unwrap_or(30),
            model: self.model.clone().filter(|m| m != "inherit"),
            effort: self.effort.clone(),
            instructions: self.prompt.clone(),
        }
    }
}

/// Prices per million tokens, for `total_cost_usd`. Without them the cost
/// is reported as 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Options for `query`.
#[derive(Clone, Default)]
pub struct AgentOptions {
    /// `provider:model` (or a bare model id for Anthropic). Overrides the config.
    pub model: Option<String>,
    /// Replaces the built-in system prompt.
    pub system_prompt: Option<String>,
    /// Added after the system prompt.
    pub append_system_prompt: Option<String>,
    /// The agent's working directory (default: the current directory).
    pub cwd: Option<PathBuf>,
    /// `.claude/` or gitagent tool names; empty means every tool.
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    /// Wall-clock budget (default: `timeout_s` from the config).
    pub timeout: Option<Duration>,
    /// Sub-agents by name. Given agents turn the sub-agent tool on.
    pub agents: BTreeMap<String, AgentDefinition>,
    /// Hook callbacks.
    pub hooks: Vec<HookMatcher>,
    /// Which settings files to read; the default is none.
    pub setting_sources: Vec<SettingSource>,
    /// gitagent-specific: a profile name or path, a `gitagent.yaml`, where
    /// records go, and prices for the cost estimate.
    pub profile: Option<String>,
    pub config_path: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub prices: Option<Prices>,
    /// MCP servers by name, with the settings of `mcp_servers` in
    /// gitagent.yaml; they replace configured servers of the same name.
    pub mcp_servers: BTreeMap<String, Value>,
    /// Asked when a call or the final response needs a person's approval
    /// (the agent's `compliance` section); without one those are denied.
    pub approver: Option<Arc<dyn gitagent_core::Approver>>,
    /// Conversation mode: a plain reply ends the run (instead of
    /// `task_complete`), with working instructions for talking with a person.
    pub conversation: bool,
}

// ---------------------------------------------------------------- hooks

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

/// What a hook callback receives, with `.claude/` tool and argument
/// names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookInput {
    pub session_id: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
}

/// What a hook callback returns (the parts gitagent acts on).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct HookOutput {
    #[serde(rename = "continue", skip_serializing_if = "Option::is_none")]
    pub continue_: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// `"block"` blocks; the other value is `"approve"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct HookSpecificOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_event_name: Option<String>,
    /// `allow`, `deny` or `ask` (treated as deny: nobody can be asked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision_reason: Option<String>,
    /// New arguments, in `.claude/` argument names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_tool_output: Option<String>,
}

impl HookOutput {
    pub fn allow() -> Self {
        HookOutput::default()
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                permission_decision: Some("deny".into()),
                permission_decision_reason: Some(reason.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub fn updated_input(input: Map<String, Value>) -> Self {
        HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                permission_decision: Some("allow".into()),
                updated_input: Some(input),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub fn updated_tool_output(text: impl Into<String>) -> Self {
        HookOutput {
            hook_specific_output: Some(HookSpecificOutput {
                updated_tool_output: Some(text.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

pub type HookFuture = Pin<Box<dyn Future<Output = HookOutput> + Send>>;
pub type HookCallback = Arc<dyn Fn(HookInput) -> HookFuture + Send + Sync>;

/// Callbacks for one event, for the tools a matcher selects.
#[derive(Clone)]
pub struct HookMatcher {
    pub event: HookEvent,
    /// `*` or `None` for every tool, exact names (`Edit|Write`), or a regex.
    pub matcher: Option<String>,
    pub hooks: Vec<HookCallback>,
}

impl HookMatcher {
    /// A matcher from a plain async function.
    pub fn new<F, Fut>(event: HookEvent, matcher: Option<&str>, f: F) -> Self
    where
        F: Fn(HookInput) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        HookMatcher {
            event,
            matcher: matcher.map(str::to_string),
            hooks: vec![Arc::new(move |input| Box::pin(f(input)))],
        }
    }
}

struct CallbackHooks {
    matchers: Vec<(Matcher, HookMatcher)>,
    cwd: String,
    session_id: String,
}

impl CallbackHooks {
    fn new(hooks: &[HookMatcher], cwd: &Path, session_id: &str) -> Option<Self> {
        let matchers: Vec<(Matcher, HookMatcher)> = hooks
            .iter()
            .filter_map(|h| Matcher::parse(h.matcher.as_deref()).map(|m| (m, h.clone())))
            .collect();
        (!matchers.is_empty()).then(|| CallbackHooks {
            matchers,
            cwd: cwd.to_string_lossy().into_owned(),
            session_id: session_id.to_string(),
        })
    }

    fn input(
        &self,
        event: HookEvent,
        tool: &str,
        args: &Map<String, Value>,
        response: Option<Value>,
    ) -> HookInput {
        HookInput {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            hook_event_name: match event {
                HookEvent::PreToolUse => "PreToolUse".into(),
                HookEvent::PostToolUse => "PostToolUse".into(),
            },
            tool_name: names::sdk_name(tool).to_string(),
            tool_input: Value::Object(names::to_sdk_input(tool, args)),
            tool_use_id: String::new(),
            tool_response: response,
        }
    }

    /// The rules for one answer.
    fn interpret(event: HookEvent, out: HookOutput) -> Decision {
        if out.continue_ == Some(false) {
            return Decision::Block {
                reason: out
                    .stop_reason
                    .unwrap_or_else(|| "stopped by a hook".into()),
            };
        }
        if out.decision.as_deref() == Some("block") {
            return Decision::Block {
                reason: out.reason.unwrap_or_else(|| "blocked by a hook".into()),
            };
        }
        let Some(specific) = out.hook_specific_output else {
            return Decision::Allow;
        };
        match event {
            HookEvent::PreToolUse => {
                if matches!(
                    specific.permission_decision.as_deref(),
                    Some("deny") | Some("ask")
                ) {
                    return Decision::Block {
                        reason: specific
                            .permission_decision_reason
                            .unwrap_or_else(|| "denied by a hook".into()),
                    };
                }
                match specific.updated_input {
                    Some(args) => Decision::Modify(Value::Object(args)),
                    None => Decision::Allow,
                }
            }
            HookEvent::PostToolUse => match specific.updated_tool_output {
                Some(text) => Decision::Modify(Value::String(text)),
                None => Decision::Allow,
            },
        }
    }
}

#[async_trait::async_trait]
impl ToolHooks for CallbackHooks {
    async fn pre(&self, tool: &str, arguments: &Map<String, Value>) -> Decision {
        let mut args = arguments.clone();
        let mut changed = false;
        for (matcher, h) in &self.matchers {
            if h.event != HookEvent::PreToolUse || !matcher.matches(tool) {
                continue;
            }
            for cb in &h.hooks {
                let input = self.input(HookEvent::PreToolUse, tool, &args, None);
                match Self::interpret(HookEvent::PreToolUse, cb(input).await) {
                    Decision::Allow => {}
                    Decision::Modify(Value::Object(sdk_args)) => {
                        args = names::from_sdk_input(tool, &sdk_args);
                        changed = true;
                    }
                    other => return other,
                }
            }
        }
        if changed {
            Decision::Modify(Value::Object(args))
        } else {
            Decision::Allow
        }
    }

    async fn post(
        &self,
        tool: &str,
        arguments: &Map<String, Value>,
        output: &str,
        is_error: bool,
    ) -> Decision {
        let mut out = output.to_string();
        let mut changed = false;
        for (matcher, h) in &self.matchers {
            if h.event != HookEvent::PostToolUse || !matcher.matches(tool) {
                continue;
            }
            for cb in &h.hooks {
                let response = json!({"output": out, "is_error": is_error});
                let input = self.input(HookEvent::PostToolUse, tool, arguments, Some(response));
                match Self::interpret(HookEvent::PostToolUse, cb(input).await) {
                    Decision::Allow => {}
                    Decision::Modify(Value::String(text)) => {
                        out = text;
                        changed = true;
                    }
                    other => return other,
                }
            }
        }
        if changed {
            Decision::Modify(Value::String(out))
        } else {
            Decision::Allow
        }
    }
}

// ---------------------------------------------------------------- messages

/// A content block as the Messages API writes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// Per-model usage (camelCase, as the stream writes it).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
    pub context_window: u64,
}

/// The API message inside an assistant or user message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompactMetadata {
    pub trigger: String,
    pub pre_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionDenial {
    pub tool_name: String,
    pub tool_use_id: String,
    pub tool_input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One message of the stream, tagged by `type`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    System {
        subtype: String,
        uuid: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[serde(rename = "permissionMode")]
        permission_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_metadata: Option<CompactMetadata>,
    },
    Assistant {
        uuid: String,
        session_id: String,
        parent_tool_use_id: Option<String>,
        message: ApiMessage,
    },
    User {
        uuid: String,
        session_id: String,
        parent_tool_use_id: Option<String>,
        message: ApiMessage,
    },
    Result {
        subtype: String,
        uuid: String,
        session_id: String,
        duration_ms: u64,
        duration_api_ms: u64,
        is_error: bool,
        num_turns: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        stop_reason: Option<String>,
        total_cost_usd: f64,
        usage: Usage,
        #[serde(rename = "modelUsage")]
        model_usage: BTreeMap<String, ModelUsage>,
        permission_denials: Vec<PermissionDenial>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        errors: Vec<String>,
    },
}

impl Message {
    pub fn is_result(&self) -> bool {
        matches!(self, Message::Result { .. })
    }
}

// ---------------------------------------------------------------- converter

/// Turns gitagent's events (`--events jsonl`) into the stream's messages.
pub struct Converter {
    session_id: String,
    model: String,
    context_window: u64,
    prices: Option<Prices>,
    started: Instant,
    api_ms: u64,
    turn_started: Option<Instant>,
    text: String,
    tool_uses: Vec<ContentBlock>,
    tool_results: Vec<ContentBlock>,
    last_call: Option<(String, String, Value)>,
    denials: Vec<PermissionDenial>,
    usage: Usage,
    cost: f64,
    seq: u64,
}

impl Converter {
    pub fn new(session_id: &str, model: &str, context_window: u64, prices: Option<Prices>) -> Self {
        Converter {
            session_id: session_id.to_string(),
            model: model.to_string(),
            context_window,
            prices,
            started: Instant::now(),
            api_ms: 0,
            turn_started: None,
            text: String::new(),
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
            last_call: None,
            denials: Vec::new(),
            usage: Usage::default(),
            cost: 0.0,
            seq: 0,
        }
    }

    fn uuid(&mut self) -> String {
        self.seq += 1;
        format!("{}-{:04}", self.session_id, self.seq)
    }

    /// The opening `system` / `init` message.
    pub fn init(
        &mut self,
        cwd: &Path,
        tools: &[String],
        agents: &[String],
        skills: &[String],
    ) -> Message {
        Message::System {
            subtype: "init".into(),
            uuid: self.uuid(),
            session_id: self.session_id.clone(),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            model: Some(self.model.clone()),
            tools: tools
                .iter()
                .map(|t| names::sdk_name(t).to_string())
                .collect(),
            agents: agents.to_vec(),
            skills: skills.to_vec(),
            permission_mode: Some("bypassPermissions".into()),
            compact_metadata: None,
        }
    }

    /// Messages produced by one event, in order.
    pub fn feed(&mut self, event: &Value) -> Vec<Message> {
        let kind = event["event"].as_str().unwrap_or("");
        match kind {
            "turn_start" => {
                self.turn_started = Some(Instant::now());
                self.text.clear();
                self.tool_uses.clear();
                self.tool_results.clear();
                Vec::new()
            }
            "model_text" => {
                if let Some(t) = event["text"].as_str() {
                    self.text = t.to_string();
                }
                if let Some(m) = event["model"].as_str() {
                    self.model = m.to_string();
                }
                if let Some(start) = self.turn_started {
                    self.api_ms += start.elapsed().as_millis() as u64;
                }
                Vec::new()
            }
            "tool_call" => {
                let id = event["id"].as_str().unwrap_or("").to_string();
                let name = event["name"].as_str().unwrap_or("").to_string();
                let args = event["arguments"].as_object().cloned().unwrap_or_default();
                let input = Value::Object(names::to_sdk_input(&name, &args));
                self.last_call = Some((id.clone(), name.clone(), input.clone()));
                self.tool_uses.push(ContentBlock::ToolUse {
                    id,
                    name: names::sdk_name(&name).to_string(),
                    input,
                });
                Vec::new()
            }
            "tool_result" => {
                self.tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: event["id"].as_str().unwrap_or("").to_string(),
                    content: event["content"].as_str().unwrap_or("").to_string(),
                    is_error: event["is_error"].as_bool().unwrap_or(false),
                });
                Vec::new()
            }
            "guard" => match event["kind"].as_str() {
                Some("hook_block") => {
                    if let Some((id, name, input)) = &self.last_call {
                        self.denials.push(PermissionDenial {
                            tool_name: names::sdk_name(name).to_string(),
                            tool_use_id: id.clone(),
                            tool_input: input.clone(),
                            reason: event["reason"].as_str().map(str::to_string),
                        });
                    }
                    Vec::new()
                }
                Some("compaction") => vec![Message::System {
                    subtype: "compact_boundary".into(),
                    uuid: self.uuid(),
                    session_id: self.session_id.clone(),
                    cwd: None,
                    model: None,
                    tools: Vec::new(),
                    agents: Vec::new(),
                    skills: Vec::new(),
                    permission_mode: None,
                    compact_metadata: Some(CompactMetadata {
                        trigger: "auto".into(),
                        pre_tokens: event["pre_tokens"].as_u64().unwrap_or(0),
                    }),
                }],
                _ => Vec::new(),
            },
            "turn_end" => {
                let u = &event["usage"];
                let turn = Usage {
                    input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                    cache_read_input_tokens: u["cache_read_tokens"].as_u64().unwrap_or(0),
                    cache_creation_input_tokens: u["cache_write_tokens"].as_u64().unwrap_or(0),
                };
                self.usage.input_tokens += turn.input_tokens;
                self.usage.output_tokens += turn.output_tokens;
                self.usage.cache_read_input_tokens += turn.cache_read_input_tokens;
                self.usage.cache_creation_input_tokens += turn.cache_creation_input_tokens;
                self.cost += cost_of(self.prices, &turn);
                let mut content = Vec::new();
                if !self.text.is_empty() {
                    content.push(ContentBlock::Text {
                        text: std::mem::take(&mut self.text),
                    });
                }
                content.append(&mut self.tool_uses);
                let stop_reason = if content
                    .iter()
                    .any(|c| matches!(c, ContentBlock::ToolUse { .. }))
                {
                    "tool_use"
                } else {
                    "end_turn"
                };
                let mut out = vec![Message::Assistant {
                    uuid: self.uuid(),
                    session_id: self.session_id.clone(),
                    parent_tool_use_id: None,
                    message: ApiMessage {
                        role: "assistant".into(),
                        content,
                        model: Some(self.model.clone()),
                        stop_reason: Some(stop_reason.into()),
                        usage: Some(turn),
                    },
                }];
                if !self.tool_results.is_empty() {
                    out.push(Message::User {
                        uuid: self.uuid(),
                        session_id: self.session_id.clone(),
                        parent_tool_use_id: None,
                        message: ApiMessage {
                            role: "user".into(),
                            content: std::mem::take(&mut self.tool_results),
                            model: None,
                            stop_reason: None,
                            usage: None,
                        },
                    });
                }
                out
            }
            "run_end" => {
                let outcome = event["outcome"].as_str().unwrap_or("");
                let (subtype, is_error) = match outcome {
                    "completed" => ("success", false),
                    "max_turns" => ("error_max_turns", true),
                    _ => ("error_during_execution", true),
                };
                let mut model_usage = BTreeMap::new();
                model_usage.insert(
                    self.model.clone(),
                    ModelUsage {
                        input_tokens: self.usage.input_tokens,
                        output_tokens: self.usage.output_tokens,
                        cache_read_input_tokens: self.usage.cache_read_input_tokens,
                        cache_creation_input_tokens: self.usage.cache_creation_input_tokens,
                        cost_usd: usd(self.cost),
                        context_window: self.context_window,
                    },
                );
                vec![Message::Result {
                    subtype: subtype.into(),
                    uuid: self.uuid(),
                    session_id: self.session_id.clone(),
                    duration_ms: self.started.elapsed().as_millis() as u64,
                    duration_api_ms: self.api_ms,
                    is_error,
                    num_turns: event["turns"].as_u64().unwrap_or(0) as u32,
                    result: event["summary"].as_str().map(str::to_string),
                    stop_reason: (!is_error).then(|| "end_turn".to_string()),
                    total_cost_usd: usd(self.cost),
                    usage: self.usage.clone(),
                    model_usage,
                    permission_denials: std::mem::take(&mut self.denials),
                    errors: if is_error {
                        vec![outcome.to_string()]
                    } else {
                        Vec::new()
                    },
                }]
            }
            _ => Vec::new(),
        }
    }
}

/// Dollars to the cent's hundredth, so costs survive a JSON round trip unchanged.
fn usd(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn cost_of(prices: Option<Prices>, u: &Usage) -> f64 {
    let Some(p) = prices else {
        return 0.0;
    };
    let uncached = u
        .input_tokens
        .saturating_sub(u.cache_read_input_tokens)
        .saturating_sub(u.cache_creation_input_tokens);
    (uncached as f64 * p.input
        + u.cache_read_input_tokens as f64 * p.cache_read
        + u.cache_creation_input_tokens as f64 * p.cache_write
        + u.output_tokens as f64 * p.output)
        / 1e6
}

// ---------------------------------------------------------------- query

/// A running query: messages arrive as the agent works; `finish` waits for
/// the run and returns gitagent's own report.
pub struct Query {
    rx: mpsc::UnboundedReceiver<Message>,
    handle: tokio::task::JoinHandle<Result<RunReport>>,
    kill: gitagent_core::KillSwitch,
}

impl Query {
    /// The next message, or `None` once the stream has ended.
    pub async fn next(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    /// Every remaining message.
    pub async fn collect(&mut self) -> Vec<Message> {
        let mut out = Vec::new();
        while let Some(m) = self.rx.recv().await {
            out.push(m);
        }
        out
    }

    /// Stops the agent before its next model call or tool call (the kill
    /// switch); the run ends with the outcome `killed`.
    pub fn kill(&self) {
        self.kill.trip();
    }

    /// Waits for the run to end.
    pub async fn finish(self) -> Result<RunReport> {
        self.handle.await.context("the agent task panicked")?
    }
}

/// The loaded configuration and the run options an `AgentOptions` asks for.
/// Public so the CLI can apply the same options.
pub struct Prepared {
    pub spec: AgentSpec,
    pub work_dir: PathBuf,
    pub out_dir: PathBuf,
    pub timeout: Duration,
    pub tool_allowlist: Option<Vec<String>>,
    pub tool_denylist: Vec<String>,
    pub hook_callbacks: Option<Arc<dyn ToolHooks>>,
    pub session_id: String,
}

pub fn prepare(options: &AgentOptions) -> Result<Prepared> {
    let work_dir = match &options.cwd {
        Some(d) => d.clone(),
        None => std::env::current_dir().context("reading the current directory")?,
    };
    let project = options.setting_sources.contains(&SettingSource::Project);
    let user = options.setting_sources.contains(&SettingSource::User);
    let mut spec = gitagent_loader::load(&LoadOptions {
        config_path: options.config_path.clone(),
        model: options.model.clone().map(|m| {
            if m.contains(':') {
                m
            } else {
                format!("anthropic:{m}")
            }
        }),
        profile: options.profile.clone(),
        conversation: options.conversation,
        project_dir: project.then(|| work_dir.clone()),
        home_dir: if user {
            std::env::var_os("HOME").map(PathBuf::from)
        } else {
            None
        },
    })?;
    if let Some(p) = &options.system_prompt {
        spec.system_prompt = format!("{}\n", p.trim_end());
    }
    if let Some(extra) = &options.append_system_prompt {
        spec.system_prompt.push('\n');
        spec.system_prompt.push_str(extra.trim_end());
        spec.system_prompt.push('\n');
    }
    if let Some(n) = options.max_turns {
        spec.config.max_turns = n;
    }
    for (name, v) in &options.mcp_servers {
        let server = gitagent_loader::mcp::parse(name, v, &work_dir)
            .map_err(|e| anyhow::anyhow!("MCP server {name:?}: {e}"))?;
        spec.mcp_servers.retain(|s| s.name != *name);
        spec.mcp_servers.push(server);
    }
    spec.mcp_servers.sort_by(|a, b| a.name.cmp(&b.name));
    if !options.agents.is_empty() {
        spec.profile.tools.subagents = true;
        for (name, def) in &options.agents {
            spec.agents.retain(|a| a.name != *name);
            spec.agents.push(def.to_def(name));
        }
        if !spec
            .agents
            .iter()
            .any(|a| a.name == gitagent_loader::GENERAL_AGENT)
        {
            // The general-purpose agent stays alongside the defined ones.
            spec.agents.push(gitagent_loader::AgentDef {
                name: gitagent_loader::GENERAL_AGENT.into(),
                description: "A helper for a self-contained piece of work.".into(),
                tools: None,
                disallowed_tools: Vec::new(),
                max_turns: 30,
                model: None,
                effort: None,
                instructions: String::new(),
            });
        }
        spec.agents.sort_by(|a, b| a.name.cmp(&b.name));
    }
    let session_id = format!("sdk-{}", std::process::id());
    let out_dir = options
        .out_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("gitagent").join(&session_id));
    let timeout = options
        .timeout
        .unwrap_or(Duration::from_secs(spec.config.timeout_s));
    let tool_allowlist = (!options.allowed_tools.is_empty())
        .then(|| names::normalize_tool_list(&options.allowed_tools));
    let tool_denylist = names::normalize_tool_list(&options.disallowed_tools);
    let hook_callbacks: Option<Arc<dyn ToolHooks>> =
        CallbackHooks::new(&options.hooks, &work_dir, &session_id)
            .map(|h| Arc::new(h) as Arc<dyn ToolHooks>);
    Ok(Prepared {
        spec,
        work_dir,
        out_dir,
        timeout,
        tool_allowlist,
        tool_denylist,
        hook_callbacks,
        session_id,
    })
}

/// Runs one task and streams its messages.
pub async fn query(prompt: impl Into<String>, options: AgentOptions) -> Result<Query> {
    let prompt = prompt.into();
    let p = prepare(&options)?;
    let provider = gitagent_provider::for_spec(&p.spec)?;
    let (tx, rx) = mpsc::unbounded_channel::<Message>();

    let converter = Arc::new(Mutex::new(Converter::new(
        &p.session_id,
        &p.spec.model.to_string(),
        u64::from(p.spec.profile.context_window),
        options.prices,
    )));
    let kill = gitagent_core::KillSwitch::default();
    let (mcp, warnings) = gitagent_core::connect_mcp(&p.spec).await;
    for w in warnings {
        eprintln!("gitagent: warning: {w}");
    }
    let tool_names: Vec<String> = gitagent_core::offered_tools(
        &p.spec,
        mcp.as_deref(),
        None,
        p.tool_allowlist.as_deref(),
        &p.tool_denylist,
    )
    .into_iter()
    .map(|t| t.name)
    .collect();
    let init = converter.lock().unwrap().init(
        &p.work_dir,
        &tool_names,
        &p.spec
            .agents
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>(),
        &p.spec
            .skills
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
    );
    let _ = tx.send(init);
    let events = {
        let tx = tx.clone();
        let converter = converter.clone();
        Events::to_callback(move |event| {
            let messages = converter.lock().unwrap().feed(&event);
            for m in messages {
                let _ = tx.send(m);
            }
        })
    };
    let run_opts = RunOptions {
        instruction: prompt,
        work_dir: p.work_dir.clone(),
        out_dir: p.out_dir.clone(),
        timeout: Some(p.timeout),
        session_id: Some(p.session_id.clone()),
        events,
        resume: None,
        sandbox: None,
        tool_allowlist: p.tool_allowlist.clone(),
        tool_denylist: p.tool_denylist.clone(),
        hook_callbacks: p.hook_callbacks.clone(),
        mcp,
        approver: options.approver.clone(),
        kill: Some(kill.clone()),
        steer: None,
        external_tools: None,
    };
    let spec = p.spec;
    let (work_dir, session_id) = (p.work_dir.clone(), p.session_id.clone());
    let handle = tokio::spawn(async move {
        let finished = gitagent_core::run(&spec, provider.as_ref(), run_opts).await?;
        gitagent_core::end_session(&spec, &work_dir, &session_id).await;
        drop(tx);
        Ok(finished.report)
    });
    Ok(Query { rx, handle, kill })
}

/// The outcome a result message reports, for callers that want gitagent's enum.
pub fn outcome_of(subtype: &str) -> Outcome {
    match subtype {
        "success" => Outcome::Completed,
        "error_max_turns" => Outcome::MaxTurns,
        other => Outcome::ProviderFailed(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
