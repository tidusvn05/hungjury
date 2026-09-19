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
        scope: q_scope(qid),
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
        scope: q_scope(qid),
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
        scope: q_scope("q1"),
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
    assert!(store.rulings("q1", 10).unwrap().is_empty());
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
    assert!(!text.contains("state_excerpt"), "state excerpt leaked: {text}");
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
    let rs = local.rulings("q1", 10).unwrap();
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
    assert!(local.rulings("q1", 10).unwrap().is_empty());
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
        scope: hungjury::memory::store::ws_scope(&repo),
        body: serde_json::json!({"text": "evidence is v1", "evidence": evidence, "commit": ""}),
        text: "evidence is v1".to_string(),
        source: Source::Judge,
        trust: 0.8,
        author: None,
        origin: None,
    };
    let id = store.insert(&e).unwrap().0;
    // Fresh: fact verifies.
    let good = ws::verify_facts(&store, dir.path(), &repo).unwrap();
    assert_eq!(good.len(), 1);
    // Drift: mark stale + excluded.
    std::fs::write(&f, "v2 CHANGED").unwrap();
    let good = ws::verify_facts(&store, dir.path(), &repo).unwrap();
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
    assert_eq!(store.all_rulings("support.dept").unwrap().len(), 1);
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
        let urgent = if req.agent.contains("mock:a") { "true" } else { "false" };
        Ok(format!(r#"{{"dept": {dept}, "urgent": {urgent}}}"#))
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let (_resp, _code) = decide(&ctx, &req).await.unwrap();

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let all = store.list(Some(Kind::Ruling), None, false).unwrap();
    assert_eq!(all.len(), 2);
    let dept_ruling = all.iter().find(|e| e.text.contains("always technical")).unwrap();
    let urg_ruling = all.iter().find(|e| e.text.contains("urgent")).unwrap();
    assert_eq!(dept_ruling.status, Status::Contested); // overrode decided jury
    assert_eq!(urg_ruling.status, Status::Active); // hung jury — escalation's job
    // Contested rulings are excluded from retrieval.
    assert!(store.rulings("support.dept", 10).unwrap().is_empty());
    assert_eq!(store.rulings("support.urgent", 10).unwrap().len(), 1);
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
        let urgent = if req.agent.contains("mock:a") { "true" } else { "false" };
        Ok(format!(r#"{{"dept": "billing", "urgent": {urgent}}}"#))
    });
    let mut c = cfg(&dir, &["mock:a", "mock:b", "mock:c"], "mock:j");
    c.escalate = Escalate::Sync;
    let ctx = ctx_with(c, backend);
    let _ = decide(&ctx, &req).await.unwrap();

    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let act = store.rulings("support.dept", 10).unwrap();
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

    learn::feedback(&ctx, &resp.id, &[("dept".to_string(), "\"technical\"".to_string())], Some("human says tech"))
        .unwrap();
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    let precs = store.list(Some(Kind::Precedent), None, true).unwrap();
    assert_eq!(precs.len(), 1);
    assert!((precs[0].trust - 1.0).abs() < 1e-9);
    assert_eq!(precs[0].source, Source::Human);
    // mock:a voted billing but the human verdict is technical → disagree.
    let rows = store.juror_stats_rows().unwrap();
    assert_eq!(rows[0], ("mock:a".to_string(), "support.dept".to_string(), 1, 0));
}

/// Juror weight flips the vote once stats cross n≥10.
#[tokio::test]
async fn juror_weight_changes_aggregation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("memory.db")).unwrap();
    // mock:a disagreed with ground truth 0/12 → weight (0+1)/(12+2) ≈ 0.07.
    for _ in 0..12 {
        store.juror_stats_update("mock:a", "support.dept", false).unwrap();
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
    let hungjury::response::AnswerOut::Choice { choice, probabilities, .. } = &resp.answers["dept"] else {
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
    let active = store.all_rulings("support.dept").unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0].body["text"], "merged rule A");
    let all = store.list(Some(Kind::Ruling), None, false).unwrap();
    assert_eq!(all.iter().filter(|e| e.status == Status::Superseded).count(), 10);
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
        scope: q_scope("support.dept"),
        body: serde_json::json!({"text": "human rule"}),
        text: "human rule".to_string(),
        source: Source::Human,
        trust: 1.0,
        author: Some("h".to_string()),
        origin: None,
    };
    let hid = store.insert(&he).unwrap().0;

    // Agreement → nothing contested.
    let n = learn::feedback(&ctx, &resp.id, &[("dept".into(), "\"billing\"".into())], None).unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.get(&jid).unwrap().unwrap().status, Status::Active);

    // Contradiction → judge ruling contested, human ruling survives.
    let n = learn::feedback(&ctx, &resp.id, &[("dept".into(), "\"technical\"".into())], None).unwrap();
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
            .record_decision(id, "h", &serde_json::json!({}), &serde_json::json!({"id": id}), by)
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
