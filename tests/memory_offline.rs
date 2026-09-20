//! Offline memory tests: store semantics, bundles, tombstones, contested,
//! workspace fact staleness, and the queue→learn→ruling path.

use std::collections::HashMap;
use std::sync::Arc;

use hungjury::backend::mock::MockBackend;
use hungjury::backend::{AgentBackend, BackendKind};
use hungjury::config::{Config, Escalate};
use hungjury::jury::{DecideCtx, decide};
use hungjury::learn;
use hungjury::memory::bundle;
use hungjury::memory::store::{Kind, NewEntry, Source, Status, Store, q_scope};
use hungjury::memory::workspace as ws;
use hungjury::request::Request;

fn cfg(dir: &tempfile::TempDir, jurors: &[&str], judge: &str) -> Config {
    Config {
        jurors: jurors.iter().map(|s| s.to_string()).collect(),
        judge: judge.to_string(),
        data_dir: dir.path().to_path_buf(),
        memory_db: dir.path().join("memory.db"),
        ..Config::default()
    }
}

fn ctx_with(config: Config, backend: MockBackend) -> DecideCtx {
    let mut map: HashMap<BackendKind, Arc<dyn AgentBackend>> = HashMap::new();
    map.insert(BackendKind::Mock, Arc::new(backend));
    DecideCtx::new(config, Some(map)).unwrap()
}

fn ruling(store: &Store, qid: &str, text: &str) -> String {
    let e = NewEntry {
        kind: Kind::Ruling,
        scope: q_scope(None, qid),
        body: serde_json::json!({"text": text}),
        text: text.to_string(),
        source: Source::Judge,
        trust: 0.8,
        author: Some("j".to_string()),
        origin: None,
    };
    store.insert(&e).unwrap().0
}

fn precedent(store: &Store, qid: &str, digest: &str, verdict: &str) -> String {
    let e = NewEntry {
        kind: Kind::Precedent,
        scope: q_scope(None, qid),
        body: serde_json::json!({
            "state_excerpt": format!("excerpt {digest}"),
            "state_digest": digest,
            "verdict": serde_json::json!(verdict),
            "rationale": "r",
        }),
        text: format!("excerpt {digest} rationale"),
        source: Source::Judge,
        trust: 0.8,
        author: Some("j".to_string()),
        origin: None,
    };
    store.insert(&e).unwrap().0
}

#[test]
fn forget_tombstones_and_blocks_resurrection() {
    let store = Store::open_memory().unwrap();
    let id = ruling(&store, "q1", "rule one");
    assert!(store.forget(&id).unwrap());
    // Re-inserting the same content must not resurrect.
    let e = NewEntry {
        kind: Kind::Ruling,
        scope: q_scope(None, "q1"),
        body: serde_json::json!({"text": "rule one"}),
        text: "rule one".to_string(),
        source: Source::Imported,
        trust: 0.4,
        author: None,
        origin: None,
    };
    let (id2, inserted) = store.insert(&e).unwrap();
    assert_eq!(id, id2);
    assert!(!inserted);
    assert!(store.rulings(None, "q1", 10).unwrap().is_empty());
}

#[test]
fn bundle_roundtrip_dedupes() {
    let a = Store::open_memory().unwrap();
    ruling(&a, "q1", "first rule");
    ruling(&a, "q2", "second rule");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.jsonl");

    let n = bundle::export(&a, &path, None, false).unwrap();
    assert_eq!(n, 2);

    let b = Store::open_memory().unwrap();
    let r1 = bundle::import(&b, &path, 1.0, false).unwrap();
    assert_eq!(r1.new, 2);
    // A→B→A re-import adds nothing.
    let path2 = dir.path().join("b2.jsonl");
    bundle::export(&b, &path2, None, false).unwrap();
    let r2 = bundle::import(&a, &path2, 1.0, false).unwrap();
    assert_eq!(r2.new, 0);
    assert_eq!(r2.duplicate, 2);
}

