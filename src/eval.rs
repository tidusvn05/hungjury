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
                Error::Request(format!("{} line {}: missing 'expected' object", path.display(), i + 1))
            })?;
        let req = Request::from_json(&serde_json::json!({
            "state": v["state"],
            "questions": v["questions"],
        })
        .to_string())
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
pub fn answer_matches(a: &crate::response::AnswerOut, expected: &serde_json::Value) -> Option<bool> {
    match a {
        crate::response::AnswerOut::Choice { choice, confidence, .. } => {
            (*confidence)?;
            Some(serde_json::Value::String(choice.clone()) == *expected)
        }
        crate::response::AnswerOut::Score { score, confidence, .. } => {
            (*confidence)?;
            Some(expected.as_f64().is_some_and(|e| (score.round() - e).abs() < 0.5)
                || expected.as_i64().is_some_and(|e| score.round() as i64 == e))
        }
        crate::response::AnswerOut::Noul { noul, confidence, .. } => {
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

/// Accumulate one response into an arm.
fn score_response(
    stats: &mut ArmStats,
    case: usize,
    resp: &Response,
    expected: &BTreeMap<String, serde_json::Value>,
) {
    stats.walls.push(resp.usage.wall_ms);
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
        match resp.answers.get(key).and_then(|a| answer_matches(a, want)) {
            Some(true) => {
                stats.decided += 1;
                stats.correct += 1;
            }
            Some(false) => {
                stats.decided += 1;
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
        match call.judged.get(key) {
            Some(j) => {
                stats.decided += 1;
                if ballot_matches(&j.ballot, want) {
                    stats.correct += 1;
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

/// The full experiment. `seed` shuffles the split deterministically.
pub async fn run(
    cases_path: &Path,
    train_frac: f64,
    report_path: &Path,
    base: &CliOverrides,
    config_path: Option<&Path>,
    seed: u64,
    label: Option<&str>,
) -> Result<()> {
    let mut cases = load_cases(cases_path)?;
    shuffle(&mut cases, seed);
    let n = cases.len();
    let n_train = ((n as f64) * train_frac.clamp(0.0, 1.0)).round() as usize;
    let (train, test) = cases.split_at(n_train.min(n.saturating_sub(1)));
    eprintln!("eval: {} cases — {} train, {} test", n, train.len(), test.len());

    // Pass 1: train — `decide --escalate sync` populates memory.
    {
        let mut over = base.clone();
        over.escalate = Some(Escalate::Sync);
        over.no_memory = false;
        over.memory_readonly = false;
        over.no_cache = true;
        let ctx = ctx_for(&over, config_path)?;
        let par = ctx.config.limits.max_concurrency.max(1);
        let results: Vec<_> = stream::iter(
            train
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let ctx = &ctx;
                    async move { (i, jury::decide(ctx, &c.req).await) }
                }),
        )
        .buffer_unordered(par)
        .collect()
        .await;
        for (i, r) in results {
            match r {
                Ok(_) => eprintln!("  train {}/{} done", i + 1, train.len()),
                Err(e) => eprintln!("  train {}/{} failed: {e}", i + 1, train.len()),
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
        let results: Vec<_> = stream::iter(
            test.iter().enumerate().map(|(i, c)| {
                let ctx = &ctx;
                async move { (i, jury::decide(ctx, &c.req).await) }
            }),
        )
        .buffer_unordered(par)
        .collect()
        .await;
        for (i, r) in results {
            match r {
                Ok((resp, _)) => {
                    score_response(&mut jury_stats, i, &resp, &test[i].expected);
                    jury_resps[i] = Some(resp);
                }
                Err(e) => eprintln!("  jury test {}/{} failed: {e}", i + 1, test.len()),
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
        let results: Vec<_> = stream::iter(
            test.iter().enumerate().map(|(i, c)| {
                let ctx = &ctx;
                async move { (i, jury::decide(ctx, &c.req).await) }
            }),
        )
        .buffer_unordered(par)
        .collect()
        .await;
        for (i, r) in results {
            match r {
                Ok((resp, _)) => score_response(&mut mem_stats, i, &resp, &test[i].expected),
                Err(e) => eprintln!("  mem test {}/{} failed: {e}", i + 1, test.len()),
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
        let results: Vec<_> = stream::iter(
            test.iter().enumerate().map(|(i, c)| {
                let ctx = &ctx;
                async move {
                    let hung: Vec<String> = c.req.questions.keys().cloned().collect();
                    let memory_block = match &ctx.store {
                        Some(s) => {
                            crate::memory::retrieve::retrieve(s, &c.req, &ctx.config.memory, None)
                                .map(|r| r.block)
                                .unwrap_or_default()
                        }
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
            }),
        )
        .buffer_unordered(par)
        .collect::<Vec<_>>()
        .await;
        for (i, call, usage) in results {
            note_calls(&mut judge_stats, &usage.model, 1);
            judge_stats.walls.push(usage.ms);
            match call {
                Some(call) => score_judge(&mut judge_stats, i, &call, &test[i].expected),
                None => eprintln!(
                    "  judge test {}/{} failed: {}",
                    i + 1,
                    test.len(),
                    usage.error.unwrap_or_default()
                ),
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
                        let memory_block = match &ctx.store {
                            Some(s) => crate::memory::retrieve::retrieve(
                                s,
                                &c.req,
                                &ctx.config.memory,
                                None,
                            )
                            .map(|r| r.block)
                            .unwrap_or_default(),
                            None => String::new(),
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
                        (i, call, usage)
                    }
                }),
        )
        .buffer_unordered(par)
        .collect::<Vec<_>>()
        .await;
        for (i, call, usage) in results {
            note_calls(&mut informed_stats, &usage.model, 1);
            informed_stats.walls.push(usage.ms);
            match call {
                Some(call) => score_judge(&mut informed_stats, i, &call, &test[i].expected),
                None => eprintln!(
                    "  informed test {}/{} failed: {}",
                    i + 1,
                    test.len(),
                    usage.error.unwrap_or_default()
                ),
            }
        }
    }

    // Go/no-go.
    let (ja, ma, ga) = (jury_stats.accuracy(), mem_stats.accuracy(), judge_stats.accuracy());
    let gap = ga - ja;
    let closed = if gap > 0.0 { (ma - ja) / gap } else { 0.0 };
    let hung_drop = if jury_stats.hung_rate() > 0.0 {
        (jury_stats.hung_rate() - mem_stats.hung_rate()) / jury_stats.hung_rate()
    } else {
        0.0
    };
    let go = closed >= 0.5 || (hung_drop >= 0.3 && ma >= ja - 0.001);

    let report_cfg = Config::load(base, config_path)?;
    let report = serde_json::json!({
        "label": label,
        "cases": n,
        "train": train.len(),
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
            "hung_rate_drop": hung_drop,
            "pass": go,
        }
    });
    let body = serde_json::to_string_pretty(&report).map_err(|e| Error::Memory(e.to_string()))?;
    crate::util::write_atomic(report_path, body.as_bytes())?;
    eprintln!("eval: go={} — wrote {}", go, report_path.display());
    println!("{body}");
    Ok(())
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
        "correct": s.correct,
        "accuracy": s.accuracy(),
        "hung_rate": s.hung_rate(),
        "calls": s.calls,
        "calls_by_backend": s.calls_by_backend,
        "est_cost_usd": s.est_cost_usd,
        "wall_ms_mean": mean,
        "wall_ms_p95": p95,
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
        use crate::response::{DecidedBy, JurorUsage, JudgeUsage, MemoryUse, Response, Usage};
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
