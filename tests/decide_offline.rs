//! Offline `decide` integration tests — mock backends, real SQLite store.

use std::collections::HashMap;
use std::sync::Arc;

use hungjury::backend::mock::MockBackend;
use hungjury::backend::{AgentBackend, BackendKind};
use hungjury::config::{Config, Escalate};
use hungjury::jury::{DecideCtx, decide};
use hungjury::memory::store::{Kind, Store};
use hungjury::request::Request;
use hungjury::response::{AnswerOut, DecidedBy};

/// A request with one choice + one noul question.
fn req() -> Request {
    Request::from_json(
        r#"{
            "state": "Customer reports the app crashes on checkout and demands a refund",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "which team", "criteria": {"billing": "money", "technical": "bugs"}},
                "refund": {"type": "noul", "id": "support.refund",
                           "instructions": "does the customer want a refund"}
            }
        }"#,
    )
    .unwrap()
}

/// Config with a temp data dir; `jurors`/`judge` are `mock:` strings.
fn cfg(dir: &tempfile::TempDir, jurors: &[&str], judge: &str) -> Config {
    Config {
        jurors: jurors.iter().map(|s| s.to_string()).collect(),
        judge: judge.to_string(),
        data_dir: dir.path().to_path_buf(),
        memory_db: dir.path().join("memory.db"),
        no_cache: false,
        ..Config::default()
    }
}

/// Ctx with `mock` wired to `backend`; optional second mock for distinct
/// juror behavior is not needed — the canned router splits by agent name.
fn ctx_with(config: Config, backend: MockBackend) -> DecideCtx {
    let mut map: HashMap<BackendKind, Arc<dyn AgentBackend>> = HashMap::new();
    map.insert(BackendKind::Mock, Arc::new(backend));
    DecideCtx::new(config, Some(map)).unwrap()
}

#[tokio::test]
async fn unanimous_jury_decides() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend::canned(&[
        ("juror:mock:a", r#"{"dept": "technical", "refund": true}"#),
        ("juror:mock:b", r#"{"dept": "technical", "refund": true}"#),
        ("juror:mock:c", r#"{"dept": "technical", "refund": true}"#),
    ]);
    let ctx = ctx_with(cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j"), backend);
    let (resp, code) = decide(&ctx, &req()).await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(resp.decided_by, DecidedBy::Jury);
    assert!(resp.hung.is_empty());
    let AnswerOut::Choice { choice, confidence, .. } = &resp.answers["dept"] else {
        panic!()
    };
    assert_eq!(choice, "technical");
    assert_eq!(*confidence, Some(1.0));
    let AnswerOut::Noul { noul, .. } = &resp.answers["refund"] else {
        panic!()
    };
    assert_eq!(*noul, 1.0);
    // Decision recorded + a second call hits the cache.
    let (resp2, _) = decide(&ctx, &req()).await.unwrap();
    assert_eq!(resp2.decided_by, DecidedBy::Cache);
}

#[tokio::test]
async fn failed_juror_is_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("juror:mock:bad") {
            return Err("cli exploded".to_string());
        }
        Ok(r#"{"dept": "billing", "refund": false}"#.to_string())
    });
    let ctx = ctx_with(
        cfg(&dir, &["mock:bad", "mock:g1", "mock:g2"], "mock:j"),
        backend,
    );
    let (resp, code) = decide(&ctx, &req()).await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(resp.usage.jurors.len(), 3);
    assert!(resp.usage.jurors.iter().any(|j| j.status != "ok"));
    let AnswerOut::Choice { choice, .. } = &resp.answers["dept"] else {
        panic!()
    };
    assert_eq!(choice, "billing");
}

#[tokio::test]
async fn hung_jury_escalate_off_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend::new(|req| {
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "technical", "refund": true}"#.to_string()
        } else {
            r#"{"dept": "billing", "refund": false}"#.to_string()
        })
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req()).await.unwrap();
    assert_eq!(code, 2);
    assert_eq!(resp.decided_by, DecidedBy::Jury);
    assert_eq!(resp.hung, vec!["dept".to_string(), "refund".to_string()]);
    assert!(resp.usage.judge.is_none());
}

#[tokio::test]
async fn hung_jury_sync_judge_resolves_and_writes_memory() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"dept": "technical", "refund": true},
                "rationale": {"dept": "crash = bug", "refund": "explicit demand"},
                "rulings": [{"question": "dept", "text": "crashes are technical issues"}],
                "facts": []
            }"#
            .to_string());
        }
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "technical", "refund": true}"#.to_string()
        } else {
            r#"{"dept": "billing", "refund": false}"#.to_string()
        })
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req()).await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(resp.decided_by, DecidedBy::Judge);
    assert!(resp.hung.is_empty());
    let judge = resp.usage.judge.as_ref().unwrap();
    assert_eq!(judge.status, "ok");
    assert!(!judge.wrote.is_empty());
    // Judge verdict rides inside the answer.
    let AnswerOut::Choice { judge: Some(jv), .. } = &resp.answers["dept"] else {
        panic!("expected judge verdict on dept")
    };
    assert_eq!(jv.choice.as_deref(), Some("technical"));
    // Memory: one ruling + two precedents (one per hung key).
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let rulings = store.list(Some(Kind::Ruling), None, true).unwrap();
    let precs = store.list(Some(Kind::Precedent), None, true).unwrap();
    assert_eq!(rulings.len(), 1);
    assert_eq!(precs.len(), 2);
    // juror_stats got ground-truthed for both questions.
    assert!(!store.juror_stats_rows().unwrap().is_empty());
}

#[tokio::test]
async fn queue_escalation_enqueues_hung() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend::new(|req| {
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "technical", "refund": true}"#.to_string()
        } else {
            r#"{"dept": "billing", "refund": false}"#.to_string()
        })
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Queue;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req()).await.unwrap();
    assert_eq!(code, 2);
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let pending = store.queue_pending().unwrap();
    assert_eq!(pending, vec![resp.id.clone()]);
}

/// Second `decide` with a fresh store sees rulings injected into juror
/// prompts (the "learned from the judge" path).
#[tokio::test]
async fn memory_reaches_juror_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"dept": "technical", "refund": true},
                "rationale": {"dept": "r", "refund": "r"},
                "rulings": [{"question": "dept", "text": "UNIQUE-RULING-MARKER"}],
                "facts": []
            }"#
            .to_string());
        }
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "technical", "refund": true}"#.to_string()
        } else {
            r#"{"dept": "billing", "refund": false}"#.to_string()
        })
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Sync;
    c.no_cache = true;
    let ctx = ctx_with(c, backend);
    let _ = decide(&ctx, &req()).await.unwrap();

    // Second decide, fresh ctx, same store: prompts must carry the ruling.
    let backend2 = MockBackend::new(|req| {
        if req.agent.starts_with("juror:") {
            assert!(
                req.prompt.contains("UNIQUE-RULING-MARKER"),
                "juror prompt lacked the ruling:\n{}",
                req.prompt
            );
        }
        Ok(r#"{"dept": "technical", "refund": true}"#.to_string())
    });
    let mut c2 = cfg(&dir, &["mock:x"], "mock:j");
    c2.escalate = Escalate::Off;
    c2.no_cache = true;
    let ctx2 = ctx_with(c2, backend2);
    let (resp, _) = decide(&ctx2, &req()).await.unwrap();
    assert!(resp.memory.rulings >= 1);
}