#[test]
fn default_export_excludes_precedents() {
    let store = Store::open_memory().unwrap();
    ruling(&store, "q1", "a rule");
    precedent(&store, "q1", "d1", "technical");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.jsonl");
    let n = bundle::export(&store, &path, None, false).unwrap();
    assert_eq!(n, 1); // only the ruling
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("state_excerpt"),
        "state excerpt leaked: {text}"
    );
    // --include-cases includes it.
    let path2 = dir.path().join("b2.jsonl");
    let n2 = bundle::export(&store, &path2, None, true).unwrap();
    assert_eq!(n2, 2);
}

#[test]
fn contested_import_marks_both_sides() {
    let local = Store::open_memory().unwrap();
    precedent(&local, "q1", "same-digest", "technical");

    let remote = Store::open_memory().unwrap();
    precedent(&remote, "q1", "same-digest", "billing");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("remote.jsonl");
    bundle::export(&remote, &path, None, true).unwrap();

    let r = bundle::import(&local, &path, 1.0, false).unwrap();
    assert_eq!(r.contested, 1);
    assert_eq!(r.new, 0);
    let all = local.list(Some(Kind::Precedent), None, false).unwrap();
    assert!(all.iter().all(|e| e.status == Status::Contested));
    // And the contested entry is queued for the judge.
    assert_eq!(local.queue_len().unwrap(), 1);
}

#[test]
fn trust_factor_scales_imports() {
    let remote = Store::open_memory().unwrap();
    ruling(&remote, "q1", "a rule"); // trust 0.8
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.jsonl");
    bundle::export(&remote, &path, None, false).unwrap();
    let local = Store::open_memory().unwrap();
    bundle::import(&local, &path, 0.5, false).unwrap();
    let rs = local.rulings(None, "q1", 10).unwrap();
    assert!((rs[0].trust - 0.4).abs() < 1e-9);
    assert_eq!(rs[0].source, Source::Imported);
}

#[test]
fn dry_run_import_writes_nothing() {
    let remote = Store::open_memory().unwrap();
    ruling(&remote, "q1", "a rule");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.jsonl");
    bundle::export(&remote, &path, None, false).unwrap();
    let local = Store::open_memory().unwrap();
    let r = bundle::import(&local, &path, 1.0, true).unwrap();
    assert_eq!(r.new, 1);
    assert!(local.rulings(None, "q1", 10).unwrap().is_empty());
}

/// Non-git dir → `sha256(path)` repo id; fact evidence hash drift → stale.
#[test]
fn workspace_fact_staleness() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("evidence.txt");
    std::fs::write(&f, "v1").unwrap();
    let repo = ws::repo_id(dir.path());
    let store = Store::open_memory().unwrap();
    let evidence = ws::evidence_for(dir.path(), std::slice::from_ref(&f));
    let e = NewEntry {
        kind: Kind::Fact,
        scope: hungjury::memory::store::ws_scope(None, &repo),
        body: serde_json::json!({"text": "evidence is v1", "evidence": evidence, "commit": ""}),
        text: "evidence is v1".to_string(),
        source: Source::Judge,
        trust: 0.8,
        author: None,
        origin: None,
    };
    let id = store.insert(&e).unwrap().0;
    // Fresh: fact verifies.
    let good = ws::verify_facts(&store, dir.path(), None, &repo).unwrap();
    assert_eq!(good.len(), 1);
    // Drift: mark stale + excluded.
    std::fs::write(&f, "v2 CHANGED").unwrap();
    let good = ws::verify_facts(&store, dir.path(), None, &repo).unwrap();
    assert!(good.is_empty());
    assert_eq!(store.get(&id).unwrap().unwrap().status, Status::Stale);
}

