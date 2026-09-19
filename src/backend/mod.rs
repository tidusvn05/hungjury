//! `AgentBackend` abstraction: one CLI agent == one backend.
//!
//! Adding a backend = one file + one arm in [`for_kind`]. Model strings follow
//! `"<backend>:<model>[@<effort>]"`; the part after `:` is passed through to
//! the CLI (effort handling differs per backend).

pub mod claude;
pub mod codex;
pub mod devin;
pub mod mock;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use crate::error::{Error, Result};

/// Which CLI backs an [`AgentBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// `devin` CLI.
    Devin,
    /// `claude` CLI.
    Claude,
    /// `codex` CLI.
    Codex,
    /// In-process mock for tests (`mock:<model>`).
    Mock,
}

impl BackendKind {
    /// Split `"<backend>:<model>"` → `(kind, Some(model))`; bare `"<backend>"`
    /// → `(kind, None)`.
    pub fn parse(model_string: &str) -> Result<(BackendKind, Option<String>)> {
        let s = model_string.trim();
        let (name, model) = match s.split_once(':') {
            Some((b, m)) => (
                b,
                if m.is_empty() {
                    None
                } else {
                    Some(m.to_string())
                },
            ),
            None => (s, None),
        };
        let kind = match name.to_lowercase().as_str() {
            "devin" => BackendKind::Devin,
            "claude" => BackendKind::Claude,
            "codex" => BackendKind::Codex,
            "mock" | "test" => BackendKind::Mock,
            other => {
                return Err(Error::BackendNotAvailable(format!(
                    "unknown backend '{other}' in model string '{model_string}'"
                )));
            }
        };
        Ok((kind, model))
    }

    /// Stable lowercase id used in logs, cache keys and audit lines.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Devin => "devin",
            BackendKind::Claude => "claude",
            BackendKind::Codex => "codex",
            BackendKind::Mock => "mock",
        }
    }

    /// Default `"<backend>:<model>"` strings for `(juror, judge)` — used by
    /// PATH auto-detection when the user configures nothing.
    pub fn default_models(&self) -> (String, String) {
        let (j, g) = match self {
            BackendKind::Devin => ("devin:swe-2-medium", "devin:claude-opus-5-high"),
            BackendKind::Claude => ("claude:haiku", "claude:opus@high"),
            BackendKind::Codex => ("codex:gpt-5.6-terra@low", "codex:gpt-5.6-sol@high"),
            BackendKind::Mock => ("mock:test", "mock:test"),
        };
        (j.to_string(), g.to_string())
    }

    /// Every real backend, in the jury auto-detect preference order.
    pub fn all_real() -> [BackendKind; 3] {
        [BackendKind::Claude, BackendKind::Codex, BackendKind::Devin]
    }

    /// Every real CLI found on `PATH`.
    pub fn detect_all() -> Vec<BackendKind> {
        Self::all_real()
            .into_iter()
            .filter(|k| crate::sys::find_on_path(k.as_str()).is_some())
            .collect()
    }

    /// First agent CLI found on `PATH`, in preference order
    /// claude → codex → devin (used for the default judge).
    pub fn detect() -> Option<BackendKind> {
        Self::detect_all().into_iter().next()
    }
}

/// Which tools the spawned agent may use. Jurors deciding over a text state
/// get none; a workspace state needs read-only exploration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolPolicy {
    /// No tools — the agent only sees the prompt (text state).
    #[default]
    None,
    /// Read-only tools — the agent may inspect `cwd` (workspace state).
    ReadOnly,
}

/// Token accounting — `None` when the CLI does not expose usage.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
}

/// One backend invocation.
pub struct AgentRequest {
    /// Fully-rendered user prompt.
    pub prompt: String,
    /// System prompt; only claude consumes it directly — other backends
    /// prepend it to `prompt`.
    pub system_prompt: Option<String>,
    /// Model id passed through to the CLI, if any.
    pub model: Option<String>,
    /// Working directory: empty temp dir for `ToolPolicy::None`, the
    /// workspace root for `ToolPolicy::ReadOnly`.
    pub cwd: PathBuf,
    /// Tool access granted to this call.
    pub tools: ToolPolicy,
    /// Per-call timeout.
    pub timeout: Duration,
    /// Agent name, for span/log correlation (e.g. `juror:claude:haiku#0`).
    pub agent: String,
    /// JSON Schema for structured output; claude (`--json-schema`) and codex
    /// (`--output-schema`) enforce it, devin ignores it (prompt-forced).
    pub json_schema: Option<serde_json::Value>,
}

