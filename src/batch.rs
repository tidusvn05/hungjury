//! `hungjury batch` — decide every case in a JSONL file, in parallel.
//!
//! Input lines are `{"state": <text|{workspace,hint}>, "questions": {...}}`
//! — or `"questions_file": "questions.json"` (resolved against the cases
//! file's directory) when every case shares one question set.
//! Any `expected` labels are ignored — this is a runner, not an
//! evaluator). Each decided case appends one JSON line:
//! `{case, id, answers, decided_by, hung, exit}`; failures become
//! `{case, error}`. Quota exhaustion fails fast per case (the daily cap
//! check happens before any spawn, so no calls are wasted once spent).
//!
//! `--pack N` switches to prompt batching: up to N consecutive
//! same-questions cases share ONE juror call (the juror answers every
//! item in a single response). Vote/quorum/hung/escalation stay
//! per-item; quota is spent per *call* — a pack of N costs one call per
//! juror instead of N.

use std::path::Path;

use futures_util::stream::{self, StreamExt};

use crate::error::{Error, Result};
use crate::jury::{self, DecideCtx, PackItem};
use crate::request::Request;

/// One completed case: input index, echoed `case` label, decide result.
type CaseResult = (
    usize,
    serde_json::Value,
    Result<(crate::response::Response, i32)>,
);

/// Run `decide` over every line of `cases`; write JSONL to `out`
/// (stdout when `None`). Returns a process exit code: 0 when every case
/// ran (hung included), 1 when any case errored.
pub async fn run(ctx: &DecideCtx, cases: &Path, out: Option<&Path>, pack: usize) -> Result<u8> {
    let text = std::fs::read_to_string(cases).map_err(|e| Error::io(cases, e))?;
    let mut cases_v = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            Error::Request(format!("{} line {}: bad JSON: {e}", cases.display(), i + 1))
        })?;
        // `questions` inline wins; `questions_file` resolves against the
        // cases file's directory so a shared question set stays DRY.
        let questions = crate::request::case_questions(&v, cases, i + 1)?;
        let req = Request::from_json(
            &serde_json::json!({"state": v["state"], "questions": questions}).to_string(),
        )
        .map_err(|e| Error::Request(format!("{} line {}: {e}", cases.display(), i + 1)))?;
        let label = v.get("case").cloned().unwrap_or(serde_json::json!(i));
        cases_v.push((i, label, req));
    }
    if cases_v.is_empty() {
        return Err(Error::Request(format!("{}: no cases", cases.display())));
    }
    if pack > 1 && cases_v.iter().any(|(_, _, r)| r.state.is_workspace()) {
        return Err(Error::Request(
            "--pack requires text states — workspace cases can't share a prompt".into(),
        ));
    }

    let n = cases_v.len();
    let par = ctx.config.limits.max_concurrency.max(1);
    let results: Vec<CaseResult> = if pack > 1 {
        // Group consecutive same-questions cases into packs of ≤N —
        // a question-set change starts a new pack (packed prompt has
        // exactly one Questions section).
        let mut groups: Vec<Vec<(usize, serde_json::Value, Request)>> = Vec::new();
        for (i, label, req) in cases_v {
            let fits = groups.last().is_some_and(|g: &Vec<_>| {
                g.len() < pack
                    && serde_json::to_value(&g[0].2.questions).ok()
                        == serde_json::to_value(&req.questions).ok()
            });
            if !fits {
                groups.push(Vec::new());
            }
            groups
                .last_mut()
                .expect("just pushed")
                .push((i, label, req));
        }
        stream::iter(groups.into_iter().map(|g| async move {
            let idx: Vec<usize> = g.iter().map(|(i, _, _)| *i).collect();
            let labels: Vec<serde_json::Value> = g.iter().map(|(_, l, _)| l.clone()).collect();
            let items: Vec<PackItem> = g.into_iter().map(|(_, _, req)| PackItem { req }).collect();
            let res = jury::decide_pack(ctx, items).await;
            idx.into_iter()
                .zip(labels)
                .zip(res)
                .map(|((i, l), r)| (i, l, r))
                .collect::<Vec<CaseResult>>()
        }))
        .buffer_unordered(par)
        .collect::<Vec<Vec<CaseResult>>>()
        .await
        .into_iter()
        .flatten()
        .collect()
    } else {
        stream::iter(
            cases_v
                .into_iter()
                .map(|(i, label, r)| async move { (i, label, jury::decide(ctx, &r).await) }),
        )
        .buffer_unordered(par)
        .collect()
        .await
    };

    let mut lines = vec![String::new(); n];
    let (mut decided, mut hung, mut failed) = (0usize, 0usize, 0usize);
    for (i, label, r) in results {
        lines[i] = match r {
            Ok((resp, code)) => {
                if resp.hung.is_empty() {
                    decided += 1;
                } else {
                    hung += 1;
                }
                serde_json::json!({
                    "case": label,
                    "id": resp.id,
                    "answers": resp.answers,
                    "decided_by": resp.decided_by,
                    "hung": resp.hung,
                    "escalated": resp.escalated,
                    "sources": resp.sources,
                    "exit": code,
                })
                .to_string()
            }
            Err(e) => {
                failed += 1;
                serde_json::json!({"case": label, "error": e.to_string()}).to_string()
            }
        };
    }
    let mut body = lines.join("\n");
    body.push('\n');
    match out {
        Some(p) => crate::util::write_atomic(p, body.as_bytes())?,
        None => print!("{body}"),
    }
    eprintln!("batch: {decided} decided, {hung} hung, {failed} failed of {n}");
    Ok(u8::from(failed > 0))
}
