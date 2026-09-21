//! `hungjury eval` — the Phase-2 go/no-go experiment.
//!
//! Input is a JSONL file of labeled cases:
//! `{"state": <text|{workspace,hint}>, "questions": {…}, "expected": {…}}`
//!
//! The runner splits cases into train/test halves (deterministic, by line
//! order with `--seed` shuffling), trains memory by running `decide` with
//! `--escalate sync` over the train half, then measures three arms on the
//! frozen-memory test half:
//!
//! - `jury`: no memory, no escalation.
//! - `jury_memory`: memory read-only, no escalation.
//! - `judge`: the judge's own verdict per case (memory read-only).
//!
//! `report.json` records per-arm accuracy + hung rate and the go/no-go
//! verdict: memory passes when it closes ≥50% of the jury→judge accuracy
//! gap, or cuts the hung rate ≥30% without losing accuracy.

use std::collections::BTreeMap;
use std::path::Path;

use futures_util::stream::{self, StreamExt};
use serde::Serialize;

use crate::config::{CliOverrides, Config, Escalate};
use crate::error::{Error, Result};
use crate::judge;
use crate::jury::{self, DecideCtx};
use crate::question::Ballot;
use crate::request::Request;
use crate::response::{AnswerOut, Response};

/// One labeled eval case.
#[derive(Clone)]
struct Case {
    /// Rebuilt request (state + questions).
    req: Request,
    /// `key → expected verdict` as the juror's raw answer value.
    expected: BTreeMap<String, serde_json::Value>,
}

/// Load a JSONL cases file.
fn load_cases(path: &Path) -> Result<Vec<Case>> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let mut cases = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            Error::Request(format!("{} line {}: bad JSON: {e}", path.display(), i + 1))
        })?;
        let expected: BTreeMap<String, serde_json::Value> = v["expected"]
            .as_object()
            .map(|o| o.iter().map(|(k, x)| (k.clone(), x.clone())).collect())
            .ok_or_else(|| {
                Error::Request(format!(
                    "{} line {}: missing 'expected' object",
                    path.display(),
                    i + 1
                ))
            })?;
        // Shared question set: `questions_file` resolves against the
        // cases file's directory (same contract as `batch`).
        let questions = crate::request::case_questions(&v, path, i + 1)?;
        let req = Request::from_json(
            &serde_json::json!({
                "state": v["state"],
                "questions": questions,
            })
            .to_string(),
        )
        .map_err(|e| Error::Request(format!("{} line {}: {e}", path.display(), i + 1)))?;
        cases.push(Case { req, expected });
    }
    if cases.is_empty() {
        return Err(Error::Request(format!("{}: no cases", path.display())));
    }
    Ok(cases)
}

/// Compare a decided answer's top-level verdict to the expected value.
/// Returns `None` when the question has no committed verdict (hung).
pub fn answer_matches(
    a: &crate::response::AnswerOut,
    expected: &serde_json::Value,
) -> Option<bool> {
    match a {
        crate::response::AnswerOut::Choice {
            choice, confidence, ..
        } => {
            (*confidence)?;
            Some(serde_json::Value::String(choice.clone()) == *expected)
        }
        crate::response::AnswerOut::Score {
            score, confidence, ..
        } => {
            (*confidence)?;
            Some(
                expected
                    .as_f64()
                    .is_some_and(|e| (score.round() - e).abs() < 0.5)
                    || expected.as_i64().is_some_and(|e| score.round() as i64 == e),
            )
        }
        crate::response::AnswerOut::Noul {
            noul, confidence, ..
        } => {
            (*confidence)?;
            let verdict = *noul >= 0.5;
            Some(expected.as_bool().is_some_and(|e| e == verdict))
        }
    }
}

/// Judge ballot vs expected.
fn ballot_matches(b: &Ballot, expected: &serde_json::Value) -> bool {
    match b {
        Ballot::Choice(c) => expected.as_str() == Some(c.as_str()),
        Ballot::Score(s) => expected.as_u64() == Some(*s as u64),
        Ballot::Noul(v) => expected.as_bool() == Some(*v),
        // A judge abstention never matches a concrete expectation.
        Ballot::Abstain => expected.as_str() == Some("hung"),
    }
}

