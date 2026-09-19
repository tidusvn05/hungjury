//! `MockBackend` — scripted responses for offline tests.
//!
//! Wire it in via a `model` string of `"mock:<anything>"` and inject the
//! backend into the jury via a backend override map.

use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;

use super::{AgentBackend, AgentRequest, AgentResult, BackendKind};
use crate::error::{Error, Result};

/// Handler deciding the response for a request. Receives the request,
/// returns text (or an error to simulate a failing CLI).
pub type MockHandler =
    Box<dyn Fn(&AgentRequest) -> std::result::Result<String, String> + Send + Sync>;

/// Deterministic in-process backend.
pub struct MockBackend {
    handler: MockHandler,
    /// Artificial per-call latency — an async sleep, so cancellation still
    /// interrupts the call (lets tests exercise timeout paths).
    delay: std::time::Duration,
    /// Every request seen (for assertions) — `(agent, prompt)`.
    pub calls: Mutex<Vec<(String, String)>>,
}

impl MockBackend {
    /// Mock with a custom handler.
    pub fn new(
        handler: impl Fn(&AgentRequest) -> std::result::Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
            delay: std::time::Duration::ZERO,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Builder: inject `d` of artificial latency into every call.
    #[allow(dead_code)] // used by integration tests
    pub fn with_delay(mut self, d: std::time::Duration) -> Self {
        self.delay = d;
        self
    }

    /// Mock that returns canned bodies per agent: exact match on
    /// `req.agent`, else `name@target` prefix match; missing keys get `{}`.
    pub fn canned(responses: &[(&'static str, &'static str)]) -> Self {
        let map: Vec<(String, String)> = responses
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Self::new(move |req| {
            if let Some((_, v)) = map.iter().find(|(k, _)| req.agent == *k) {
                return Ok(v.clone());
            }
            Ok(map
                .iter()
                .find(|(k, _)| {
                    req.agent
                        .strip_prefix(k.as_str())
                        .is_some_and(|rest| rest.starts_with('@') || rest.starts_with('#'))
                })
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "{}".to_string()))
        })
    }
}

#[async_trait]
impl AgentBackend for MockBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Mock
    }

    fn supports_fs(&self) -> bool {
        false
    }

    async fn run(&self, req: AgentRequest) -> Result<AgentResult> {
        self.calls
            .lock()
            .map(|mut c| c.push((req.agent.clone(), req.prompt.clone())))
            .unwrap_or_default();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let started = Instant::now();
        match (self.handler)(&req) {
            Ok(text) => Ok(AgentResult {
                text,
                backend: BackendKind::Mock,
                model: req.model,
                duration: started.elapsed(),
                usage: None,
                stderr_tail: String::new(),
            }),
            Err(msg) => Err(Error::Backend {
                backend: "mock",
                message: msg,
                stderr_tail: String::new(),
            }),
        }
    }
}
