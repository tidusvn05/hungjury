//! One juror ballot: spawn → extract → validate → retry-with-feedback.
//!
//! Retries happen only on parse/validation failure — a `Timeout`/`Backend`
//! error is not retried (it would just burn another timeout's worth of
//! wall-clock). A juror that never produces a valid ballot is excluded
//! from the vote and reported in `usage`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use crate::backend::{AgentBackend, AgentRequest, TokenUsage, extract_json, tail};
use crate::error::Error;
use crate::question::{Ballot, Question, validate_ballot};
use crate::quota::{CallRecord, Quota, now_rfc3339};

/// A juror's validated ballot across all questions.
pub type BallotMap = BTreeMap<String, Ballot>;

/// The outcome of one `(juror, sample)` call, for voting + `usage`.
pub struct JurorRun {
    /// `"<backend>:<model>"`.
    pub juror: String,
    /// Sample index.
    pub sample: u32,
    /// `ok` | `timeout` | `error` | `invalid`.
    pub status: &'static str,
    /// Wall ms including retries.
    pub ms: u64,
    /// Attempts beyond the first.
    pub retries: u32,
    /// Validated ballots (on `ok`).
    pub ballots: Option<BallotMap>,
    /// `_why` map (only with `--explain`).
    pub why: Option<BTreeMap<String, String>>,
    /// Failure detail.
    pub error: Option<String>,
    /// Token usage when reported.
    pub usage: Option<TokenUsage>,
}

/// Shared call loop: quota → spawn → parse → retry-on-invalid.
/// `parse` maps raw response text to the ballot payload (`T` =
/// `ValidatedBallot` for single prompts, per-item ballots for packed).
/// Retries happen only on parse/validation failure — a `Timeout`/`Backend`
/// error is not retried (it would just burn another timeout's worth of
/// wall-clock).
async fn call_loop<T>(
    backend: &Arc<dyn AgentBackend>,
    mut req: AgentRequest,
    retry_attempts: u32,
    quota: &Quota,
    parse: impl Fn(&str) -> crate::error::Result<T>,
) -> (
    &'static str,
    u32,
    Option<T>,
    Option<String>,
    Option<TokenUsage>,
) {
    let base_prompt = req.prompt.clone();
    let mut feedback = String::new();
    let mut last_status = "error";
    let mut last_err = String::from("no attempts");
    let mut retries = 0u32;

    for attempt in 0..=retry_attempts {
        if attempt > 0 {
            retries += 1;
        }
        if let Err(e) = quota.consume().await {
            return ("error", retries, None, Some(e.to_string()), None);
        }
        req.prompt = format!("{base_prompt}{feedback}");
        let t0 = Instant::now();
        let response = backend
            .run(AgentRequest {
                prompt: req.prompt.clone(),
                system_prompt: req.system_prompt.clone(),
                model: req.model.clone(),
                cwd: req.cwd.clone(),
                tools: req.tools,
                timeout: req.timeout,
                agent: req.agent.clone(),
                json_schema: req.json_schema.clone(),
            })
            .await;
        let (status, err, usage) = match response {
            Ok(r) => match parse(&r.text) {
                Ok(parsed) => {
                    quota
                        .record(CallRecord {
                            ts: now_rfc3339(),
                            agent: &req.agent,
                            backend: backend.kind().as_str(),
                            model: req.model.as_deref().unwrap_or(""),
                            prompt_chars: req.prompt.len(),
                            secs: t0.elapsed().as_secs_f64(),
                            status: "ok",
                            input_tokens: r.usage.as_ref().map(|u| u.input),
                            output_tokens: r.usage.as_ref().map(|u| u.output),
                        })
                        .await;
                    return ("ok", retries, Some(parsed), None, r.usage);
                }
                Err(e) => ("invalid", e, r.usage),
            },
            Err(e) => match e {
                Error::Timeout { .. } => ("timeout", e, None),
                _ => ("error", e, None),
            },
        };
        quota
            .record(CallRecord {
                ts: now_rfc3339(),
                agent: &req.agent,
                backend: backend.kind().as_str(),
                model: req.model.as_deref().unwrap_or(""),
                prompt_chars: req.prompt.len(),
                secs: t0.elapsed().as_secs_f64(),
                status,
                input_tokens: usage.as_ref().map(|u| u.input),
                output_tokens: usage.as_ref().map(|u| u.output),
            })
            .await;
        last_status = status;
        last_err = err.to_string();
        if status != "invalid" {
            break;
        }
        feedback = format!(
            "\n\n**RETRY**: Your previous response failed: {last_err}. \
             Correct it and return ONLY the required JSON object."
        );
    }
    (last_status, retries, None, Some(last_err), None)
}

/// `juror:<model>#<sample>` → bare model string + sample index; stats
/// and vote weights key on the model, not the sample instance.
fn split_agent(agent: &str) -> (String, u32) {
    let stripped = agent.trim_start_matches("juror:");
    match stripped.rsplit_once('#') {
        Some((j, s)) => (j.to_string(), s.parse::<u32>().unwrap_or(0)),
        None => (stripped.to_string(), 0),
    }
}