/// Per-arm counts.
#[derive(Default, Serialize)]
struct ArmStats {
    /// Questions with a committed verdict.
    decided: usize,
    /// Questions still hung.
    hung: usize,
    /// Decided + correct.
    correct: usize,
    /// Spawned CLI calls (juror attempts + judge calls).
    calls: usize,
    /// Calls split by backend (`claude`/`codex`/`devin`).
    calls_by_backend: BTreeMap<String, usize>,
    /// Estimated USD spent (`None` when `[costs]` isn't configured).
    est_cost_usd: Option<f64>,
    /// Per-case decision wall ms (for mean/p95).
    #[serde(skip)]
    walls: Vec<u64>,
    /// Decided-but-wrong details `{case, key, expected, got}` (cap 20).
    mismatches: Vec<serde_json::Value>,
    /// Total rulings/precedents/facts injected into juror prompts.
    memory_injected: usize,
    /// Per-question-key decided/correct — surfaces which question is
    /// the weak axis (e.g. `frustration`) without reading mismatches.
    per_key: BTreeMap<String, [usize; 2]>,
    /// Expected keys never scored — the case's decide/judge call failed,
    /// so `decided + hung + unscored` = total expected keys.
    unscored: usize,
}

impl ArmStats {
    fn accuracy(&self) -> f64 {
        if self.decided == 0 {
            0.0
        } else {
            self.correct as f64 / self.decided as f64
        }
    }
    fn hung_rate(&self) -> f64 {
        let total = self.decided + self.hung;
        if total == 0 {
            0.0
        } else {
            self.hung as f64 / total as f64
        }
    }
}

/// The winning value of an aggregated answer, as plain JSON.
fn answerout_json(a: &AnswerOut) -> serde_json::Value {
    match a {
        AnswerOut::Choice { choice, .. } => serde_json::Value::String(choice.clone()),
        AnswerOut::Score { score, .. } => serde_json::json!(score.round() as i64),
        AnswerOut::Noul { noul, .. } => serde_json::Value::Bool(*noul >= 0.5),
    }
}

fn note_calls(stats: &mut ArmStats, agent: &str, n: usize) {
    stats.calls += n;
    let backend = agent.split(':').next().unwrap_or("?").to_string();
    *stats.calls_by_backend.entry(backend).or_default() += n;
}

fn note_mismatch(
    stats: &mut ArmStats,
    case: usize,
    key: &str,
    want: &serde_json::Value,
    got: serde_json::Value,
) {
    if stats.mismatches.len() < 20 {
        stats.mismatches.push(serde_json::json!({
            "case": case, "key": key, "expected": want, "got": got,
        }));
    }
}

/// Expected keys of a case whose decide/judge call failed — neither
/// decided nor hung, so without this counter they vanish from the arm.
fn note_failure(stats: &mut ArmStats, expected: &BTreeMap<String, serde_json::Value>) {
    stats.unscored += expected.len();
}

/// Accumulate one response into an arm.
fn score_response(
    stats: &mut ArmStats,
    case: usize,
    resp: &Response,
    expected: &BTreeMap<String, serde_json::Value>,
) {
    stats.walls.push(resp.usage.wall_ms);
    stats.memory_injected += resp.memory.rulings + resp.memory.precedents + resp.memory.facts;
    if let Some(c) = resp.usage.est_cost_usd {
        *stats.est_cost_usd.get_or_insert(0.0) += c;
    }
    for j in &resp.usage.jurors {
        note_calls(stats, &j.juror, 1 + j.retries as usize);
    }
    if let Some(ju) = &resp.usage.judge {
        note_calls(stats, &ju.model, 1);
    }
    for (key, want) in expected {
        let k = stats.per_key.entry(key.clone()).or_default();
        // `"hung"` is a valid expectation: correct iff the key stayed
        // unresolved (hung or judge-abstained).
        if want.as_str() == Some("hung") {
            stats.decided += 1;
            k[0] += 1;
            if resp.hung.contains(key) {
                stats.correct += 1;
                k[1] += 1;
            } else {
                let got = resp
                    .answers
                    .get(key)
                    .map(answerout_json)
                    .unwrap_or(serde_json::json!("missing"));
                note_mismatch(stats, case, key, want, got);
            }
            continue;
        }
        match resp.answers.get(key).and_then(|a| answer_matches(a, want)) {
            Some(true) => {
                stats.decided += 1;
                stats.correct += 1;
                k[0] += 1;
                k[1] += 1;
            }
            Some(false) => {
                stats.decided += 1;
                k[0] += 1;
                note_mismatch(stats, case, key, want, answerout_json(&resp.answers[key]));
            }
            None => stats.hung += 1,
        }
    }
}

