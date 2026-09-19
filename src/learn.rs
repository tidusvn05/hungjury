//! Offline learning: `learn --queue` (judge hung decisions), `--audit N`
//! (judge re-scores jury decisions), `--consolidate` (merge rulings), and
//! `feedback` (human correction → trust-1.0 precedent).
//!
//! Judge answers are ground truth everywhere: `juror_stats` is updated by
//! [`crate::judge::commit_judge`] on every re-judged case.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::judge::{self, JudgeCall};
use crate::jury::DecideCtx;
use crate::memory::store::{Kind, NewEntry, Source, Store};
use crate::question::{Ballot, Question};
use crate::request::{Request, State};
use crate::response::{AnswerOut, Response};

/// Rebuild a [`Request`] from a stored decision's request JSON.
fn request_from_stored(v: &serde_json::Value) -> Result<Request> {
    Request::from_json(&v.to_string())
}

/// Rebuild `(juror_name → ballots)` from a stored response's usage block.
fn juror_ballots_from_stored(
    resp: &Response,
    questions: &BTreeMap<String, Question>,
) -> Vec<(String, BTreeMap<String, Ballot>)> {
    let mut out = Vec::new();
    for j in &resp.usage.jurors {
        if j.status != "ok" {
            continue;
        }
        let Some(answers) = &j.answers else {
            continue;
        };
        let mut ballots = BTreeMap::new();
        for (key, val) in answers {
            if let Some(q) = questions.get(key)
                && let Ok(b) = q.validate_answer(key, val)
            {
                ballots.insert(key.clone(), b);
            }
        }
        if !ballots.is_empty() {
            out.push((j.juror.clone(), ballots));
        }
    }
    out
}

/// Where the judge's verdict differs from the jury's committed answer —
/// these keys earn precedents.
fn disagreed_keys(call: &JudgeCall, resp: &Response) -> Vec<String> {
    let mut keys = Vec::new();
    for (key, j) in &call.judged {
        match (resp.answers.get(key), &j.ballot) {
            (Some(AnswerOut::Choice { choice, .. }), Ballot::Choice(c)) if c != choice => {
                keys.push(key.clone())
            }
            (Some(AnswerOut::Score { score, .. }), Ballot::Score(s))
                if *s as f64 != score.round() =>
            {
                keys.push(key.clone())
            }
            (Some(AnswerOut::Noul { noul, .. }), Ballot::Noul(b))
                if (*noul >= 0.5) != *b =>
            {
                keys.push(key.clone())
            }
            (None, _) => keys.push(key.clone()),
            _ => {}
        }
    }
    keys
}

/// Re-judge one stored decision — `judge_call` only; the caller commits
/// once with the precedent keys it wants.
async fn rejudge(
    ctx: &DecideCtx,
    store: &Store,
    req: &Request,
    resp: &Response,
    judge_keys: &[String],
) -> Option<(JudgeCall, Vec<(String, BTreeMap<String, Ballot>)>, Option<std::path::PathBuf>, Option<String>)> {
    let juror_ballots = juror_ballots_from_stored(resp, &req.questions);
    let ws_path = match &req.state {
        State::Workspace { path, .. } => Some(path.clone()),
        State::Text(_) => None,
    };
    let repo_id = ws_path.as_deref().map(crate::memory::workspace::repo_id);
    // Fresh retrieval: later cases see what earlier judged cases taught.
    let memory_block = crate::memory::retrieve::retrieve(store, req, &ctx.config.memory, None)
        .map(|r| r.block)
        .unwrap_or_default();
    let (call, usage) = judge::judge_call(
        ctx,
        req,
        &memory_block,
        &juror_ballots,
        &resp.answers,
        judge_keys,
        &crate::util::nonce(),
        ws_path.as_deref(),
    )
    .await;
    if usage.status != "ok" {
        eprintln!("judge failed for {}: {}", resp.id, usage.error.unwrap_or_default());
    }
    call.map(|c| (c, juror_ballots, ws_path, repo_id))
}

