use serde::Deserialize;

use crate::Family;

/// Per-model-family tuning (BLUEPRINT §6). Every field is required so that a
/// profile always states its values explicitly.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub family: Family,
    pub tool_calling: ToolCalling,
    /// Which names the model sees for the tools: gitagent's own, or the
    /// `.claude/` ones (`Bash`, `Read`, `Edit`, … with their argument names).
    #[serde(default)]
    pub tool_naming: ToolNaming,
    pub edit_tool: EditTool,
    pub effort: Effort,
    /// Omitted for models that reject a temperature.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// `top_p`, `top_k` and stop sequences, from `agent.yaml`'s
    /// `model.constraints` only: a profile never sets them.
    #[serde(skip)]
    pub sampling: Sampling,
    pub max_output_tokens: u32,
    pub context_window: u32,
    pub exec_default_yield_ms: u64,
    pub shape: ShapeCfg,
    pub context: ContextCfg,
    pub guards: GuardsCfg,
    /// Probe the environment once and add it to the first user message.
    pub snapshot: bool,
    /// Append `prompts/lessons.md` (learned lessons) to the system prompt.
    pub lessons: bool,
    pub stop: StopCfg,
    pub cache: CacheCfg,
    pub tools: ToolsCfg,
    /// Anthropic-only request options; required when `family: anthropic`.
    #[serde(default)]
    pub anthropic: Option<AnthropicCfg>,
    /// OpenAI-only request options; required when `family: openai`.
    #[serde(default)]
    pub openai: Option<OpenAiCfg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCalling {
    Native,
    NativeWithTextFallback,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolNaming {
    #[default]
    Gitagent,
    Sdk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditTool {
    StrReplace,
    ApplyPatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl Effort {
    /// One level higher (`max` stays `max`).
    pub fn next(self) -> Effort {
        match self {
            Effort::Low => Effort::Medium,
            Effort::Medium => Effort::High,
            Effort::High => Effort::XHigh,
            Effort::XHigh | Effort::Max => Effort::Max,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// Sampling options beyond the temperature. Each provider sends the ones it
/// accepts; the loader warns about the rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sampling {
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop_sequences: Vec<String>,
}

impl Sampling {
    /// The settings `family` has no field for, by their agent.yaml names.
    pub fn unsupported(&self, family: crate::Family) -> Vec<&'static str> {
        let (top_k, stop) = match family {
            crate::Family::Anthropic | crate::Family::Google | crate::Family::Compat => {
                (true, true)
            }
            crate::Family::OpenAI => (false, false),
        };
        let mut out = Vec::new();
        if self.top_k.is_some() && !top_k {
            out.push("top_k");
        }
        if !self.stop_sequences.is_empty() && !stop {
            out.push("stop_sequences");
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeCfg {
    pub cap_tokens: u32,
    pub head_ratio: f32,
    pub collapse_min_run: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCfg {
    pub masking: Masking,
    pub mask_batch_tokens: u32,
    pub keep_last: u32,
    /// Summarize the history when fewer than `compact_at_tokens` of the
    /// context window are left (and when the provider reports overflow).
    pub compaction: bool,
    pub compact_at_tokens: u32,
}

/// How old tool results are hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Masking {
    None,
    /// gitagent replaces them with stubs at batched checkpoints.
    Client,
    /// The provider clears them on its side (Anthropic context editing); the
    /// history sent stays unchanged.
    Server,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardsCfg {
    /// Minutes left in every tool result, a reminder at `nudge_at` and a
    /// final call at `force_stop_at` (shares of the time budget).
    pub time_budget: bool,
    pub nudge_at: f32,
    pub force_stop_at: f32,
    pub loop_detector: bool,
    pub loop_repeats: u32,
    /// After a loop warning, use the next effort level for one call.
    pub loop_effort_bump: bool,
    /// Tell the model when 80% of its turns (model calls) are used, and
    /// again before the last one, so a run that runs out still ends with an
    /// answer. Always on for sub-agents.
    #[serde(default)]
    pub turn_warning: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopCfg {
    pub enabled: bool,
    pub max_rejections: u32,
    pub verify_patterns: Vec<String>,
    pub edit_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheCfg {
    pub breakpoints: Vec<String>,
    /// Cache lifetime where the provider lets us choose: `5m` or `1h`.
    #[serde(default = "default_ttl")]
    pub ttl: String,
}

fn default_ttl() -> String {
    "5m".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicCfg {
    /// `summarized` returns a readable summary of the model's reasoning (kept in
    /// the trajectory); `omitted` returns empty thinking text. Billing is the same.
    pub thinking_display: ThinkingDisplay,
    /// What the API does with a replayed thinking block whose conversation
    /// prefix changed: `error` (reject the request) or `drop_block`.
    pub thinking_binding: ThinkingBinding,
    /// Let the API re-run a refused request on its recommended fallback model.
    pub refusal_fallback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingBinding {
    Error,
    DropBlock,
}

/// Which optional tools the model is offered.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsCfg {
    /// Background sessions (`exec` with `background`) and the `write_stdin`
    /// tool for polling, input and Ctrl-C.
    pub write_stdin: bool,
    /// `image_read`: show the model an image file (vision models only).
    pub image_read: bool,
    /// `todo`: a checklist the model keeps for itself.
    pub todo: bool,
    /// `subagent`: start a sub-agent with a fresh context for a
    /// self-contained piece of work; only its summary comes back.
    #[serde(default)]
    pub subagents: bool,
    /// How many levels of sub-agents may nest (1: sub-agents cannot start
    /// sub-agents of their own).
    #[serde(default = "default_subagent_depth")]
    pub subagent_depth: u32,
    /// Sub-agents called in one reply run at the same time, this many at once.
    #[serde(default = "default_max_concurrent_subagents")]
    pub max_concurrent_subagents: u32,
    /// `plan`: phases of tasks run at the same time as sub-agents, then a
    /// reconciling step (agent.yaml `runtime.parallel_tasks`).
    #[serde(default)]
    pub plan: bool,
}

fn default_subagent_depth() -> u32 {
    1
}

fn default_max_concurrent_subagents() -> u32 {
    4
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCfg {
    /// Reasoning summary to request (`auto`, `concise`, `detailed`), or
    /// `none` to request none. Summaries make trajectories readable.
    pub reasoning_summary: String,
    /// Stable cache-routing key shared by every task, so requests with the
    /// same system prompt and tools land on the same prompt cache.
    #[serde(default)]
    pub prompt_cache_key: Option<String>,
}