/// Score one judge call against expected values.
fn score_judge(
    stats: &mut ArmStats,
    case: usize,
    call: &judge::JudgeCall,
    expected: &BTreeMap<String, serde_json::Value>,
) {
    for (key, want) in expected {
        let k = stats.per_key.entry(key.clone()).or_default();
        // Same `"hung"` semantics as score_response: abstaining (no
        // judged entry) is the correct outcome, not an uncounted hung.
        if want.as_str() == Some("hung") {
            stats.decided += 1;
            k[0] += 1;
            match call.judged.get(key) {
                None => {
                    stats.correct += 1;
                    k[1] += 1;
                }
                Some(j) => note_mismatch(stats, case, key, want, j.ballot.to_json()),
            }
            continue;
        }
        match call.judged.get(key) {
            Some(j) => {
                stats.decided += 1;
                k[0] += 1;
                if ballot_matches(&j.ballot, want) {
                    stats.correct += 1;
                    k[1] += 1;
                } else {
                    note_mismatch(stats, case, key, want, j.ballot.to_json());
                }
            }
            None => stats.hung += 1,
        }
    }
}

/// Build a fresh ctx for one arm (own quota/cache/store handles).
fn ctx_for(over: &CliOverrides, config_path: Option<&Path>) -> Result<DecideCtx> {
    let cfg = Config::load(over, config_path)?;
    DecideCtx::new(cfg, None)
}

/// The full experiment over `seeds` deterministic shuffles. Each seed
/// runs against a throwaway memory db — the eval never pollutes the
/// project's real rulings, and seeds can't leak rulings into each other.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    cases_path: &Path,
    train_frac: f64,
    report_path: &Path,
    base: &CliOverrides,
    config_path: Option<&Path>,
    seed: u64,
    label: Option<&str>,
    seeds: u64,
    audit_train: usize,
) -> Result<()> {
    let cases = load_cases(cases_path)?;
    let mut reports = Vec::new();
    let mut seed_errors = Vec::new();
    for s in seed..seed + seeds.max(1) {
        let tmp =
            std::env::temp_dir().join(format!("hungjury-eval-{}-{s}.db", crate::util::nonce()));
        let mut over = base.clone();
        over.memory_db = Some(tmp.clone());
        let rep = run_seed(
            &cases,
            train_frac,
            &over,
            config_path,
            s,
            label,
            audit_train,
        )
        .await;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", tmp.display()));
        }
        match rep {
            Ok(r) => reports.push((s, r)),
            // A failed seed must not discard the seeds that did run.
            Err(e) => {
                eprintln!("eval: seed {s} failed: {e}");
                seed_errors.push(serde_json::json!({"seed": s, "error": e.to_string()}));
            }
        }
    }
    if reports.is_empty() {
        return Err(Error::Memory(format!(
            "all {} seed(s) failed",
            seeds.max(1)
        )));
    }

    let body = if reports.len() == 1 && seed_errors.is_empty() {
        serde_json::to_string_pretty(&reports[0].1)
    } else {
        serde_json::to_string_pretty(&aggregate_report(label, &reports, &seed_errors))
    }
    .map_err(|e| Error::Memory(e.to_string()))?;
    crate::util::write_atomic(report_path, body.as_bytes())?;
    eprintln!("eval: wrote {}", report_path.display());
    println!("{body}");
    Ok(())
}

/// Mean/min/max across seeds, per arm + go verdicts. `seed_errors`
/// records seeds that failed so the aggregate isn't silently partial.
fn aggregate_report(
    label: Option<&str>,
    reports: &[(u64, serde_json::Value)],
    seed_errors: &[serde_json::Value],
) -> serde_json::Value {
    let arms = ["jury", "jury_memory", "judge", "judge_informed"];
    let mut agg_arms = serde_json::Map::new();
    for arm in arms {
        let accs: Vec<f64> = reports
            .iter()
            .filter_map(|(_, r)| r["arms"][arm]["accuracy"].as_f64())
            .collect();
        let injects: Vec<f64> = reports
            .iter()
            .filter_map(|(_, r)| r["arms"][arm]["memory_injected"].as_f64())
            .collect();
        let mean = accs.iter().sum::<f64>() / accs.len().max(1) as f64;
        // Per-key accuracy means: collect the union of keys seen in any
        // seed's per_key block, then average each across seeds.
        let mut key_names: std::collections::BTreeSet<String> = Default::default();
        for (_, r) in reports {
            if let Some(pk) = r["arms"][arm]["per_key"].as_object() {
                key_names.extend(pk.keys().cloned());
            }
        }
        let per_key: serde_json::Map<String, serde_json::Value> = key_names
            .into_iter()
            .map(|k| {
                let v: Vec<f64> = reports
                    .iter()
                    .filter_map(|(_, r)| r["arms"][arm]["per_key"][&k]["accuracy"].as_f64())
                    .collect();
                (
                    k,
                    serde_json::json!(v.iter().sum::<f64>() / v.len().max(1) as f64),
                )
            })
            .collect();
        agg_arms.insert(
            arm.to_string(),
            serde_json::json!({
                "accuracy_mean": mean,
                "accuracy_min": accs.iter().cloned().fold(f64::INFINITY, f64::min),
                "accuracy_max": accs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                "memory_injected_mean": injects.iter().sum::<f64>() / injects.len().max(1) as f64,
                "per_key_mean": per_key,
            }),
        );
    }
    let deltas: Vec<f64> = reports
        .iter()
        .filter_map(|(_, r)| r["go"]["memory_delta_vs_jury"].as_f64())
        .collect();
    let passes = reports
        .iter()
        .filter(|(_, r)| r["go"]["pass"].as_bool() == Some(true))
        .count();
    serde_json::json!({
        "label": label,
        "seeds_run": reports.len(),
        "aggregate": {
            "arms": agg_arms,
            "memory_delta_vs_jury_mean": deltas.iter().sum::<f64>() / deltas.len().max(1) as f64,
            "go_pass": format!("{passes}/{}", reports.len()),
        },
        "runs": reports.iter().map(|(s, r)| serde_json::json!({"seed": s, "report": r})).collect::<Vec<_>>(),
        "seed_errors": seed_errors,
    })
}