/// `learn --queue`: every pending decision gets judged; its originally
/// hung keys earn precedents; the item is marked done.
pub async fn learn_queue(ctx: &DecideCtx, dry_run: bool) -> Result<()> {
    let Some(store) = &ctx.store else {
        return Err(Error::Memory("memory db unavailable".to_string()));
    };
    let pending = store.queue_pending()?;
    eprintln!("queue: {} pending", pending.len());
    for id in &pending {
        let Some((req_json, resp_json, _)) = store.get_decision(id)? else {
            eprintln!("  {id}: decision missing — marking done");
            let _ = store.queue_done(id);
            continue;
        };
        let req = request_from_stored(&req_json)?;
        let resp: Response = serde_json::from_value(resp_json)
            .map_err(|e| Error::Memory(format!("decision {id} response: {e}")))?;
        if dry_run {
            eprintln!("  {id}: would judge (hung: {})", resp.hung.join(", "));
            continue;
        }
        match rejudge(ctx, store, &req, &resp, &resp.hung).await {
            Some((call, juror_ballots, ws_path, repo_id)) => {
                let wrote = judge::commit_judge(
                    store,
                    false,
                    &req,
                    &call,
                    &resp.hung,
                    &juror_ballots,
                    &resp.answers,
                    ctx.config.hung_threshold,
                    repo_id.as_deref(),
                    ws_path.as_deref(),
                    &ctx.config.judge,
                )
                .unwrap_or_default();
                eprintln!("  {id}: judged, wrote {}", wrote.len());
                let _ = store.queue_done(id);
            }
            None => eprintln!("  {id}: judge failed — left in queue"),
        }
    }
    Ok(())
}

/// `learn --audit N`: N random jury decisions re-judged; disagreements
/// earn precedents (+ rulings the judge distills).
pub async fn learn_audit(ctx: &DecideCtx, n: usize, dry_run: bool) -> Result<()> {
    let Some(store) = &ctx.store else {
        return Err(Error::Memory("memory db unavailable".to_string()));
    };
    let ids = store.sample_jury_decisions(n)?;
    eprintln!("audit: {} decisions sampled", ids.len());
    for id in &ids {
        let Some((req_json, resp_json, _)) = store.get_decision(id)? else {
            continue;
        };
        let req = request_from_stored(&req_json)?;
        let resp: Response = serde_json::from_value(resp_json)
            .map_err(|e| Error::Memory(format!("decision {id} response: {e}")))?;
        if dry_run {
            eprintln!("  {id}: would re-judge");
            continue;
        }
        // Audit re-judges every question — including ones the jury had
        // decided — so judge/jury conflicts can actually surface.
        let all_keys: Vec<String> = req.questions.keys().cloned().collect();
        match rejudge(ctx, store, &req, &resp, &all_keys).await {
            Some((call, juror_ballots, ws_path, repo_id)) => {
                let keys = disagreed_keys(&call, &resp);
                let wrote = judge::commit_judge(
                    store,
                    false,
                    &req,
                    &call,
                    &keys,
                    &juror_ballots,
                    &resp.answers,
                    ctx.config.hung_threshold,
                    repo_id.as_deref(),
                    ws_path.as_deref(),
                    &ctx.config.judge,
                )
                .unwrap_or_default();
                // Audit demote: active judge rulings on scopes where the judge
                // just contradicted a decided jury majority taught the wrong
                // lesson — move them to contested.
                for key in
                    judge::conflict_keys(&req, &call, &resp.answers, ctx.config.hung_threshold)
                {
                    if let Some(q) = req.questions.get(&key) {
                        for e in store.rulings(&q.qid(), 100).unwrap_or_default() {
                            if e.source == crate::memory::store::Source::Judge {
                                let _ = store.set_status(
                                    &e.id,
                                    crate::memory::store::Status::Contested,
                                    None,
                                );
                            }
                        }
                    }
                }
                if keys.is_empty() {
                    eprintln!("  {id}: judge agrees with jury");
                } else {
                    eprintln!("  {id}: disagreed on {}, wrote {}", keys.join(","), wrote.len());
                }
            }
            None => eprintln!("  {id}: judge failed"),
        }
    }
    Ok(())
}