/// Run one juror ballot. `req` carries the fully-rendered prompt and
/// request parameters; retries append a `**RETRY**` feedback block.
/// A juror that never produces a valid ballot is excluded from the vote
/// and reported in `usage`.
pub async fn run_juror(
    backend: &Arc<dyn AgentBackend>,
    req: AgentRequest,
    questions: &BTreeMap<String, Question>,
    explain: bool,
    retry_attempts: u32,
    quota: &Quota,
) -> JurorRun {
    let (juror, sample) = split_agent(&req.agent);
    let started = Instant::now();
    let agent = req.agent.clone();
    let (status, retries, parsed, error, usage) =
        call_loop(backend, req, retry_attempts, quota, move |text| {
            parse_ballot(text, questions, explain, &agent)
        })
        .await;
    let mut run = finish(juror, sample, status, started, retries, error, usage);
    if let Some((ballots, why)) = parsed {
        run.ballots = Some(ballots);
        run.why = why;
    }
    run
}

/// The outcome of one packed juror call: one prompt, N item ballots.
/// `item_ballots[i]` is `None` when the juror omitted or mangled that
/// item's answer object — a missing item is no ballot, not a failed call.
pub struct PackedJurorRun {
    /// `"<backend>:<model>"`.
    pub juror: String,
    /// Sample index.
    pub sample: u32,
    /// `ok` | `timeout` | `error` | `invalid`.
    pub status: &'static str,
    /// Wall ms including retries.
    pub ms: u64,
    /// Attempts beyond the first.
    pub retries: u32,
    /// Per-item ballots, aligned with the pack's item order.
    pub item_ballots: Option<Vec<Option<BallotMap>>>,
    /// Failure detail.
    pub error: Option<String>,
    /// Token usage when reported.
    pub usage: Option<TokenUsage>,
}

/// Run one packed juror call answering `item_ids` in a single response.
pub async fn run_juror_packed(
    backend: &Arc<dyn AgentBackend>,
    req: AgentRequest,
    questions: &BTreeMap<String, Question>,
    item_ids: &[String],
    explain: bool,
    retry_attempts: u32,
    quota: &Quota,
) -> PackedJurorRun {
    let (juror, sample) = split_agent(&req.agent);
    let started = Instant::now();
    let agent = req.agent.clone();
    let (status, retries, parsed, error, usage) =
        call_loop(backend, req, retry_attempts, quota, move |text| {
            parse_packed_ballot(text, questions, item_ids, explain, &agent)
        })
        .await;
    PackedJurorRun {
        juror,
        sample,
        status,
        ms: started.elapsed().as_millis() as u64,
        retries,
        item_ballots: parsed,
        error,
        usage,
    }
}

fn finish(
    juror: String,
    sample: u32,
    status: &'static str,
    started: Instant,
    retries: u32,
    error: Option<String>,
    usage: Option<TokenUsage>,
) -> JurorRun {
    JurorRun {
        juror,
        sample,
        status,
        ms: started.elapsed().as_millis() as u64,
        retries,
        ballots: None,
        why: None,
        error,
        usage,
    }
}

/// `extract_json` → `validate_ballot`.
fn parse_ballot(
    text: &str,
    questions: &BTreeMap<String, Question>,
    explain: bool,
    agent: &str,
) -> crate::error::Result<(BallotMap, Option<BTreeMap<String, String>>)> {
    let v = extract_json(text, agent).map_err(|e| Error::Parse {
        agent: agent.to_string(),
        message: format!("{} — raw: {}", e, tail(text, 200)),
    })?;
    validate_ballot(questions, &v, explain)
}

/// `extract_json` → per-item `validate_ballot`. Per-item tolerant: an
/// item whose answer object is missing or invalid contributes `None`
/// (no ballot) — one bad item must not void the other N−1 answers.
/// Only a top-level parse failure (not a JSON object at all) errors,
/// which is what drives the retry-with-feedback path.
fn parse_packed_ballot(
    text: &str,
    questions: &BTreeMap<String, Question>,
    item_ids: &[String],
    explain: bool,
    agent: &str,
) -> crate::error::Result<Vec<Option<BallotMap>>> {
    let v = extract_json(text, agent).map_err(|e| Error::Parse {
        agent: agent.to_string(),
        message: format!("{} — raw: {}", e, tail(text, 200)),
    })?;
    let obj = v.as_object().ok_or_else(|| Error::Validation {
        agent: agent.to_string(),
        message: format!(
            "expected {{item_id: answers}} object, got {}",
            tail(&v.to_string(), 200)
        ),
    })?;
    Ok(item_ids
        .iter()
        .map(|id| {
            obj.get(id)
                .and_then(|iv| validate_ballot(questions, iv, explain).ok())
                .map(|(ballots, _why)| ballots)
        })
        .collect())
}