/// One seed: shuffle → train → four test arms → report JSON.
async fn run_seed(
    cases: &[Case],
    train_frac: f64,
    base: &CliOverrides,
    config_path: Option<&Path>,
    seed: u64,
    label: Option<&str>,
    audit_train: usize,
) -> Result<serde_json::Value> {
    let mut cases = cases.to_vec();
    shuffle(&mut cases, seed);
    let n = cases.len();
    let n_train = ((n as f64) * train_frac.clamp(0.0, 1.0)).round() as usize;
    let (train, test) = cases.split_at(n_train.min(n.saturating_sub(1)));
    eprintln!(
        "eval: seed {seed} — {} cases — {} train, {} test",
        n,
        train.len(),
        test.len()
    );

    // Pass 1: train — `decide --escalate sync` populates memory.
    let mut train_escalations = 0usize;
    let mut rulings_written = 0usize;
    let mut contested_written = 0usize;
    {
        let mut over = base.clone();
        over.escalate = Some(Escalate::Sync);
        over.no_memory = false;
        over.memory_readonly = false;
        over.no_cache = true;
        let ctx = ctx_for(&over, config_path)?;
        let par = ctx.config.limits.max_concurrency.max(1);
        let results: Vec<_> = stream::iter(train.iter().enumerate().map(|(i, c)| {
            let ctx = &ctx;
            async move { (i, jury::decide(ctx, &c.req).await) }
        }))
        .buffer_unordered(par)
        .collect()
        .await;
        for (i, r) in results {
            match r {
                Ok((resp, _)) => {
                    if resp.usage.judge.is_some() {
                        train_escalations += 1;
                    }
                    eprintln!("  train {}/{} done", i + 1, train.len());
                }
                Err(e) => eprintln!("  train {}/{} failed: {e}", i + 1, train.len()),
            }
        }
        // Optional: judge re-judges K of the just-written jury decisions —
        // rulings exist even when the jury never hung (learn --audit).
        if audit_train > 0 {
            crate::learn::learn_audit(&ctx, audit_train, true, false).await?;
        }
        // What training persisted — previously only auditable from the
        // throwaway memory db before it was deleted.
        if let Some(store) = &ctx.store {
            for (kind, status, n) in store.counts().unwrap_or_default() {
                if kind == "ruling" {
                    rulings_written += n as usize;
                }
                if status == "contested" {
                    contested_written += n as usize;
                }
            }
        }
    }

    // Pass 2: jury arm — no memory, no escalation. Responses are kept:
    // the `judge_informed` arm reuses their ballots.
    let mut jury_stats = ArmStats::default();
    let mut jury_resps: Vec<Option<Response>> = vec![None; test.len()];
    {
        let mut over = base.clone();
        over.no_memory = true;
        over.escalate = Some(Escalate::Off);
        over.no_cache = true;
        let ctx = ctx_for(&over, config_path)?;
        let par = ctx.config.limits.max_concurrency.max(1);
        let results: Vec<_> = stream::iter(test.iter().enumerate().map(|(i, c)| {
            let ctx = &ctx;
            async move { (i, jury::decide(ctx, &c.req).await) }
        }))
        .buffer_unordered(par)
        .collect()
        .await;
        for (i, r) in results {
            match r {
                Ok((resp, _)) => {
                    score_response(&mut jury_stats, i, &resp, &test[i].expected);
                    jury_resps[i] = Some(resp);
                }
                Err(e) => {
                    note_failure(&mut jury_stats, &test[i].expected);
                    eprintln!("  jury test {}/{} failed: {e}", i + 1, test.len());
                }
            }
        }
    }

    // Pass 3: jury+memory arm — frozen memory, no escalation.
    let mut mem_stats = ArmStats::default();
    {
        let mut over = base.clone();
        over.no_memory = false;
        over.memory_readonly = true;
        over.escalate = Some(Escalate::Off);
        over.no_cache = true;
        let ctx = ctx_for(&over, config_path)?;
        let par = ctx.config.limits.max_concurrency.max(1);
        let results: Vec<_> = stream::iter(test.iter().enumerate().map(|(i, c)| {
            let ctx = &ctx;
            async move { (i, jury::decide(ctx, &c.req).await) }
        }))
        .buffer_unordered(par)
        .collect()
        .await;
        for (i, r) in results {
            match r {
                Ok((resp, _)) => score_response(&mut mem_stats, i, &resp, &test[i].expected),
                Err(e) => {
                    note_failure(&mut mem_stats, &test[i].expected);
                    eprintln!("  mem test {}/{} failed: {e}", i + 1, test.len());
                }
            }
        }
    }

    // Pass 4: judge arm — the judge answers every question directly.
    let mut judge_stats = ArmStats::default();
    {
        let mut over = base.clone();
        over.no_memory = false;
        over.memory_readonly = true;
        over.no_cache = true;
        let ctx = ctx_for(&over, config_path)?;
        let par = ctx.config.limits.max_concurrency.max(1);
        let results: Vec<_> = stream::iter(test.iter().enumerate().map(|(i, c)| {
            let ctx = &ctx;
            async move {
                let hung: Vec<String> = c.req.questions.keys().cloned().collect();
                let memory_block = match &ctx.store {
                    Some(s) => crate::memory::retrieve::retrieve(
                        s,
                        &c.req,
                        &ctx.config.memory,
                        ctx.config.namespace.as_deref(),
                        None,
                    )
                    .map(|r| r.block)
                    .unwrap_or_default(),
                    None => String::new(),
                };
                let ws_path = match &c.req.state {
                    crate::request::State::Workspace { path, .. } => Some(path.as_path()),
                    _ => None,
                };
                let (call, usage) = judge::judge_call(
                    ctx,
                    &c.req,
                    &memory_block,
                    &[],
                    &BTreeMap::new(),
                    &hung,
                    &crate::util::nonce(),
                    ws_path,
                )
                .await;
                (i, call, usage)
            }
        }))
        .buffer_unordered(par)
        .collect::<Vec<_>>()
        .await;
        for (i, call, usage) in results {
            note_calls(&mut judge_stats, &usage.model, 1);
            judge_stats.walls.push(usage.ms);
            match call {
                Some(call) => score_judge(&mut judge_stats, i, &call, &test[i].expected),
                None => {
                    note_failure(&mut judge_stats, &test[i].expected);
                    eprintln!(
                        "  judge test {}/{} failed: {}",
                        i + 1,
                        test.len(),
                        usage.error.unwrap_or_default()
                    );
                }
            }
        }
    }

    // Pass 5: judge_informed — production-faithful judge that sees the
    // jury's ballots, the aggregated answers, and memory. Fairer ceiling
    // than the cold `judge` arm.
    let mut informed_stats = ArmStats::default();
    {
        let mut over = base.clone();
        over.no_memory = false;
        over.memory_readonly = true;
        over.no_cache = true;
        let ctx = ctx_for(&over, config_path)?;
        let par = ctx.config.limits.max_concurrency.max(1);
        let results: Vec<_> = stream::iter(
            test.iter()
                .enumerate()
                .filter_map(|(i, c)| jury_resps[i].as_ref().map(|r| (i, c, r)))
                .map(|(i, c, resp)| {
                    let ctx = &ctx;
                    async move {
                        let hung: Vec<String> = c.req.questions.keys().cloned().collect();
                        let (memory_block, injected) = match &ctx.store {
                            Some(s) => crate::memory::retrieve::retrieve(
                                s,
                                &c.req,
                                &ctx.config.memory,
                                ctx.config.namespace.as_deref(),
                                None,
                            )
                            .map(|r| {
                                let n = r.used.rulings + r.used.precedents + r.used.facts;
                                (r.block, n)
                            })
                            .unwrap_or_default(),
                            None => Default::default(),
                        };
                        let juror_ballots: Vec<(String, BTreeMap<String, Ballot>)> = resp
                            .usage
                            .jurors
                            .iter()
                            .filter(|j| j.status == "ok")
                            .filter_map(|j| {
                                j.answers.as_ref().map(|a| (j.juror.clone(), a.clone()))
                            })
                            .map(|(name, ans)| {
                                let ballots: BTreeMap<String, Ballot> = ans
                                    .iter()
                                    .filter_map(|(k, v)| {
                                        c.req
                                            .questions
                                            .get(k)
                                            .and_then(|q| q.validate_answer(k, v).ok())
                                            .map(|b| (k.clone(), b))
                                    })
                                    .collect();
                                (name, ballots)
                            })
                            .collect();
                        let ws_path = match &c.req.state {
                            crate::request::State::Workspace { path, .. } => Some(path.as_path()),
                            _ => None,
                        };
                        let (call, usage) = judge::judge_call(
                            ctx,
                            &c.req,
                            &memory_block,
                            &juror_ballots,
                            &resp.answers,
                            &hung,
                            &crate::util::nonce(),
                            ws_path,
                        )
                        .await;
                        (i, call, usage, injected)
                    }
                }),
        )
        .buffer_unordered(par)
        .collect::<Vec<_>>()
        .await;
        for (i, call, usage, injected) in results {
            note_calls(&mut informed_stats, &usage.model, 1);
            informed_stats.memory_injected += injected;
            informed_stats.walls.push(usage.ms);
            match call {
                Some(call) => score_judge(&mut informed_stats, i, &call, &test[i].expected),
                None => {
                    note_failure(&mut informed_stats, &test[i].expected);
                    eprintln!(
                        "  informed test {}/{} failed: {}",
                        i + 1,
                        test.len(),
                        usage.error.unwrap_or_default()
                    );
                }
            }
        }
    }

    // Go/no-go. When the judge is the ceiling (gap > 0), memory passes
    // by closing ≥50% of that gap. When it isn't (jury ≥ judge — small
    // or easy sets can put the jury on top), the meaningful bar is
    // that memory doesn't drag the jury below its no-memory accuracy.
    let (ja, ma, ga) = (
        jury_stats.accuracy(),
        mem_stats.accuracy(),
        judge_stats.accuracy(),
    );
    let gap = ga - ja;
    let mem_delta = ma - ja;
    let closed = if gap > 0.0 { mem_delta / gap } else { 0.0 };
    let hung_drop = if jury_stats.hung_rate() > 0.0 {
        (jury_stats.hung_rate() - mem_stats.hung_rate()) / jury_stats.hung_rate()
    } else {
        0.0
    };
    let go = if gap > 0.0 {
        closed >= 0.5 || (hung_drop >= 0.3 && ma >= ja - 0.001)
    } else {
        ma >= ja - 0.001
    };

    let report_cfg = Config::load(base, config_path)?;
    Ok(serde_json::json!({
        "label": label,
        "cases": n,
        "train": train.len(),
        "train_escalations": train_escalations,
        "rulings_written": rulings_written,
        "contested_written": contested_written,
        "test": test.len(),
        "seed": seed,
        "config": {
            "jurors": report_cfg.jurors,
            "judge": report_cfg.judge,
            "samples": report_cfg.samples,
            "hung_threshold": report_cfg.hung_threshold,
            "min_quorum": report_cfg.min_quorum,
            "escalate": format!("{:?}", report_cfg.escalate),
        },
        "arms": {
            "jury": arm_json(&jury_stats),
            "jury_memory": arm_json(&mem_stats),
            "judge": arm_json(&judge_stats),
            "judge_informed": arm_json(&informed_stats),
        },
        "go": {
            "accuracy_gap_jury_to_judge": gap,
            "gap_closed_by_memory": closed,
            "memory_delta_vs_jury": mem_delta,
            "hung_rate_drop": hung_drop,
            "pass": go,
        }
    }))
}