/// queue → `learn --queue` → judge resolves, precedent+ruling written,
/// queue drained, juror_stats updated.
#[tokio::test]
async fn learn_queue_judges_hung() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "app crashes on checkout",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "which team", "criteria": {"billing": "money", "technical": "bugs"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"dept": "technical"},
                "rationale": {"dept": "crash"},
                "rulings": [{"question": "dept", "text": "crashes → technical"}],
                "facts": []
            }"#
            .to_string());
        }
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "technical"}"#.to_string()
        } else {
            r#"{"dept": "billing"}"#.to_string()
        })
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Queue;
    let ctx = ctx_with(c, backend);
    let (_resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 2);

    learn::learn_queue(&ctx, false).await.unwrap();
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    assert_eq!(store.queue_len().unwrap(), 0);
    assert_eq!(
        store
            .all_rulings(&q_scope(None, "support.dept"))
            .unwrap()
            .len(),
        1
    );
    let precs = store.list(Some(Kind::Precedent), None, true).unwrap();
    assert_eq!(precs.len(), 1); // one per hung key
    // Both jurors got stat'd against the judge verdict.
    let rows = store.juror_stats_rows().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|(j, _, _, a)| j == "mock:a" && *a == 1));
    assert!(rows.iter().any(|(j, _, _, a)| j == "mock:b" && *a == 0));
}

/// Guard: rulings on a question the judge *overrode* a decided jury
/// majority land `contested`; rulings on genuinely hung questions stay
/// active (that's what escalation is for).
#[tokio::test]
async fn judge_override_of_decided_jury_marks_contested() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "charged twice, want a refund; also is this urgent?",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "which team", "criteria": {"billing": "m", "technical": "t"}},
                "urgent": {"type": "noul", "id": "support.urgent",
                           "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    // dept: all jurors say billing (decided). urgent: 1-2 split (hung).
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"dept": "technical", "urgent": true},
                "rationale": {"dept": "because", "urgent": "yes"},
                "rulings": [
                    {"question": "dept", "text": "always technical"},
                    {"question": "urgent", "text": "refund requests are urgent"}
                ],
                "facts": []
            }"#
            .to_string());
        }
        let dept = r#""billing""#;
        let urgent = if req.agent.contains("mock:a") {
            "true"
        } else {
            "false"
        };
        Ok(format!(r#"{{"dept": {dept}, "urgent": {urgent}}}"#))
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (_resp, _code) = decide(&ctx, &req).await.unwrap();

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let all = store.list(Some(Kind::Ruling), None, false).unwrap();
    assert_eq!(all.len(), 2);
    let dept_ruling = all
        .iter()
        .find(|e| e.text.contains("always technical"))
        .unwrap();
    let urg_ruling = all.iter().find(|e| e.text.contains("urgent")).unwrap();
    assert_eq!(dept_ruling.status, Status::Contested); // overrode decided jury
    assert_eq!(urg_ruling.status, Status::Active); // hung jury — escalation's job
    // Contested rulings are excluded from retrieval.
    assert!(store.rulings(None, "support.dept", 10).unwrap().is_empty());
    assert_eq!(store.rulings(None, "support.urgent", 10).unwrap().len(), 1);
}

/// Same path but judge agrees with the jury → ruling stays active.
#[tokio::test]
async fn judge_agreement_keeps_ruling_active() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "charged twice, want a refund; also is this urgent?",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "which team", "criteria": {"billing": "m", "technical": "t"}},
                "urgent": {"type": "noul", "id": "support.urgent",
                           "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"dept": "billing", "urgent": true},
                "rationale": {"dept": "money", "urgent": "yes"},
                "rulings": [{"question": "dept", "text": "duplicate charge → billing"}],
                "facts": []
            }"#
            .to_string());
        }
        let urgent = if req.agent.contains("mock:a") {
            "true"
        } else {
            "false"
        };
        Ok(format!(r#"{{"dept": "billing", "urgent": {urgent}}}"#))
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let _ = decide(&ctx, &req).await.unwrap();

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let act = store.rulings(None, "support.dept", 10).unwrap();
    assert_eq!(act.len(), 1);
    assert_eq!(act[0].status, Status::Active);
}

/// `feedback` writes a trust-1.0 precedent + updates juror_stats.
#[tokio::test]
async fn feedback_writes_human_precedent() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "anything",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "which team", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::canned(&[("juror:mock:a", r#"{"dept": "billing"}"#)]);
    let mut c = cfg(&dir, &["mock:a"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, _) = decide(&ctx, &req).await.unwrap();

    learn::feedback(
        &ctx,
        &resp.id,
        &[("dept".to_string(), "\"technical\"".to_string())],
        Some("human says tech"),
    )
    .unwrap();
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let precs = store.list(Some(Kind::Precedent), None, true).unwrap();
    assert_eq!(precs.len(), 1);
    assert!((precs[0].trust - 1.0).abs() < 1e-9);
    assert_eq!(precs[0].source, Source::Human);
    // mock:a voted billing but the human verdict is technical → disagree.
    let rows = store.juror_stats_rows().unwrap();
    assert_eq!(
        rows[0],
        ("mock:a".to_string(), "support.dept".to_string(), 1, 0)
    );
}

/// Juror weight flips the vote once stats cross n≥10.
#[tokio::test]
async fn juror_weight_changes_aggregation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    // mock:a disagreed with ground truth 0/12 → weight (0+1)/(12+2) ≈ 0.07.
    for _ in 0..12 {
        store
            .juror_stats_update("mock:a", "support.dept", false)
            .unwrap();
    }
    let w_a = store.juror_weight("mock:a", "support.dept").unwrap();
    assert!(w_a < 0.1);
    // mock:b has no stats → weight 1.0 outvotes mock:a despite 1v1.
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "i", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "technical"}"#.to_string()
        } else {
            r#"{"dept": "billing"}"#.to_string()
        })
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Off;
    c.no_cache = true;
    let ctx = ctx_with(c, backend);
    let (resp, _) = decide(&ctx, &req).await.unwrap();
    let hungjury::response::AnswerOut::Choice {
        choice,
        probabilities,
        ..
    } = &resp.answers["dept"]
    else {
        panic!()
    };
    assert_eq!(choice, "billing");
    assert!(probabilities["billing"] > 0.9);
}

