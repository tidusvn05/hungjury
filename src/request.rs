//! The `decide` request: an unstructured `state` plus typed `questions`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::question::{Question, valid_key};

/// The unstructured input under judgment.
#[derive(Debug, Clone)]
pub enum State {
    /// Free text pasted inline.
    Text(String),
    /// A repository/directory the agents explore read-only.
    Workspace {
        /// Workspace root (juror cwd).
        path: PathBuf,
        /// Caller hint, e.g. "look at the diff vs main".
        hint: Option<String>,
    },
}

impl State {
    /// Is this a workspace state?
    pub fn is_workspace(&self) -> bool {
        matches!(self, State::Workspace { .. })
    }

    /// Text for hashing / prompt embedding. Workspace states contribute
    /// their canonical path (the agent explores; content isn't inlined).
    pub fn cache_material(&self) -> String {
        match self {
            State::Text(t) => t.clone(),
            State::Workspace { path, hint } => {
                format!("workspace:{} hint:{}", path.display(), hint.as_deref().unwrap_or(""))
            }
        }
    }
}

/// `state` in the input JSON: bare string, or
/// `{"workspace": "<dir>", "hint": "…"}`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StateIn {
    Text(String),
    Ws {
        workspace: PathBuf,
        #[serde(default)]
        hint: Option<String>,
    },
}

/// `{"state": …, "questions": {…}}` — the whole request file.
#[derive(Debug, Deserialize)]
struct RequestIn {
    state: StateIn,
    questions: BTreeMap<String, Question>,
}

/// A validated decide request.
#[derive(Debug, Clone)]
pub struct Request {
    /// What is being judged.
    pub state: State,
    /// Question key → question. `BTreeMap` keeps a stable order everywhere
    /// (prompts, schemas, canonical hashing).
    pub questions: BTreeMap<String, Question>,
}

impl Request {
    /// Parse the `{state, questions}` JSON document.
    pub fn from_json(text: &str) -> Result<Request> {
        let r: RequestIn = serde_json::from_str(text).map_err(|e| {
            Error::Request(format!("invalid request JSON: {e}"))
        })?;
        let state = match r.state {
            StateIn::Text(t) => State::Text(t),
            StateIn::Ws { workspace, hint } => State::Workspace {
                path: workspace,
                hint,
            },
        };
        Request::new(state, r.questions)
    }

    /// Build + validate.
    pub fn new(state: State, questions: BTreeMap<String, Question>) -> Result<Request> {
        if questions.is_empty() {
            return Err(Error::Request("at least one question is required".to_string()));
        }
        for (key, q) in &questions {
            if !valid_key(key) {
                return Err(Error::Request(format!(
                    "question key '{key}' must match [A-Za-z_][A-Za-z0-9_]*"
                )));
            }
            match q {
                Question::Choice { criteria, .. } if criteria.len() < 2 => {
                    return Err(Error::Request(format!(
                        "choice question '{key}' needs ≥2 options"
                    )));
                }
                Question::Score { criteria, .. } if criteria.len() < 2 => {
                    return Err(Error::Request(format!(
                        "score question '{key}' needs ≥2 levels"
                    )));
                }
                _ => {}
            }
        }
        if let State::Workspace { path, .. } = &state
            && !path.is_dir() {
                return Err(Error::Request(format!(
                    "workspace {} is not a directory",
                    path.display()
                )));
            }
        Ok(Request { state, questions })
    }

    /// Build from separate parts (CLI `--state*`/`-q` form).
    pub fn from_parts(
        state: State,
        questions_json: &str,
    ) -> Result<Request> {
        let questions: BTreeMap<String, Question> =
            serde_json::from_str(questions_json).map_err(|e| {
                Error::Request(format!("invalid questions JSON: {e}"))
            })?;
        Request::new(state, questions)
    }

    /// Canonical JSON for hashing (cache key, request_hash in the decisions
    /// log): state material + questions with sorted keys.
    pub fn canonical(&self) -> String {
        // serde_json preserves BTreeMap order — deterministic.
        serde_json::json!({
            "state": self.state.cache_material(),
            "questions": self.questions,
        })
        .to_string()
    }
}

/// Resolve a case's `questions` — inline `questions` wins, else load
/// `questions_file` relative to `base` (the cases file's directory).
/// Shared by `batch` and `eval` case loading.
pub fn case_questions(
    v: &serde_json::Value,
    cases_path: &std::path::Path,
    line: usize,
) -> Result<serde_json::Value> {
    match v.get("questions") {
        Some(q) if !q.is_null() => Ok(q.clone()),
        _ => {
            let Some(f) = v.get("questions_file").and_then(|x| x.as_str()) else {
                return Err(Error::Request(format!(
                    "{} line {line}: needs `questions` or `questions_file`",
                    cases_path.display()
                )));
            };
            let p = cases_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(f);
            serde_json::from_str(
                &std::fs::read_to_string(&p).map_err(|e| Error::io(&p, e))?,
            )
            .map_err(|e| Error::Request(format!("{}: bad JSON: {e}", p.display())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qs_json() -> &'static str {
        r#"{"dept": {"type":"choice","instructions":"i","criteria":{"a":"x","b":"y"}},
            "urg": {"type":"noul","instructions":"u"}}"#
    }

    #[test]
    fn parses_text_request() {
        let text = format!(
            "{{\"state\": \"hello world\", \"questions\": {}}}",
            qs_json()
        );
        let r = Request::from_json(&text).unwrap();
        assert!(matches!(r.state, State::Text(ref t) if t == "hello world"));
        assert_eq!(r.questions.len(), 2);
    }

    #[test]
    fn parses_workspace_request() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "{{\"state\": {{\"workspace\": \"{}\", \"hint\": \"h\"}}, \"questions\": {}}}",
            dir.path().display(),
            qs_json()
        );
        let r = Request::from_json(&text).unwrap();
        assert!(r.state.is_workspace());
    }

    #[test]
    fn rejects_bad_requests() {
        assert!(Request::from_json("{\"state\":\"x\",\"questions\":{}}").is_err());
        let one_choice = r#"{"c":{"type":"choice","instructions":"i","criteria":{"a":"x"}}}"#;
        assert!(Request::from_parts(State::Text("s".into()), one_choice).is_err());
        let bad_key = r#"{"9x":{"type":"noul","instructions":"i"}}"#;
        assert!(Request::from_parts(State::Text("s".into()), bad_key).is_err());
        let missing = format!(
            "{{\"state\": {{\"workspace\": \"/definitely/not/here\"}}, \"questions\": {}}}",
            qs_json()
        );
        assert!(Request::from_json(&missing).is_err());
    }

    #[test]
    fn canonical_is_deterministic() {
        let r1 = Request::from_parts(State::Text("s".into()), qs_json()).unwrap();
        let r2 = Request::from_parts(State::Text("s".into()), qs_json()).unwrap();
        assert_eq!(r1.canonical(), r2.canonical());
    }
}