fn arm_json(s: &ArmStats) -> serde_json::Value {
    let mut w = s.walls.clone();
    w.sort_unstable();
    let n = w.len();
    let mean = if n == 0 {
        0.0
    } else {
        w.iter().map(|x| *x as f64).sum::<f64>() / n as f64
    };
    let p95 = if n == 0 {
        0
    } else {
        w[((n as f64 * 0.95).ceil() as usize).max(1) - 1]
    };
    serde_json::json!({
        "decided": s.decided,
        "hung": s.hung,
        "unscored": s.unscored,
        "correct": s.correct,
        "accuracy": s.accuracy(),
        "hung_rate": s.hung_rate(),
        "calls": s.calls,
        "calls_by_backend": s.calls_by_backend,
        "est_cost_usd": s.est_cost_usd,
        "wall_ms_mean": mean,
        "wall_ms_p95": p95,
        "memory_injected": s.memory_injected,
        "per_key": s.per_key.iter().map(|(k, v)| (k.clone(), serde_json::json!({
            "decided": v[0],
            "correct": v[1],
            "accuracy": if v[0] == 0 { 0.0 } else { v[1] as f64 / v[0] as f64 },
        }))).collect::<serde_json::Map<_, _>>(),
        "mismatches": s.mismatches,
    })
}

