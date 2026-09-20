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

/// Run one juror ballot. `base` carries the fully-rendered prompt and
/// request parameters; retries append a `**RETRY**` feedback block.
pub async fn run_juror(
    backend: &Arc<dyn AgentBackend>,
    mut req: AgentRequest,
    questions: &BTreeMap<String, Question>,
    explain: bool,
    retry_attempts: u32,
    quota: &Quota,
) -> JurorRun {
    // `juror:<model>#<sample>` → bare model string + sample index; stats
    // and vote weights key on the model, not the sample instance.
    let stripped = req.agent.trim_start_matches("juror:");
    let (juror, sample) = match stripped.rsplit_once('#') {
        Some((j, s)) => (j.to_string(), s.parse::<u32>().unwrap_or(0)),
        None => (stripped.to_string(), 0),
    };
    let started = Instant::now();
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
            return finish(
                juror,
                sample,
                "error",
                started,
                retries,
                Some(e.to_string()),
                None,
            );
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
            Ok(r) => match parse_ballot(&r.text, questions, explain, &req.agent) {
                Ok((ballots, why)) => {
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
                    let mut run = finish(juror, sample, "ok", started, retries, None, r.usage);
                    run.ballots = Some(ballots);
                    run.why = why;
                    return run;
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
        // Only schema problems get a retry — spawning again after a
        // backend failure buys nothing.
        if status != "invalid" {
            break;
        }
        feedback = format!(
            "\n\n**RETRY**: Your previous response failed: {last_err}. \
             Correct it and return ONLY the required JSON object."
        );
    }

    let mut run = finish(
        juror,
        sample,
        last_status,
        started,
        retries,
        Some(last_err),
        None,
    );
    run.retries = retries;
    run
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