/// Consolidation: >max_rulings rulings → judge rewrites ≤cap, old superseded.
#[tokio::test]
async fn consolidate_supersedes_old_rulings() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    for i in 0..10 {
        ruling(&store, "support.dept", &format!("rule {i}"));
    }
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{"rulings": ["merged rule A", "merged rule B"]}"#.to_string());
        }
        Ok("{}".to_string())
    });
    let mut c = cfg(&dir, &["mock:x"], "mock:j");
    c.memory.max_rulings = 4;
    let ctx = ctx_with(c, backend);
    learn::learn_consolidate(&ctx, false).await.unwrap();
    let active = store.all_rulings(&q_scope(None, "support.dept")).unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0].body["text"], "merged rule A");
    let all = store.list(Some(Kind::Ruling), None, false).unwrap();
    assert_eq!(
        all.iter()
            .filter(|e| e.status == Status::Superseded)
            .count(),
        10
    );
}

/// `feedback` on a decided answer: agreement leaves rulings active;
/// contradiction demotes judge-written rulings/precedents on the scope
/// to contested (human + imported entries untouched).
#[tokio::test]
async fn feedback_contradiction_demotes_judge_rulings() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "charged twice, want a refund",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "which team", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|_| Ok(r#"{"dept": "billing"}"#.to_string()));
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, _) = decide(&ctx, &req).await.unwrap();

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let jid = ruling(&store, "support.dept", "refund-ish → billing");
    // A human-authored ruling on the same scope must never be demoted.
    let he = NewEntry {
        kind: Kind::Ruling,
        scope: q_scope(None, "support.dept"),
        body: serde_json::json!({"text": "human rule"}),
        text: "human rule".to_string(),
        source: Source::Human,
        trust: 1.0,
        author: Some("h".to_string()),
        origin: None,
    };
    let hid = store.insert(&he).unwrap().0;

    // Agreement → nothing contested.
    let n = learn::feedback(
        &ctx,
        &resp.id,
        &[("dept".into(), "\"billing\"".into())],
        None,
    )
    .unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.get(&jid).unwrap().unwrap().status, Status::Active);

    // Contradiction → judge ruling contested, human ruling survives.
    let n = learn::feedback(
        &ctx,
        &resp.id,
        &[("dept".into(), "\"technical\"".into())],
        None,
    )
    .unwrap();
    assert_eq!(n, 1);
    assert_eq!(store.get(&jid).unwrap().unwrap().status, Status::Contested);
    assert_eq!(store.get(&hid).unwrap().unwrap().status, Status::Active);
}