/// What the CLI returned.
#[derive(Debug)]
#[allow(dead_code)] // diagnostics fields consumed by callers as needed
pub struct AgentResult {
    /// Final message / stdout text.
    pub text: String,
    /// Backend that produced it.
    pub backend: BackendKind,
    /// Model actually used, if known.
    pub model: Option<String>,
    /// Wall-clock duration of the call.
    pub duration: Duration,
    /// Token usage if the CLI reports it.
    pub usage: Option<TokenUsage>,
    /// Last bytes of stderr (kept for diagnostics even on success).
    pub stderr_tail: String,
}

/// An authenticated CLI agent usable as an LLM.
#[async_trait]
pub trait AgentBackend: Send + Sync {
    /// Which CLI this is.
    fn kind(&self) -> BackendKind;
    /// Whether the agent can read files inside `cwd` (workspace mode).
    #[allow(dead_code)]
    fn supports_fs(&self) -> bool;
    /// Run one prompt, return the final text.
    async fn run(&self, req: AgentRequest) -> Result<AgentResult>;
}

/// Construct a backend by kind — the only place `match` on kinds lives.
pub fn for_kind(kind: BackendKind) -> Arc<dyn AgentBackend> {
    match kind {
        BackendKind::Devin => Arc::new(devin::DevinBackend),
        BackendKind::Claude => Arc::new(claude::ClaudeBackend),
        BackendKind::Codex => Arc::new(codex::CodexBackend),
        BackendKind::Mock => Arc::new(mock::MockBackend::canned(&[])),
    }
}

/// Env vars that would flip a CLI onto metered API billing or confuse session
/// state — never inherited by spawned agents. Defined once, used by every
/// backend.
const BANNED_PREFIXES: &[&str] = &[
    "ANTHROPIC_",
    "OPENAI_",
    "CLAUDE_API",
    "CODEX_API",
    "DEVIN_API",
    "OPENHANDS_",
];

/// Strip billing-related env vars from a child command.
pub fn sanitized_env(cmd: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if BANNED_PREFIXES.iter().any(|p| k.starts_with(p)) {
            cmd.env_remove(&key);
        }
    }
}

/// Last `n` chars of a string for error messages.
pub fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars.iter().skip(chars.len().saturating_sub(n)).collect()
}

/// Lenient JSON extraction, ported from deepwiki-rs via agentwiki:
/// strict parse → fenced `json` block → depth-counted first `{…}` object.
pub fn extract_json(text: &str, agent: &str) -> Result<serde_json::Value> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(v);
    }
    // ```json … ``` fence
    if let Some(start) = trimmed.find("```json") {
        let body = &trimmed[start + 7..];
        if let Some(end) = body.find("```")
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(body[..end].trim())
        {
            return Ok(v);
        }
    }
    // Depth-counted first object/array, honoring strings/escapes.
    if let Some(start) = trimmed.find(['{', '[']) {
        let bytes = trimmed.as_bytes();
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for (i, &b) in bytes.iter().enumerate().skip(start) {
            match b {
                b'\\' if in_str => esc = !esc,
                b'"' if !esc => in_str = !in_str,
                b'{' | b'[' if !in_str => depth += 1,
                b'}' | b']' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v) =
                            serde_json::from_str::<serde_json::Value>(&trimmed[start..=i])
                        {
                            return Ok(v);
                        }
                        break;
                    }
                }
                _ => esc = false,
            }
        }
    }
    Err(Error::Parse {
        agent: agent.to_string(),
        message: format!("no valid JSON in response: {}", tail(text, 200)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_strings() {
        let (k, m) = BackendKind::parse("devin:swe-2-medium").unwrap();
        assert_eq!(k, BackendKind::Devin);
        assert_eq!(m.as_deref(), Some("swe-2-medium"));

        let (k, m) = BackendKind::parse("claude").unwrap();
        assert_eq!(k, BackendKind::Claude);
        assert_eq!(m, None);

        assert!(BackendKind::parse("openai:gpt-5").is_err());
    }

    #[test]
    fn extract_strict() {
        let v = extract_json("{\"a\": 1}", "t").unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn extract_fenced() {
        let v = extract_json("Here:\n```json\n{\"a\": 2}\n```\nThanks", "t").unwrap();
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn extract_prose_wrapped() {
        let v = extract_json("Sure! {\"a\": {\"b\": \"x{y}\"}} done", "t").unwrap();
        assert_eq!(v["a"]["b"], "x{y}");
    }

    #[test]
    fn extract_fails_on_prose() {
        assert!(extract_json("no json here", "t").is_err());
    }
}
