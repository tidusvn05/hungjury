//! The judge: one high-tier call that answers every question, returns a
//! rationale, and distills rulings/facts into memory.
//!
//! Split into two steps so callers choose which keys become precedents:
//! [`judge_call`] renders the prompt, calls the CLI, validates output;
//! [`commit_judge`] writes rulings/facts/juror_stats plus one precedent
//! per key the caller marks (hung keys for `decide`, disagreed keys for
//! `learn --audit`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::backend::{
    AgentBackend, AgentRequest, BackendKind, ToolPolicy, extract_json,
};
use crate::error::{Error, Result};
use crate::jury::DecideCtx;
use crate::jury::{questions_render, schema_block_text, state_render, workspace_render};
use crate::memory::store::{Kind, NewEntry, Source, Store};
use crate::memory::workspace as ws;
use crate::question::{Ballot, Question, validate_ballot};
use crate::request::Request;
use crate::response::{AnswerOut, JudgeUsage, JudgeVerdict};

/// A judge's verdict for one question (ballot + rationale + serde form).
#[derive(Debug, Clone)]
pub struct JudgeOut {
    /// Typed ballot.
    pub ballot: Ballot,
    /// Serializable form for `answers.*.judge`.
    pub verdict: JudgeVerdict,
}

/// The judge output schema: juror answers + rationale + rulings + facts.
fn judge_schema(questions: &BTreeMap<String, Question>) -> serde_json::Value {
    let answers = crate::question::ballot_schema(questions, false);
    let rationale_props: serde_json::Map<String, serde_json::Value> = questions
        .keys()
        .map(|k| (k.clone(), serde_json::json!({"type": "string"})))
        .collect();
    let keys: Vec<serde_json::Value> = questions
        .keys()
        .map(|k| serde_json::Value::String(k.clone()))
        .collect();
    serde_json::json!({
        "type": "object",
        "properties": {
            "answers": answers,
            "rationale": {
                "type": "object",
                "properties": rationale_props,
                "required": keys,
                "additionalProperties": false
            },
            "rulings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {"type": "string"},
                        "text": {"type": "string"}
                    },
                    "required": ["question", "text"],
                    "additionalProperties": false
                }
            },
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "evidence": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["text", "evidence"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["answers", "rationale", "rulings", "facts"],
        "additionalProperties": false
    })
}

/// The validated result of a judge call.
pub struct JudgeCall {
    /// `key → verdict`.
    pub judged: BTreeMap<String, JudgeOut>,
    /// Raw judge JSON (rulings/facts live here).
    pub raw: serde_json::Value,
}

