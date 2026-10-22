//! Model provider interface and adapters.
//!
//! Adapters are thin hand-written HTTP clients so that the bytes sent to the
//! provider stay under our control (prompt-cache hits depend on it).

pub mod anthropic;
pub mod compat;
mod fallback;
pub mod google;
pub mod openai;
mod scripted;
mod sse;
#[cfg(test)]
mod test_server;
mod types;

pub use anthropic::AnthropicProvider;
pub use compat::CompatProvider;
pub use fallback::FallbackProvider;
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
    let primary = for_model(&spec.model, &spec.profile, &spec.profile_source)?;
    if spec.fallbacks.is_empty() {
        return Ok(primary);
    }
    let mut chain = vec![(spec.model.to_string(), spec.model.model.clone(), primary)];
    for fb in &spec.fallbacks {
        // A fallback that cannot be set up (no key, say) must not stop a
        // run whose own model works.
        match for_model(&fb.model, &fb.profile, "fallback profile") {
            Ok(provider) => chain.push((fb.model.to_string(), fb.model.model.clone(), provider)),
            Err(e) => eprintln!(
                "gitagent: warning: fallback model {} left out: {e:#}",
                fb.model
            ),
        }
    }
    if chain.len() == 1 {
        return Ok(chain.pop().expect("the primary").2);
    }
    Ok(Box::new(FallbackProvider::new(chain)))
}

/// One model's adapter. A model with a base URL goes to that
/// OpenAI-compatible endpoint, with the key from `{PROVIDER}_API_KEY` or
/// `GITAGENT_API_KEY`.
fn for_model(
    model: &gitagent_loader::ModelRef,
    profile: &gitagent_loader::Profile,
    profile_source: &str,
) -> anyhow::Result<Box<dyn Provider>> {
    use anyhow::Context;
    if let Some(url) = &model.base_url {
        return Ok(Box::new(CompatProvider::new(
            url.clone(),
            compat::api_key(&model.provider),
        )?));
    }
    match model.provider.as_str() {
        "scripted" => Ok(Box::new(ScriptedProvider::from_file(
            std::path::Path::new(&model.model),
        )?)),
        "anthropic" => {
            let cfg = profile
                .anthropic
                .clone()
                .with_context(|| format!("profile {profile_source} has no `anthropic` section"))?;
            Ok(Box::new(AnthropicProvider::from_env(cfg)?))
        }
        "openai" => {
            let cfg = profile
                .openai
                .clone()
                .with_context(|| format!("profile {profile_source} has no `openai` section"))?;
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

#[cfg(test)]
mod for_spec_tests {
    use super::*;

    #[test]
    fn a_fallback_that_cannot_be_set_up_is_left_out() {
        let mut spec = gitagent_loader::load(&gitagent_loader::LoadOptions {
            model: Some("openai:m@http://127.0.0.1:9/v1".into()),
            ..Default::default()
        })
        .unwrap();
        spec.fallbacks.push(gitagent_loader::Fallback {
            model: gitagent_loader::ModelRef::parse("scripted:/nonexistent/script.yaml").unwrap(),
            profile: spec.profile.clone(),
        });
        assert!(for_spec(&spec).is_ok());
    }
}
