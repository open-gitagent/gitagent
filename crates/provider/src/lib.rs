//! Model provider interface and adapters.
//!
//! Adapters are thin hand-written HTTP clients so that the bytes sent to the
//! provider stay under our control (prompt-cache hits depend on it).

pub mod anthropic;
pub mod compat;
pub mod google;
pub mod openai;
mod scripted;
mod sse;
#[cfg(test)]
mod test_server;
mod types;

pub use anthropic::AnthropicProvider;
pub use compat::CompatProvider;
pub use google::GoogleProvider;
pub use openai::OpenAiProvider;
pub use scripted::ScriptedProvider;
pub use types::*;

use async_trait::async_trait;
use gitagent_loader::Family;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError>;
    fn family(&self) -> Family;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Worth retrying after a backoff: rate limits, overload, 5xx, network errors.
    #[error("transient provider error: {0}")]
    Transient(String),
    /// The request does not fit the model's context window.
    #[error("context window exceeded: {0}")]
    ContextOverflow(String),
    /// Anything else; retrying will not help.
    #[error("provider error: {0}")]
    Fatal(String),
}

/// The provider a loaded configuration asks for: `scripted`, `anthropic`,
/// `openai`, `google` / `gemini`, or any OpenAI-compatible endpoint by name.
pub fn for_spec(spec: &gitagent_loader::AgentSpec) -> anyhow::Result<Box<dyn Provider>> {
    use anyhow::Context;
    match spec.model.provider.as_str() {
        "scripted" => Ok(Box::new(ScriptedProvider::from_file(
            std::path::Path::new(&spec.model.model),
        )?)),
        "anthropic" => {
            let cfg = spec.profile.anthropic.clone().with_context(|| {
                format!("profile {} has no `anthropic` section", spec.profile_source)
            })?;
            Ok(Box::new(AnthropicProvider::from_env(cfg)?))
        }
        "openai" => {
            let cfg = spec.profile.openai.clone().with_context(|| {
                format!("profile {} has no `openai` section", spec.profile_source)
            })?;
            Ok(Box::new(OpenAiProvider::from_env(cfg)?))
        }
        "google" | "gemini" => Ok(Box::new(GoogleProvider::from_env()?)),
        other => Ok(Box::new(CompatProvider::from_env(other)?)),
    }
}

/// HTTP client shared by the adapters: rustls with bundled roots, a connect
/// timeout, and a read timeout long enough for streams that send keep-alives.
pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("gitagent/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(300))
        .build()?)
}

/// A temperature as JSON with the digits as written (0.7, not 0.699999988).
pub(crate) fn temperature_json(t: f32) -> serde_json::Value {
    serde_json::from_str(&t.to_string()).unwrap_or_else(|_| serde_json::json!(t))
}
