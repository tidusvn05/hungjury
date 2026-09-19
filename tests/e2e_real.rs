//! Real-CLI e2e tests — ignored by default (they spend subscription quota).
//!
//! Run all:     `cargo test --test e2e_real -- --ignored`
//! One backend: `cargo test --test e2e_real claude -- --ignored`
//!
//! Each backend is exercised under both tool policies:
//! - `ToolPolicy::None`      — a text-state juror ballot.
//! - `ToolPolicy::ReadOnly`  — a workspace-state juror ballot.

use std::collections::BTreeMap;
use std::time::Duration;

use hungjury::backend::{
    AgentRequest, AgentResult, BackendKind, ToolPolicy, extract_json, for_kind,
};
use hungjury::question::{Question, validate_ballot};
use hungjury::sys::find_on_path;

fn questions() -> BTreeMap<String, Question> {
    serde_json::from_value(serde_json::json!({
        "dept": {"type": "choice", "instructions": "which team handles this",
                 "criteria": {"billing": "money issues", "technical": "bugs"}},
        "refund": {"type": "noul", "instructions": "does the customer want a refund"}
    }))
    .unwrap()
}

/// Run one request through the real CLI and validate the ballot.
async fn ballot(
    kind: BackendKind,
    model: &str,
    tools: ToolPolicy,
    prompt: String,
    cwd: std::path::PathBuf,
) -> AgentResult {
    let backend = for_kind(kind);
    let (k, m) = BackendKind::parse(model).unwrap();
    assert_eq!(k, kind);
    let qs = questions();
    let r = backend
        .run(AgentRequest {
            prompt,
            system_prompt: Some("Output ONLY a single JSON object — no prose.".to_string()),
            model: m,
            cwd,
            tools,
            timeout: Duration::from_secs(240),
            agent: format!("e2e:{model}"),
            json_schema: None,
        })
        .await
        .unwrap_or_else(|e| panic!("{model} backend failed: {e}"));
    let v = extract_json(&r.text, "e2e").expect("ballot JSON");
    let (ballots, _) = validate_ballot(&qs, &v, false).expect("valid ballot");
    assert!(ballots.contains_key("dept"));
    assert!(ballots.contains_key("refund"));
    r
}

async fn text_case(kind: BackendKind, model: &str) {
    if find_on_path(kind.as_str()).is_none() {
        eprintln!("skipped: {} not on PATH", kind.as_str());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let r = ballot(
        kind,
        model,
        ToolPolicy::None,
        "You are a juror. Answer the two questions about the state below.\n\
         Output ONLY a JSON object {\"dept\": \"billing\"|\"technical\", \
         \"refund\": true|false} — no prose, no fences.\n\n\
         STATE: Customer reports the app crashes on checkout and demands a refund"
            .to_string(),
        dir.path().to_path_buf(),
    )
    .await;
    eprintln!("{model} text ok in {:?}", r.duration);
}

async fn workspace_case(kind: BackendKind, model: &str) {
    if find_on_path(kind.as_str()).is_none() {
        eprintln!("skipped: {} not on PATH", kind.as_str());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("README.md"),
        "# shop\nCheckout library. Refund policy: no refunds.",
    )
    .unwrap();
    let r = ballot(
        kind,
        model,
        ToolPolicy::ReadOnly,
        "You are a juror deciding about this repository. Read whatever files you \
         need (READ-ONLY — do not modify anything), then answer:\n\
         Output ONLY a JSON object {\"dept\": \"billing\"|\"technical\", \
         \"refund\": true|false} where dept = which team maintains the code and \
         refund = whether the README mentions refunds."
            .to_string(),
        dir.path().to_path_buf(),
    )
    .await;
    eprintln!("{model} workspace ok in {:?}", r.duration);
}

#[tokio::test]
#[ignore = "spends real CLI quota"]
async fn claude_text() {
    text_case(BackendKind::Claude, "claude:haiku").await;
}

#[tokio::test]
#[ignore = "spends real CLI quota"]
async fn claude_workspace() {
    workspace_case(BackendKind::Claude, "claude:haiku").await;
}

#[tokio::test]
#[ignore = "spends real CLI quota"]
async fn codex_text() {
    text_case(BackendKind::Codex, "codex:gpt-5.6-terra@low").await;
}

#[tokio::test]
#[ignore = "spends real CLI quota"]
async fn codex_workspace() {
    workspace_case(BackendKind::Codex, "codex:gpt-5.6-terra@low").await;
}

#[tokio::test]
#[ignore = "spends real CLI quota"]
async fn devin_text() {
    text_case(BackendKind::Devin, "devin:swe-2-medium").await;
}

#[tokio::test]
#[ignore = "spends real CLI quota"]
async fn devin_workspace() {
    workspace_case(BackendKind::Devin, "devin:swe-2-medium").await;
}