/// Render + call + validate the judge once (one retry on bad output).
/// Returns `Err` for a failed call and `Ok(None)` for invalid output —
/// both surfaces land in `usage`.
#[allow(clippy::too_many_arguments)]
pub async fn judge_call(
    ctx: &DecideCtx,
    req: &Request,
    memory_block: &str,
    juror_ballots: &[(String, BTreeMap<String, Ballot>)],
    answers: &BTreeMap<String, AnswerOut>,
    hung: &[String],
    tag: &str,
    ws_path: Option<&Path>,
) -> (Option<JudgeCall>, JudgeUsage) {
    let started = Instant::now();
    let judge_str = ctx.config.judge.clone();
    let mut usage = JudgeUsage {
        model: judge_str.clone(),
        status: "error".to_string(),
        ms: 0,
        wrote: vec![],
        error: None,
    };
    macro_rules! bail {
        ($status:expr, $msg:expr) => {{
            usage.status = $status.to_string();
            usage.error = Some($msg);
            usage.ms = started.elapsed().as_millis() as u64;
            return (None, usage);
        }};
    }

    let Ok((kind, model)) = BackendKind::parse(&judge_str) else {
        bail!("error", format!("bad judge model '{judge_str}'"));
    };
    let backend: Arc<dyn AgentBackend> = ctx.backend(kind);

    let schema = judge_schema(&req.questions);
    let prompt = match render_judge_prompt(
        ctx,
        req,
        memory_block,
        juror_ballots,
        answers,
        hung,
        &schema,
        tag,
    ) {
        Ok(p) => p,
        Err(e) => bail!("error", e.to_string()),
    };
    let workspace_mode = ws_path.is_some();
    let base = AgentRequest {
        prompt,
        system_prompt: Some(
            "You are the judge in a decision system. Decide every question, then \
             distill what the case teaches. Output ONLY a single JSON object."
                .to_string(),
        ),
        model,
        cwd: ws_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| ctx.empty_cwd.path().to_path_buf()),
        tools: if workspace_mode {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::None
        },
        timeout: ctx.config.judge_timeout(),
        agent: format!("judge:{judge_str}"),
        json_schema: Some(schema),
    };

    let mut feedback = String::new();
    let mut parsed: Option<serde_json::Value> = None;
    for _attempt in 0..=ctx.config.limits.retry_attempts {
        if let Err(e) = ctx.quota.consume().await {
            bail!("error", e.to_string());
        }
        let r2 = AgentRequest {
            prompt: format!("{}{}", base.prompt, feedback),
            system_prompt: base.system_prompt.clone(),
            model: base.model.clone(),
            cwd: base.cwd.clone(),
            tools: base.tools,
            timeout: base.timeout,
            agent: base.agent.clone(),
            json_schema: base.json_schema.clone(),
        };
        let t0 = std::time::Instant::now();
        let response = backend.run(r2).await;
        {
            let (status, usage2) = match &response {
                Ok(r) => ("ok", r.usage.as_ref()),
                Err(Error::Timeout { .. }) => ("timeout", None),
                Err(_) => ("error", None),
            };
            ctx.quota
                .record(crate::quota::CallRecord {
                    ts: crate::quota::now_rfc3339(),
                    agent: &base.agent,
                    backend: kind.as_str(),
                    model: base.model.as_deref().unwrap_or(""),
                    prompt_chars: base.prompt.len(),
                    secs: t0.elapsed().as_secs_f64(),
                    status,
                    input_tokens: usage2.map(|u| u.input),
                    output_tokens: usage2.map(|u| u.output),
                })
                .await;
        }
        match response {
            Ok(r) => match extract_json(&r.text, "judge") {
                Ok(v) => match validate_ballot(&req.questions, &v["answers"], false) {
                    Ok(_) => {
                        parsed = Some(v);
                        break;
                    }
                    Err(e) => {
                        usage.status = "invalid".to_string();
                        usage.error = Some(e.to_string());
                        feedback = retry_feedback(&e);
                    }
                },
                Err(e) => {
                    usage.status = "invalid".to_string();
                    usage.error = Some(e.to_string());
                    feedback = retry_feedback(&e);
                }
            },
            Err(Error::Timeout { secs, .. }) => {
                bail!("timeout", format!("judge timed out after {secs:?}"));
            }
            Err(e) => {
                bail!("error", e.to_string());
            }
        }
    }
    let Some(v) = parsed else {
        usage.ms = started.elapsed().as_millis() as u64;
        return (None, usage);
    };

    // Verdicts per question.
    let (ballots, _) =
        validate_ballot(&req.questions, &v["answers"], false).unwrap_or_default();
    let rationale = &v["rationale"];
    let mut judged = BTreeMap::new();
    for key in req.questions.keys() {
        if let Some(b) = ballots.get(key) {
            judged.insert(
                key.clone(),
                JudgeOut {
                    ballot: b.clone(),
                    verdict: JudgeVerdict {
                        choice: match b {
                            Ballot::Choice(c) => Some(c.clone()),
                            _ => None,
                        },
                        score: match b {
                            Ballot::Score(s) => Some(*s as i64),
                            _ => None,
                        },
                        noul: match b {
                            Ballot::Noul(x) => Some(*x),
                            _ => None,
                        },
                        rationale: rationale[key].as_str().map(str::to_string),
                    },
                },
            );
        }
    }
    usage.status = "ok".to_string();
    usage.ms = started.elapsed().as_millis() as u64;
    (Some(JudgeCall { judged, raw: v }), usage)
}

fn retry_feedback(e: &Error) -> String {
    format!(
        "\n\n**RETRY**: Your previous response failed: {e}. \
         Correct it and return ONLY the required JSON object."
    )
}

/// Render `judge.md`.
#[allow(clippy::too_many_arguments)]
fn render_judge_prompt(
    ctx: &DecideCtx,
    req: &Request,
    memory_block: &str,
    juror_ballots: &[(String, BTreeMap<String, Ballot>)],
    answers: &BTreeMap<String, AnswerOut>,
    hung: &[String],
    schema: &serde_json::Value,
    tag: &str,
) -> Result<String> {
    let tmpl = ctx.prompts.load("judge.md")?;
    let mut vars: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    vars.insert("schema_block", schema_block_text(schema));
    vars.insert("questions_block", questions_render(req));
    vars.insert("memory_block", memory_block.to_string());
    vars.insert("workspace_block", workspace_render(req));
    vars.insert(
        "ballots_block",
        crate::jury::vote::ballot_table(req.questions.iter(), juror_ballots, answers, hung),
    );
    vars.insert("state_tag", format!("state-{tag}"));
    vars.insert("state", state_render(req));
    Ok(crate::prompt::render(&tmpl, &vars))
}

