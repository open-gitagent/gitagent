//! Model fallback: try the models in order, moving to the next one when the
//! current one fails for good. Once moved, later calls stay on the new model,
//! so one conversation does not bounce between providers.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use async_trait::async_trait;
use gitagent_loader::Family;

use crate::{ChatRequest, ChatResponse, Provider, ProviderError};

/// Transient errors in a row before moving on (rate limits and overload
/// usually clear on retry; a provider that keeps failing does not).
const TRANSIENT_LIMIT: u32 = 3;

pub struct FallbackProvider {
    /// Label for messages, model id sent in requests, provider.
    chain: Vec<(String, String, Box<dyn Provider>)>,
    current: AtomicUsize,
    transient_in_a_row: AtomicU32,
}

impl FallbackProvider {
    /// `chain`: label (for messages), model id and provider, primary first.
    pub fn new(chain: Vec<(String, String, Box<dyn Provider>)>) -> Self {
        assert!(!chain.is_empty(), "a fallback chain needs a provider");
        FallbackProvider {
            chain,
            current: AtomicUsize::new(0),
            transient_in_a_row: AtomicU32::new(0),
        }
    }

    /// Which model in the chain is in use (0 is the primary).
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    fn advance(&self, from: usize, why: &ProviderError) -> bool {
        let next = from + 1;
        if next >= self.chain.len() {
            return false;
        }
        eprintln!(
            "gitagent: {} failed ({why}); switching to {}",
            self.chain[from].0, self.chain[next].0
        );
        self.current.store(next, Ordering::Relaxed);
        self.transient_in_a_row.store(0, Ordering::Relaxed);
        true
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        loop {
            let i = self.current();
            let (_, model, provider) = &self.chain[i];
            let result = if *model == req.model {
                provider.complete(req).await
            } else {
                let mut req = req.clone();
                req.model = model.clone();
                provider.complete(&req).await
            };
            match result {
                Ok(resp) => {
                    self.transient_in_a_row.store(0, Ordering::Relaxed);
                    return Ok(resp);
                }
                // A different model will not make the request smaller.
                Err(e @ ProviderError::ContextOverflow(_)) => return Err(e),
                Err(e @ ProviderError::Transient(_)) => {
                    let n = self.transient_in_a_row.fetch_add(1, Ordering::Relaxed) + 1;
                    if n < TRANSIENT_LIMIT || !self.advance(i, &e) {
                        // Let the caller back off and retry this model.
                        return Err(e);
                    }
                }
                Err(e @ ProviderError::Fatal(_)) => {
                    if !self.advance(i, &e) {
                        return Err(e);
                    }
                }
            }
        }
    }

    fn family(&self) -> Family {
        self.chain[self.current()].2.family()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers from a queue of results; records how often it was called.
    struct Fake {
        results: Mutex<Vec<Result<&'static str, ProviderError>>>,
        calls: AtomicU32,
    }

    fn fake(results: Vec<Result<&'static str, ProviderError>>) -> Box<Fake> {
        Box::new(Fake {
            results: Mutex::new(results.into_iter().rev().collect()),
            calls: AtomicU32::new(0),
        })
    }

    #[async_trait]
    impl Provider for Fake {
        async fn complete(&self, _req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.results.lock().unwrap().pop() {
                Some(Ok(text)) => Ok(response(text)),
                Some(Err(e)) => Err(e),
                None => Ok(response("")),
            }
        }
        fn family(&self) -> Family {
            Family::Compat
        }
    }

    fn response(text: &str) -> ChatResponse {
        ChatResponse {
            model: None,
            text: text.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            usage: Default::default(),
            stop_reason: crate::StopReason::EndTurn,
            raw: None,
            cleared_tool_results: 0,
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "a-model".into(),
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            effort: gitagent_loader::Effort::High,
            temperature: None,
            max_output_tokens: 1000,
            cache: Default::default(),
            server_masking: None,
        }
    }

    fn link(name: &str, fake: Box<Fake>) -> (String, String, Box<dyn Provider>) {
        (name.into(), format!("{name}-model"), fake)
    }

    #[tokio::test]
    async fn a_fatal_error_moves_to_the_next_model_and_stays_there() {
        let p = FallbackProvider::new(vec![
            link("a", fake(vec![Err(ProviderError::Fatal("bad key".into()))])),
            link("b", fake(vec![Ok("from b"), Ok("b again")])),
        ]);
        assert_eq!(p.complete(&req()).await.unwrap().text, "from b");
        assert_eq!(p.current(), 1);
        assert_eq!(p.complete(&req()).await.unwrap().text, "b again");
    }

    #[tokio::test]
    async fn transient_errors_are_retried_before_moving_on() {
        let t = || Err(ProviderError::Transient("overloaded".into()));
        let p = FallbackProvider::new(vec![
            link("a", fake(vec![t(), t(), t()])),
            link("b", fake(vec![Ok("from b")])),
        ]);
        // The first two transient errors go back to the caller to retry.
        assert!(matches!(
            p.complete(&req()).await,
            Err(ProviderError::Transient(_))
        ));
        assert!(matches!(
            p.complete(&req()).await,
            Err(ProviderError::Transient(_))
        ));
        assert_eq!(p.current(), 0);
        // The third moves to the next model.
        assert_eq!(p.complete(&req()).await.unwrap().text, "from b");
        assert_eq!(p.current(), 1);
    }

    #[tokio::test]
    async fn the_last_model_reports_its_error_and_overflow_never_switches() {
        let p = FallbackProvider::new(vec![link(
            "a",
            fake(vec![Err(ProviderError::Fatal("down".into()))]),
        )]);
        assert!(matches!(
            p.complete(&req()).await,
            Err(ProviderError::Fatal(_))
        ));

        let p = FallbackProvider::new(vec![
            link(
                "a",
                fake(vec![Err(ProviderError::ContextOverflow("too long".into()))]),
            ),
            link("b", fake(vec![Ok("unused")])),
        ]);
        assert!(matches!(
            p.complete(&req()).await,
            Err(ProviderError::ContextOverflow(_))
        ));
        assert_eq!(p.current(), 0);
    }
}