/// `learn --consolidate`: qids over the rulings/precedent caps get a
/// `consolidate.md` judge call; the rewritten rulings supersede the old.
pub async fn learn_consolidate(ctx: &DecideCtx, dry_run: bool) -> Result<()> {
    let Some(store) = &ctx.store else {
        return Err(Error::Memory("memory db unavailable".to_string()));
    };
    // Scopes are per-qid: enumerate active rulings grouped by scope.
    let all = store.list(Some(Kind::Ruling), None, true)?;
    let mut by_scope: BTreeMap<String, Vec<crate::memory::store::Entry>> = BTreeMap::new();
    for e in all {
        by_scope.entry(e.scope.clone()).or_default().push(e);
    }
    let mut qids: BTreeMap<String, ()> = BTreeMap::new();
    for (scope, rulings) in &by_scope {
        if rulings.len() > ctx.config.memory.max_rulings {
            qids.insert(scope.clone(), ());
        }
    }
    // Precedent cap: >30 active precedents also triggers consolidation.
    let precs = store.list(Some(Kind::Precedent), None, true)?;
    let mut prec_by_scope: BTreeMap<String, usize> = BTreeMap::new();
    for e in precs {
        *prec_by_scope.entry(e.scope.clone()).or_default() += 1;
    }
    for (scope, n) in &prec_by_scope {
        if *n > 30 {
            qids.insert(scope.clone(), ());
        }
    }
    if qids.is_empty() {
        eprintln!("consolidate: nothing over the caps");
        return Ok(());
    }
    for scope in qids.keys() {
        let qid = scope.trim_start_matches("q:");
        let rulings = store.all_rulings(qid)?;
        if dry_run {
            eprintln!("  {scope}: {} rulings → consolidate", rulings.len());
            continue;
        }
        consolidate_scope(ctx, store, qid, rulings).await?;
    }
    Ok(())
}