/// Feedback on a hung (uncommitted) answer does not demote — the jury
/// never committed a verdict for lessons to have caused.
#[tokio::test]
async fn feedback_on_hung_answer_does_not_demote() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "urgent": {"type": "noul", "id": "support.urgent", "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        let v = req.agent.contains("mock:a");
        Ok(format!(r#"{{"urgent": {v}}}"#))
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 2); // 1-2 split → hung

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let jid = ruling(&store, "support.urgent", "refunds are urgent");
    let n = learn::feedback(&ctx, &resp.id, &[("urgent".into(), "true".into())], None).unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.get(&jid).unwrap().unwrap().status, Status::Active);
}

/// `resolve` transitions: contested → active (accept) → forgotten (reject).
#[test]
fn resolve_transitions() {
    let store = Store::open_memory().unwrap();
    let id = ruling(&store, "q1", "rule");
    store.set_status(&id, Status::Contested, None).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().status, Status::Contested);
    // --accept
    store.set_status(&id, Status::Active, None).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().status, Status::Active);
    // --reject
    assert!(store.forget(&id).unwrap());
    assert_eq!(store.get(&id).unwrap().unwrap().status, Status::Forgotten);
}

/// `list_decisions` returns newest first; `recent_jury_decisions` only
/// picks jury-decided rows.
#[test]
fn decisions_listing_and_recent() {
    let store = Store::open_memory().unwrap();
    for (id, by) in [("d1", "jury"), ("d2", "judge"), ("d3", "jury")] {
        store
            .record_decision(
                id,
                "h",
                &serde_json::json!({}),
                &serde_json::json!({"id": id}),
                by,
            )
            .unwrap();
    }
    let rows = store.list_decisions(10).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].id, "d3"); // newest first
    let recent = store.recent_jury_decisions(10).unwrap();
    assert_eq!(recent, vec!["d3".to_string(), "d1".to_string()]);
    assert_eq!(store.recent_jury_decisions(1).unwrap()[0], "d3");
}

/// `policy_block` renders a Domain policy section only when configured,
/// and `policy_file` resolves through Config::load (incl. missing file).
#[test]
fn policy_block_renders_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(&dir, &["mock:a"], "mock:j");
    let ctx = ctx_with(c.clone(), MockBackend::new(|_| Ok("{}".into())));
    assert_eq!(hungjury::jury::policy_block(&ctx), "");

    c.policy = Some("refunds always → billing".to_string());
    let ctx = ctx_with(c, MockBackend::new(|_| Ok("{}".into())));
    let block = hungjury::jury::policy_block(&ctx);
    assert!(block.contains("Domain policy") && block.contains("refunds always"));

    // --policy-file resolves through Config::load.
    let pf = dir.path().join("policy.md");
    std::fs::write(&pf, "  domain rules here\n").unwrap();
    let over = hungjury::config::CliOverrides {
        policy_file: Some(pf),
        ..Default::default()
    };
    let empty_toml = dir.path().join("empty.toml");
    std::fs::write(&empty_toml, "").unwrap();
    let loaded = Config::load(&over, Some(empty_toml.as_path())).unwrap();
    assert_eq!(loaded.policy.as_deref(), Some("domain rules here"));
}