/// Persist rulings/facts/juror_stats + one precedent per `precedent_keys`.
/// No-op under `--memory-readonly`. Returns the ids written.
#[allow(clippy::too_many_arguments)]
pub fn commit_judge(
    store: &Store,
    readonly: bool,
    req: &Request,
    call: &JudgeCall,
    precedent_keys: &[String],
    juror_ballots: &[(String, BTreeMap<String, Ballot>)],
    repo_id: Option<&str>,
    ws_path: Option<&Path>,
    judge_str: &str,
) -> Result<Vec<String>> {
    if readonly {
        return Ok(vec![]);
    }
    let origin = store.machine_id().ok();
    let mut wrote = Vec::new();
    let judge_json = &call.raw;
    let judged = &call.judged;

    // Rulings: question key → qid scope; reject >300 chars / unknown keys.
    if let Some(rs) = judge_json["rulings"].as_array() {
        for r in rs {
            let key = r["question"].as_str().unwrap_or("");
            let text = r["text"].as_str().unwrap_or("").trim();
            let Some(q) = req.questions.get(key) else {
                continue;
            };
            if text.is_empty() || text.len() > 300 {
                continue;
            }
            let e = NewEntry {
                kind: Kind::Ruling,
                scope: crate::memory::store::q_scope(&q.qid()),
                body: serde_json::json!({
                    "text": text,
                    "question": {"type": q.kind_str(), "instructions": q.instructions()},
                }),
                text: format!("{} {text}", q.instructions()),
                source: Source::Judge,
                trust: Source::Judge.base_trust(),
                author: Some(judge_str.to_string()),
                origin: origin.clone(),
            };
            if let Ok((id, true)) = store.insert(&e) {
                wrote.push(id);
            }
        }
    }

    // Precedents: one per marked question the judge answered.
    let (excerpt, digest) = match &req.state {
        crate::request::State::Text(t) => (
            t.chars().take(400).collect::<String>(),
            crate::util::sha256_str(t),
        ),
        crate::request::State::Workspace { path, hint } => (
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
    for key in precedent_keys {
        let (Some(q), Some(j)) = (req.questions.get(key), judged.get(key)) else {
            continue;
        };
        let e = NewEntry {
            kind: Kind::Precedent,
            scope: crate::memory::store::q_scope(&q.qid()),
            body: serde_json::json!({
                "state_excerpt": excerpt,
                "state_digest": digest,
                "verdict": j.ballot.to_json(),
                "rationale": j.verdict.rationale.clone().unwrap_or_default(),
            }),
            text: format!("{excerpt} {}", j.verdict.rationale.as_deref().unwrap_or("")),
            source: Source::Judge,
            trust: Source::Judge.base_trust(),
            author: Some(judge_str.to_string()),
            origin: origin.clone(),
        };
        if let Ok((id, true)) = store.insert(&e) {
            wrote.push(id);
        }
    }

    // Facts (workspace mode only): evidence files get hashed now.
    if let (Some(repo), Some(path)) = (repo_id, ws_path)
        && let Some(fs) = judge_json["facts"].as_array()
    {
        let commit = ws::workspace_stamp(path)
            .map(|s| s.split(':').next().unwrap_or("").to_string())
            .unwrap_or_default();
        for f in fs {
            let text = f["text"].as_str().unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            let paths: Vec<std::path::PathBuf> = f["evidence"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|rel| path.join(rel))
                        .collect()
                })
                .unwrap_or_default();
            let evidence = ws::evidence_for(path, &paths);
            let e = NewEntry {
                kind: Kind::Fact,
                scope: crate::memory::store::ws_scope(repo),
                body: serde_json::json!({
                    "text": text,
                    "evidence": evidence,
                    "commit": commit,
                }),
                text: text.to_string(),
                source: Source::Judge,
                trust: Source::Judge.base_trust(),
                author: Some(judge_str.to_string()),
                origin: origin.clone(),
            };
            if let Ok((id, true)) = store.insert(&e) {
                wrote.push(id);
            }
        }
    }

    // juror_stats: judge answers are ground truth for every question.
    for (key, q) in &req.questions {
        let Some(j) = judged.get(key) else {
            continue;
        };
        for (juror_name, ballots) in juror_ballots {
            if let Some(b) = ballots.get(key) {
                let _ = store.juror_stats_update(juror_name, &q.qid(), *b == j.ballot);
            }
        }
    }
    Ok(wrote)
}
