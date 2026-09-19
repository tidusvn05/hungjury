//! `hungjury batch` — decide every case in a JSONL file, in parallel.
//!
//! Input lines are `{"state": <text|{workspace,hint}>, "questions": {...}}`
//! (any `expected` labels are ignored — this is a runner, not an
//! evaluator). Each decided case appends one JSON line:
//! `{case, id, answers, decided_by, hung, exit}`; failures become
//! `{case, error}`. Quota exhaustion fails fast per case (the daily cap
//! check happens before any spawn, so no calls are wasted once spent).

use std::path::Path;

use futures_util::stream::{self, StreamExt};

use crate::error::{Error, Result};
use crate::jury::{self, DecideCtx};
use crate::request::Request;

/// One completed case: input index, echoed `case` label, decide result.
type CaseResult = (usize, serde_json::Value, Result<(crate::response::Response, i32)>);

/// Run `decide` over every line of `cases`; write JSONL to `out`
/// (stdout when `None`). Returns a process exit code: 0 when every case
/// ran (hung included), 1 when any case errored.
pub async fn run(ctx: &DecideCtx, cases: &Path, out: Option<&Path>) -> Result<u8> {
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
        let req = Request::from_json(
            &serde_json::json!({"state": v["state"], "questions": v["questions"]}).to_string(),
        )
        .map_err(|e| Error::Request(format!("{} line {}: {e}", cases.display(), i + 1)))?;
        let label = v.get("case").cloned().unwrap_or(serde_json::json!(i));
        cases_v.push((i, label, req));
    }
    if cases_v.is_empty() {
        return Err(Error::Request(format!("{}: no cases", cases.display())));
    }

    let n = cases_v.len();
    let par = ctx.config.limits.max_concurrency.max(1);
    let results: Vec<CaseResult> =
        stream::iter(
            cases_v
                .into_iter()
                .map(|(i, label, r)| async move { (i, label, jury::decide(ctx, &r).await) }),
        )
        .buffer_unordered(par)
        .collect()
        .await;

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
