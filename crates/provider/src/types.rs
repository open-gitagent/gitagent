use gitagent_loader::{Effort, Sampling};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Map<String, Value>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        /// Images returned by the tool (e.g. `image_read`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageData>,
    },
}

/// A base64-encoded image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
    /// The provider's own content blocks for this message, replayed verbatim to
    /// the same provider (thinking blocks and their signatures must come back
    /// unchanged). Other providers use `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawContent {
    pub provider: String,
    pub blocks: Vec<Value>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![Content::Text { text: text.into() }],
            raw: None,
        }
    }

    pub fn tool_results(results: Vec<Content>) -> Self {
        Message {
            role: Role::User,
            content: results,
            raw: None,
        }
    }
}

/// A tool as offered to the model. `input_schema` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Map<String, Value>,
}

/// Where to place prompt-cache breakpoints (providers without explicit
/// breakpoints ignore this).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CachePlan {
    pub system: bool,
    /// Number of trailing messages that get a breakpoint.
    pub last_messages: u8,
    /// Ask for the one-hour cache lifetime instead of the five-minute one.
    pub one_hour: bool,
}

impl CachePlan {
    /// Builds a plan from a profile's `cache.breakpoints` list: `system`,
    /// `last_message`, `last_two_messages`. Unknown names are ignored.
    pub fn from_breakpoints(names: &[String]) -> Self {
        let mut plan = CachePlan::default();
        for name in names {
            match name.as_str() {
                "system" => plan.system = true,
                "last_message" => plan.last_messages = plan.last_messages.max(1),
                "last_two_messages" => plan.last_messages = plan.last_messages.max(2),
                _ => {}
            }
        }
        plan
    }
}

/// Provider-side clearing of old tool results (Anthropic context editing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerMasking {
    /// Start clearing once the prompt reaches this many tokens.
    pub trigger_tokens: u32,
    /// Most recent tool results to keep.
    pub keep: u32,
    /// Clear only when at least this much can go (keeps clears batched).
    pub clear_at_least: u32,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub effort: Effort,
    pub temperature: Option<f32>,
    pub sampling: Sampling,
    pub max_output_tokens: u32,
    pub cache: CachePlan,
    pub server_masking: Option<ServerMasking>,
}

/// Token usage for one call. `input_tokens` is the full prompt, including
/// tokens read from or written to the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    /// The provider's safety classifiers declined the request.
    Refusal,
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The model that produced the response, when the provider reports it
    /// (it can differ from the requested one after a refusal fallback).
    pub model: Option<String>,
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    /// Provider content blocks to store with the assistant message.
    pub raw: Option<RawContent>,
    /// Tool results the provider cleared from its copy of the history.
    pub cleared_tool_results: u64,
}

impl ChatResponse {
    /// The assistant message to append to the history.
    pub fn to_message(&self) -> Message {
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(Content::Text {
                text: self.text.clone(),
            });
        }
        content.extend(self.tool_calls.iter().map(|c| Content::ToolUse {
            id: c.id.clone(),
            name: c.name.clone(),
            input: c.arguments.clone(),
        }));
        Message {
            role: Role::Assistant,
            content,
            raw: self.raw.clone(),
        }
    }
}