/// Quorum: a single surviving juror must not decide — below
/// `min_quorum` ballots the key hangs (escalate=off ⇒ exit 2).
#[tokio::test]
async fn min_quorum_hangs_single_juror() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "i", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|_| Ok(r#"{"dept": "billing"}"#.to_string()));
    let mut c = cfg(&dir, &["mock:a"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 2);
    assert_eq!(resp.hung, vec!["dept".to_string()]);

    // min_quorum=1 restores the old single-ballot behaviour.
    let mut c = cfg(&dir, &["mock:a"], "mock:j");
    c.escalate = Escalate::Off;
    c.min_quorum = 1;
    c.no_cache = true;
    let ctx = ctx_with(
        c,
        MockBackend::new(|_| Ok(r#"{"dept": "billing"}"#.to_string())),
    );
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 0);
    assert!(resp.hung.is_empty());
}

/// Rulings distilled on escalated (undecided) keys are provisional:
/// `provisional_trust`, still active — and a judge answering a
/// quorum-failed key is never "overriding a majority".
#[tokio::test]
async fn escalation_rulings_are_provisional() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "urgent": {"type": "noul", "id": "support.urgent", "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    // One juror (votes=1 < quorum) → hung → sync escalate → judge answers.
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"urgent": false},
                "rationale": {"urgent": "no deadline"},
                "rulings": [{"question": "urgent", "text": "no deadline → not urgent"}],
                "facts": []
            }"#
            .to_string());
        }
        Ok(r#"{"urgent": true}"#.to_string())
    });
    let mut c = cfg(&dir, &["mock:a"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (resp, _) = decide(&ctx, &req).await.unwrap();
    assert_eq!(resp.decided_by, hungjury::response::DecidedBy::Judge);

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let rs = store.rulings(None, "support.urgent", 10).unwrap();
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].status, Status::Active); // provisional ≠ contested
    assert!((rs[0].trust - 0.4).abs() < 1e-9); // provisional, not 0.8
}

/// A ruling's `supersedes` id retires the older ruling it names.
#[tokio::test]
async fn judge_supersedes_retires_old_ruling() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let store = Store::open(&db).unwrap();
    let old = ruling(&store, "support.dept", "outdated: refunds → technical");
    let prefix = old[..8].to_string();
    drop(store);

    let req = Request::from_json(
        r#"{
            "state": "refund please",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "i", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    // 1-1 split → hung → judge runs and sees the old ruling's [id:] tag.
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(
        c,
        MockBackend::new(move |req| {
            if req.agent.starts_with("judge:") {
                return Ok(format!(
                    r#"{{"answers": {{"dept": "billing"}},
                        "rationale": {{"dept": "money"}},
                        "rulings": [{{"question": "dept", "text": "refunds → billing",
                                      "supersedes": "{prefix}"}}],
                        "facts": []}}"#
                ));
            }
            Ok(if req.agent.contains("mock:a") {
                r#"{"dept": "billing"}"#.to_string()
            } else {
                r#"{"dept": "technical"}"#.to_string()
            })
        }),
    );
    let (_resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 0);

    let store = Store::open(&db).unwrap();
    let old_e = store.get(&old).unwrap().unwrap();
    assert_eq!(old_e.status, Status::Superseded);
    assert!(old_e.superseded_by.is_some());
    let active = store.rulings(None, "support.dept", 10).unwrap();
    assert_eq!(active.len(), 1);
    assert!(active[0].text.contains("refunds"));
}