/// Deterministic Fisher–Yates with a split-mix64 stream.
fn shuffle<T>(v: &mut [T], seed: u64) {
    let mut s = seed.wrapping_add(0x9e3779b97f4a7c15);
    let mut next = || {
        s = s.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    };
    for i in (1..v.len()).rev() {
        v.swap(i, (next() % (i as u64 + 1)) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::AnswerOut;
    use serde_json::json;

    #[test]
    fn answer_matches_variants() {
        let c = AnswerOut::Choice {
            choice: "a".into(),
            probabilities: BTreeMap::new(),
            confidence: Some(0.6),
            judge: None,
        };
        assert_eq!(answer_matches(&c, &json!("a")), Some(true));
        assert_eq!(answer_matches(&c, &json!("b")), Some(false));

        let s = AnswerOut::Score {
            score: 1.4,
            legend: "x".into(),
            confidence: Some(0.8),
            judge: None,
        };
        assert_eq!(answer_matches(&s, &json!(1)), Some(true));
        assert_eq!(answer_matches(&s, &json!(2)), Some(false));

        let n = AnswerOut::Noul {
            noul: 0.8,
            confidence: Some(0.6),
            judge: None,
        };
        assert_eq!(answer_matches(&n, &json!(true)), Some(true));
        assert_eq!(answer_matches(&n, &json!(false)), Some(false));

        // Hung (no confidence) → None, counted as hung not wrong.
        let h = AnswerOut::Noul {
            noul: 0.5,
            confidence: None,
            judge: None,
        };
        assert_eq!(answer_matches(&h, &json!(true)), None);
    }

    #[test]
    fn arm_json_reports_calls_and_latency() {
        use crate::response::{DecidedBy, JudgeUsage, JurorUsage, MemoryUse, Response, Usage};
        let resp = Response {
            id: "dec_x".into(),
            decided_by: DecidedBy::Jury,
            answers: BTreeMap::from([(
                "k".into(),
                AnswerOut::Choice {
                    choice: "a".into(),
                    probabilities: BTreeMap::new(),
                    confidence: Some(0.9),
                    judge: None,
                },
            )]),
            hung: vec![],
            escalated: vec![],
            sources: BTreeMap::new(),
            memory: MemoryUse::default(),
            usage: Usage {
                wall_ms: 100,
                jurors: vec![
                    JurorUsage {
                        juror: "a".into(),
                        sample: 0,
                        status: "ok".into(),
                        ms: 100,
                        retries: 1,
                        answers: None,
                        error: None,
                        input_tokens: None,
                        output_tokens: None,
                    },
                    JurorUsage {
                        juror: "b".into(),
                        sample: 0,
                        status: "ok".into(),
                        ms: 90,
                        retries: 0,
                        answers: None,
                        error: None,
                        input_tokens: None,
                        output_tokens: None,
                    },
                ],
                judge: Some(JudgeUsage {
                    model: "m".into(),
                    status: "ok".into(),
                    ms: 50,
                    wrote: vec![],
                    error: None,
                }),
                est_cost_usd: None,
            },
        };
        let mut stats = ArmStats::default();
        let expected = BTreeMap::from([("k".into(), json!("a"))]);
        score_response(&mut stats, 0, &resp, &expected);
        let j = arm_json(&stats);
        assert_eq!(j["calls"], json!(4)); // 2 jurors + 1 retry + 1 judge
        assert_eq!(j["calls_by_backend"]["a"], json!(2));
        assert_eq!(j["calls_by_backend"]["m"], json!(1));
        assert_eq!(j["wall_ms_mean"], json!(100.0));
        assert_eq!(j["wall_ms_p95"], json!(100));
        assert_eq!(j["accuracy"], json!(1.0));
    }

    #[test]
    fn score_judge_hung_expectation_matches_jury_semantics() {
        use crate::judge::{JudgeCall, JudgeOut};
        use crate::question::Ballot;
        use crate::response::JudgeVerdict;
        // Judge abstained on `a` (not judged), decided `b` wrongly, and
        // decided `c` correctly. `a` expects "hung" — abstention must
        // count decided+correct exactly like score_response.
        let call = JudgeCall {
            judged: BTreeMap::from([
                (
                    "b".into(),
                    JudgeOut {
                        ballot: Ballot::Noul(true),
                        verdict: JudgeVerdict {
                            choice: None,
                            score: None,
                            noul: Some(true),
                            rationale: None,
                        },
                    },
                ),
                (
                    "c".into(),
                    JudgeOut {
                        ballot: Ballot::Choice("x".into()),
                        verdict: JudgeVerdict {
                            choice: Some("x".into()),
                            score: None,
                            noul: None,
                            rationale: None,
                        },
                    },
                ),
            ]),
            raw: json!({}),
        };
        let expected = BTreeMap::from([
            ("a".into(), json!("hung")),
            ("b".into(), json!(false)),
            ("c".into(), json!("x")),
        ]);
        let mut stats = ArmStats::default();
        score_judge(&mut stats, 0, &call, &expected);
        assert_eq!(stats.decided, 3);
        assert_eq!(stats.correct, 2);
        assert_eq!(stats.hung, 0);
        assert_eq!(stats.mismatches.len(), 1);
        assert_eq!(stats.per_key["a"], [1, 1]);
    }

    #[test]
    fn score_response_hung_expectation() {
        use crate::response::{DecidedBy, MemoryUse, Response, Usage};
        let resp = Response {
            id: "dec_x".into(),
            decided_by: DecidedBy::Jury,
            answers: BTreeMap::new(),
            hung: vec!["a".into()],
            escalated: vec![],
            sources: BTreeMap::new(),
            memory: MemoryUse::default(),
            usage: Usage {
                wall_ms: 0,
                jurors: vec![],
                judge: None,
                est_cost_usd: None,
            },
        };
        let mut stats = ArmStats::default();
        let expected = BTreeMap::from([("a".into(), json!("hung"))]);
        score_response(&mut stats, 0, &resp, &expected);
        assert_eq!(stats.decided, 1);
        assert_eq!(stats.correct, 1);
        // A key expected to hang that instead carries no answer at all
        // must not panic.
        let mut stats2 = ArmStats::default();
        let resp2 = Response {
            hung: vec![],
            ..resp
        };
        score_response(&mut stats2, 0, &resp2, &expected);
        assert_eq!(stats2.correct, 0);
        assert_eq!(stats2.mismatches[0]["got"], json!("missing"));
    }

    #[test]
    fn unscored_explains_decided_hung_gap() {
        use crate::response::{DecidedBy, MemoryUse, Response, Usage};
        // One scored case (1 decided key, 1 hung key) plus one case whose
        // call failed (3 expected keys): decided + hung + unscored must
        // account for every expected key — the audit gap that produced
        // "decided=87, hung=0 on 90 keys".
        let resp = Response {
            id: "dec_x".into(),
            decided_by: DecidedBy::Jury,
            answers: BTreeMap::from([
                (
                    "a".into(),
                    AnswerOut::Choice {
                        choice: "x".into(),
                        probabilities: BTreeMap::new(),
                        confidence: Some(0.9),
                        judge: None,
                    },
                ),
                (
                    "b".into(),
                    AnswerOut::Noul {
                        noul: 0.5,
                        confidence: None,
                        judge: None,
                    },
                ),
            ]),
            hung: vec!["b".into()],
            escalated: vec![],
            sources: BTreeMap::new(),
            memory: MemoryUse::default(),
            usage: Usage {
                wall_ms: 0,
                jurors: vec![],
                judge: None,
                est_cost_usd: None,
            },
        };
        let mut stats = ArmStats::default();
        let expected = BTreeMap::from([("a".into(), json!("x")), ("b".into(), json!(true))]);
        score_response(&mut stats, 0, &resp, &expected);
        note_failure(
            &mut stats,
            &BTreeMap::from([
                ("c".into(), json!("x")),
                ("d".into(), json!(true)),
                ("e".into(), json!(1)),
            ]),
        );
        let j = arm_json(&stats);
        assert_eq!(j["decided"], json!(1));
        assert_eq!(j["hung"], json!(1));
        assert_eq!(j["unscored"], json!(3));
    }

    #[test]
    fn shuffle_is_deterministic() {
        let mut a: Vec<i32> = (0..20).collect();
        let mut b = a.clone();
        shuffle(&mut a, 42);
        shuffle(&mut b, 42);
        assert_eq!(a, b);
        let mut c: Vec<i32> = (0..20).collect();
        shuffle(&mut c, 7);
        assert_ne!(a, c);
    }
}