/// One consolidate call for a scope.
async fn consolidate_scope(
    ctx: &DecideCtx,
    store: &Store,
    qid: &str,
    rulings: Vec<crate::memory::store::Entry>,
) -> Result<()> {
    let (kind, model) = crate::backend::BackendKind::parse(&ctx.config.judge)?;
    let backend = ctx.backend(kind);
    let rulings_block = rulings
        .iter()
        .map(|r| {
            format!(
                "- (trust {:.1}) {}",
                r.trust,
                r.body["text"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "rulings": {
                "type": "array",
                "items": {"type": "string"},
            }
        },
        "required": ["rulings"],
        "additionalProperties": false
    });
    let tmpl = ctx.prompts.load("consolidate.md")?;
    let mut vars: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    vars.insert("max_rulings", ctx.config.memory.max_rulings.to_string());
    vars.insert("question_key", qid.to_string());
    vars.insert("question_desc", question_desc(rulings.first()));
    vars.insert("rulings_block", rulings_block);
    vars.insert("schema_block", crate::jury::schema_block_text(&schema));
    let prompt = crate::prompt::render(&tmpl, &vars);

    ctx.quota.consume().await?;
    let req = crate::backend::AgentRequest {
        prompt,
        system_prompt: Some(
            "You maintain interpretation rules for a decision system. \
             Output ONLY a single JSON object.".to_string(),
        ),
        model,
        cwd: ctx.empty_cwd.path().to_path_buf(),
        tools: crate::backend::ToolPolicy::None,
        timeout: ctx.config.judge_timeout(),
        agent: format!("judge:consolidate:{qid}"),
        json_schema: Some(schema),
    };
    let out = backend.run(req).await?;
    let v = crate::backend::extract_json(&out.text, "judge:consolidate")?;
    let Some(new_rulings) = v["rulings"].as_array() else {
        return Err(Error::Validation {
            agent: "judge:consolidate".to_string(),
            message: "missing rulings array".to_string(),
        });
    };
    let origin = store.machine_id().ok();
    let mut new_ids = Vec::new();
    for r in new_rulings.iter().take(ctx.config.memory.max_rulings) {
        let Some(text) = r.as_str().map(str::trim).filter(|t| !t.is_empty() && t.len() <= 300)
        else {
            continue;
        };
        let e = NewEntry {
            kind: Kind::Ruling,
            scope: crate::memory::store::q_scope(qid),
            body: serde_json::json!({
                "text": text,
                "question": rulings.first()
                    .map(|x| &x.body["question"])
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            }),
            text: text.to_string(),
            source: Source::Judge,
            trust: Source::Judge.base_trust(),
            author: Some(ctx.config.judge.clone()),
            origin: origin.clone(),
        };
        if let Ok((id, true)) = store.insert(&e) {
            new_ids.push(id);
        }
    }
    // Supersede the old rulings (round-robin pointer into the new set).
    for (i, old) in rulings.iter().enumerate() {
        let by = new_ids.get(i % new_ids.len().max(1)).map(|s| s.as_str());
        let _ = store.set_status(&old.id, crate::memory::store::Status::Superseded, by);
    }
    eprintln!("  q:{qid}: {} → {} rulings", rulings.len(), new_ids.len());
    Ok(())
}

fn question_desc(first: Option<&crate::memory::store::Entry>) -> String {
    first
        .and_then(|e| e.body["question"]["instructions"].as_str())
        .unwrap_or("")
        .to_string()
}

/// `feedback`: a human correction becomes a trust-1.0 precedent and
/// updates `juror_stats`. `sets` are `key=value` pairs; `value` is parsed
/// as the answer JSON (`"technical"`, `1`, `true`).
pub fn feedback(
    ctx: &DecideCtx,
    decision_id: &str,
    sets: &[(String, String)],
    note: Option<&str>,
) -> Result<()> {
    let Some(store) = &ctx.store else {
        return Err(Error::Memory("memory db unavailable".to_string()));
    };
    let Some((req_json, resp_json, _)) = store.get_decision(decision_id)? else {
        return Err(Error::Memory(format!("no decision '{decision_id}'")));
    };
    let req = request_from_stored(&req_json)?;
    let resp: Response = serde_json::from_value(resp_json)
        .map_err(|e| Error::Memory(format!("decision response: {e}")))?;

    let (excerpt, digest) = match &req.state {
        State::Text(t) => (
            t.chars().take(400).collect::<String>(),
            crate::util::sha256_str(t),
        ),
        State::Workspace { path, hint } => (
            format!(
                "workspace {} {}",
                path.display(),
                hint.as_deref().unwrap_or("")
            )
            .chars()
            .take(400)
            .collect::<String>(),
            crate::util::sha256_str(&path.display().to_string()),
        ),
    };
    let origin = store.machine_id().ok();
    for (key, raw) in sets {
        let Some(q) = req.questions.get(key) else {
            return Err(Error::Request(format!("unknown question key '{key}'")));
        };
        let val: serde_json::Value = serde_json::from_str(raw)
            .unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
        let ballot = q.validate_answer(key, &val)?;
        let e = NewEntry {
            kind: Kind::Precedent,
            scope: crate::memory::store::q_scope(&q.qid()),
            body: serde_json::json!({
                "state_excerpt": excerpt,
                "state_digest": digest,
                "verdict": ballot.to_json(),
                "rationale": note.unwrap_or("human feedback"),
            }),
            text: format!("{excerpt} {}", note.unwrap_or("")),
            source: Source::Human,
            trust: Source::Human.base_trust(),
            author: Some("human".to_string()),
            origin: origin.clone(),
        };
        let (id, _) = store.insert(&e)?;
        eprintln!("  {key}: precedent {id}");

        // juror_stats: the human verdict is ground truth for every juror
        // that voted on this question.
        for j in &resp.usage.jurors {
            if j.status != "ok" {
                continue;
            }
            if let Some(answers) = &j.answers
                && let Some(v) = answers.get(key)
                && let Ok(b) = q.validate_answer(key, v)
            {
                let _ = store.juror_stats_update(&j.juror, &q.qid(), b == ballot);
            }
        }
    }
    Ok(())
}