/// Human feedback matching the judge's verdict promotes provisional
/// rulings on that scope to full judge trust.
#[tokio::test]
async fn feedback_confirmation_promotes_provisional_rulings() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "urgent": {"type": "noul", "id": "support.urgent", "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"urgent": true},
                "rationale": {"urgent": "deadline"},
                "rulings": [{"question": "urgent", "text": "deadline → urgent"}],
                "facts": []
            }"#
            .to_string());
        }
        let v = req.agent.contains("mock:a");
        Ok(format!(r#"{{"urgent": {v}}}"#))
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (resp, _) = decide(&ctx, &req).await.unwrap();

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let r = store.rulings(None, "support.urgent", 10).unwrap();
    assert_eq!(r.len(), 1);
    assert!((r[0].trust - 0.4).abs() < 1e-9); // provisional

    // Human agrees with the judge verdict (urgent=true) → promote.
    learn::feedback(&ctx, &resp.id, &[("urgent".into(), "true".into())], None).unwrap();
    let r = store.rulings(None, "support.urgent", 10).unwrap();
    assert!((r[0].trust - 0.8).abs() < 1e-9);
}

/// `expire_rulings_before` stales old rulings; precedents untouched.
#[test]
fn ruling_ttl_expires_old_entries() {
    let store = Store::open_memory().unwrap();
    let rid = ruling(&store, "q1", "old rule");
    let pid = precedent(&store, "q1", "d1", "technical");
    assert_eq!(store.expire_rulings(0).unwrap(), 0); // disabled
    let n = store.expire_rulings_before("2999-01-01T00:00:00Z").unwrap();
    assert_eq!(n, 1);
    assert_eq!(store.get(&rid).unwrap().unwrap().status, Status::Stale);
    assert_eq!(store.get(&pid).unwrap().unwrap().status, Status::Active);
}

/// `promote_rulings` only lifts judge-sourced entries — imported and
/// human rulings keep their own trust.
#[test]
fn promote_leaves_non_judge_rulings_alone() {
    let store = Store::open_memory().unwrap();
    let provisional = {
        let e = NewEntry {
            kind: Kind::Ruling,
            scope: q_scope(None, "q1"),
            body: serde_json::json!({"text": "provisional lesson"}),
            text: "provisional lesson".to_string(),
            source: Source::Judge,
            trust: 0.4,
            author: None,
            origin: None,
        };
        store.insert(&e).unwrap().0
    };
    let imported = {
        let e = NewEntry {
            kind: Kind::Ruling,
            scope: q_scope(None, "q1"),
            body: serde_json::json!({"text": "imported lesson"}),
            text: "imported lesson".to_string(),
            source: Source::Imported,
            trust: 0.4,
            author: None,
            origin: None,
        };
        store.insert(&e).unwrap().0
    };
    assert_eq!(store.promote_rulings(&q_scope(None, "q1"), 0.8).unwrap(), 1);
    assert!((store.get(&provisional).unwrap().unwrap().trust - 0.8).abs() < 1e-9);
    assert!((store.get(&imported).unwrap().unwrap().trust - 0.4).abs() < 1e-9);
}

/// `id_by_prefix` resolves a unique 8-char tag; ambiguous → None.
#[test]
fn id_prefix_resolution() {
    let store = Store::open_memory().unwrap();
    let id = ruling(&store, "q1", "some rule");
    assert_eq!(store.id_by_prefix(&id[..8]).unwrap(), Some(id));
    assert_eq!(store.id_by_prefix("zzzzzzzz").unwrap(), None);
    assert_eq!(store.id_by_prefix("").unwrap(), None); // 0 or >1 rows
}

/// `batch` decides a JSONL file in parallel and echoes `case` labels.
#[tokio::test]
async fn batch_decides_and_echoes_case_labels() {
    let dir = tempfile::tempdir().unwrap();
    let cases = dir.path().join("cases.jsonl");
    std::fs::write(
        &cases,
        concat!(
            r#"{"case": "a", "state": "s1", "questions": {"q": {"type": "choice", "id": "k.q", "instructions": "i", "criteria": {"x": "1", "y": "2"}}}}"#,
            "\n",
            r#"{"case": "b", "state": "s2", "questions": {"q": {"type": "choice", "id": "k.q", "instructions": "i", "criteria": {"x": "1", "y": "2"}}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let out = dir.path().join("out.jsonl");
    let backend = MockBackend::new(|_| Ok(r#"{"q": "x"}"#.to_string()));
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let code = hungjury::batch::run(&ctx, &cases, Some(&out))
        .await
        .unwrap();
    assert_eq!(code, 0);
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["case"], "a");
    assert_eq!(lines[1]["case"], "b");
    assert_eq!(lines[0]["decided_by"], "jury");
}

/// `questions_file` resolves relative to the cases file's directory.
#[tokio::test]
async fn batch_loads_shared_questions_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("shared.json"),
        r#"{"q": {"type": "choice", "id": "k.q", "instructions": "i", "criteria": {"x": "1", "y": "2"}}}"#,
    )
    .unwrap();
    let cases = dir.path().join("cases.jsonl");
    std::fs::write(
        &cases,
        r#"{"state": "s1", "questions_file": "shared.json"}"#.to_string() + "\n",
    )
    .unwrap();
    let out = dir.path().join("out.jsonl");
    let backend = MockBackend::new(|_| Ok(r#"{"q": "y"}"#.to_string()));
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let code = hungjury::batch::run(&ctx, &cases, Some(&out))
        .await
        .unwrap();
    assert_eq!(code, 0);
    let line: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&out).unwrap().trim()).unwrap();
    assert_eq!(line["answers"]["q"]["choice"], "y");
}

/// Every juror abstaining on a question = zero ballots → quorum-fail →
/// hung (not a unanimous "decision").
#[tokio::test]
async fn unanimous_abstain_hangs() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "hello?? anyone there",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "i", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|_| Ok(r#"{"dept": "abstain"}"#.to_string()));
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 2);
    assert_eq!(resp.hung, vec!["dept".to_string()]);
    // Hung keys have no decider attribution.
    assert!(!resp.sources.contains_key("dept"));
}

/// One abstention + two agreeing ballots → quorum met, jury decides.
#[tokio::test]
async fn abstain_does_not_block_quorum() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "refund please",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "i", "criteria": {"billing": "m", "technical": "t"}}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "abstain"}"#
        } else {
            r#"{"dept": "billing"}"#
        }
        .to_string())
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Off;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 0);
    assert!(resp.hung.is_empty());
    assert_eq!(
        resp.sources.get("dept"),
        Some(&hungjury::response::DecidedBy::Jury)
    );
}

/// A judge abstention leaves the key hung instead of silently clearing
/// it — `escalated` records what was sent up, `sources` marks judge keys.
#[tokio::test]
async fn judge_abstain_keeps_key_hung() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "urgent": {"type": "noul", "id": "support.urgent", "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"urgent": "abstain"},
                "rationale": {},
                "rulings": [], "facts": []
            }"#
            .to_string());
        }
        Ok(r#"{"urgent": true}"#.to_string())
    });
    let mut c = cfg(&dir, &["mock:a"], "mock:j"); // 1 juror < quorum → hung
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 2);
    assert_eq!(resp.hung, vec!["urgent".to_string()]);
    assert_eq!(resp.escalated, vec!["urgent".to_string()]);
    assert_eq!(resp.decided_by, hungjury::response::DecidedBy::Jury);
}

/// When the judge resolves an escalated key, `sources` attributes that
/// key to the judge while jury-decided keys stay `jury`.
#[tokio::test]
async fn sources_attribute_judge_keys() {
    let dir = tempfile::tempdir().unwrap();
    let req = Request::from_json(
        r#"{
            "state": "x",
            "questions": {
                "dept": {"type": "choice", "id": "support.dept",
                         "instructions": "i", "criteria": {"billing": "m", "technical": "t"}},
                "urgent": {"type": "noul", "id": "support.urgent", "instructions": "time-sensitive"}
            }
        }"#,
    )
    .unwrap();
    let backend = MockBackend::new(|req| {
        if req.agent.starts_with("judge:") {
            return Ok(r#"{
                "answers": {"dept": "billing", "urgent": false},
                "rationale": {}, "rulings": [], "facts": []
            }"#
            .to_string());
        }
        // dept splits 1-1 → hung; urgent unanimous → jury.
        Ok(if req.agent.contains("mock:a") {
            r#"{"dept": "billing", "urgent": false}"#
        } else {
            r#"{"dept": "technical", "urgent": false}"#
        }
        .to_string())
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (resp, code) = decide(&ctx, &req).await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(resp.decided_by, hungjury::response::DecidedBy::Judge);
    assert_eq!(resp.escalated, vec!["dept".to_string()]);
    assert_eq!(
        resp.sources.get("dept"),
        Some(&hungjury::response::DecidedBy::Judge)
    );
    assert_eq!(
        resp.sources.get("urgent"),
        Some(&hungjury::response::DecidedBy::Jury)
    );
}
