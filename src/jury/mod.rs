//! The `decide` orchestration: cache → retrieve → jurors in parallel →
//! vote → escalate → record.

pub mod juror;
pub mod vote;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::sync::Semaphore;

use crate::backend::{
    AgentBackend, AgentRequest, BackendKind, ToolPolicy, for_kind,
};
use crate::cache::Cache;
use crate::config::{Config, Escalate};
use crate::error::{Error, Result};
use crate::judge;
use crate::memory::retrieve::{self, Retrieval};
use crate::memory::store::Store;
use crate::memory::workspace;
use crate::prompt::PromptLoader;
use crate::question::{Ballot, ballot_schema};
use crate::quota::Quota;
use crate::request::{Request, State};
use crate::response::{
    AnswerOut, DecidedBy, JudgeUsage, JurorUsage, Response, Usage,
};

/// Everything `decide` needs, built once per invocation.
pub struct DecideCtx {
    /// Resolved config.
    pub config: Config,
    /// Backend overrides for tests (`mock:` etc.) — missing kinds fall
    /// back to `for_kind`.
    pub backends: HashMap<BackendKind, Arc<dyn AgentBackend>>,
    /// Prompt template loader.
    pub prompts: PromptLoader,
    /// Daily cap + calls.jsonl.
    pub quota: Quota,
    /// Decision cache.
    pub cache: Cache,
    /// Memory store — `None` when memory is disabled or the db failed to
    /// open (a broken db must not block decisions; doctor reports it).
    pub store: Option<Store>,
    /// Empty temp dir used as juror cwd for text states.
    pub empty_cwd: tempfile::TempDir,
    /// Concurrency bound over all juror calls.
    pub semaphore: Semaphore,
}

impl DecideCtx {
    /// Build from config; `backends` may inject mocks.
    pub fn new(config: Config, backends: Option<HashMap<BackendKind, Arc<dyn AgentBackend>>>) -> Result<Self> {
        let store = if config.memory.enabled || config.memory.readonly {
            match Store::open(&config.memory_db) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!(error = %e, "memory db unavailable, continuing without it");
                    None
                }
            }
        } else {
            None
        };
        let empty_cwd = tempfile::tempdir().map_err(|e| Error::io("tempdir", e))?;
        Ok(DecideCtx {
            quota: Quota::new(&config.data_dir, config.limits.daily_cap),
            cache: Cache::new(&config.data_dir, config.no_cache, config.refresh),
            prompts: PromptLoader::new(config.prompts_dir.clone()),
            semaphore: Semaphore::new(config.limits.max_concurrency),
            backends: backends.unwrap_or_default(),
            store,
            empty_cwd,
            config,
        })
    }

    /// Backend for a kind — injected override or a fresh CLI adapter.
    pub fn backend(&self, kind: BackendKind) -> Arc<dyn AgentBackend> {
        self.backends
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| for_kind(kind))
    }
}

/// Run one decision. Returns the response + process exit code
/// (`0` decided, `2` hung unresolved).
pub async fn decide(ctx: &DecideCtx, req: &Request) -> Result<(Response, i32)> {
    let started = Instant::now();
    let workspace_mode = req.state.is_workspace();
    let ws_path = match &req.state {
        State::Workspace { path, .. } => Some(path.clone()),
        State::Text(_) => None,
    };
    let repo_id = ws_path.as_deref().map(workspace::repo_id);

    // Scopes this request touches: q:<qid> per question (+ ws:<repo_id>).
    let mut scopes: Vec<String> = req
        .questions
        .values()
        .map(|q| crate::memory::store::q_scope(&q.qid()))
        .collect();
    if let Some(r) = &repo_id {
        scopes.push(crate::memory::store::ws_scope(r));
    }

    // 0. Exact-match cache.
    let memory_epoch = match &ctx.store {
        Some(s) => {
            let ids = s.active_ids(&scopes).unwrap_or_default();
            crate::util::sha256_str(&ids.join(","))
        }
        None => String::new(),
    };
    let ws_stamp = ws_path
        .as_deref()
        .and_then(workspace::workspace_stamp);
    if workspace_mode && ws_stamp.is_none() {
        tracing::info!("non-git workspace: decision will not be cached");
    }
    let jury_config = serde_json::json!({
        "jurors": ctx.config.jurors,
        "samples": ctx.config.samples,
        "judge": ctx.config.judge,
        "threshold": ctx.config.hung_threshold,
        "min_quorum": ctx.config.min_quorum,
        "tools": workspace_mode,
        "explain": ctx.config.explain,
        "memory": ctx.config.memory.enabled,
        // Policy text changes prompts — fold its hash into the cache key
        // so stale verdicts can't hit after a policy edit.
        "policy": ctx.config.policy.as_deref().map(crate::util::sha256_str),
    })
    .to_string();
    let cache_key = Cache::key(
        &req.canonical(),
        &jury_config,
        &memory_epoch,
        ws_stamp.as_deref(),
    );
    if (ws_stamp.is_some() || !workspace_mode)
        && let Some(hit) = ctx.cache.get(&cache_key)
            && let Ok(mut resp) =
                serde_json::from_value::<Response>(hit.response.clone())
        {
            resp.decided_by = DecidedBy::Cache;
            let code = resp.exit_code();
            return Ok((resp, code));
        }

    // 1. Retrieve memory (in-process, before any spawn).
    let retrieval = retrieve_memory(ctx, req, repo_id.as_deref(), ws_path.as_deref())?;

    // 2. Render prompts.
    let schema = ballot_schema(&req.questions, ctx.config.explain);
    let tag = crate::util::nonce();
    let prompt_with_mem = render_juror_prompt(ctx, req, &retrieval.block, &schema, &tag)?;
    let prompt_blind = if ctx.config.memory.blind_juror {
        Some(render_juror_prompt(ctx, req, "", &schema, &tag)?)
    } else {
        None
    };
    let system_prompt = system_prompt(ctx);

    // 3. Spawn juror × sample in parallel.
    let mut futures = FuturesUnordered::new();
    let n_jurors = ctx.config.jurors.len();
    for (i, model_str) in ctx.config.jurors.iter().enumerate() {
        let (kind, model) = BackendKind::parse(model_str)?;
        let backend = ctx.backend(kind);
        for sample in 0..ctx.config.samples {
            let blind = prompt_blind.is_some() && i == n_jurors - 1;
            let prompt = if blind {
                prompt_blind.clone().unwrap_or_default()
            } else {
                prompt_with_mem.clone()
            };
            let cwd = ws_path
                .clone()
                .unwrap_or_else(|| ctx.empty_cwd.path().to_path_buf());
            let agent = format!("juror:{model_str}#{sample}");
            let req_inner = AgentRequest {
                prompt,
                system_prompt: system_prompt.clone(),
                model: model.clone(),
                cwd,
                tools: if workspace_mode {
                    ToolPolicy::ReadOnly
                } else {
                    ToolPolicy::None
                },
                timeout: ctx.config.juror_timeout(workspace_mode),
                agent,
                json_schema: Some(schema.clone()),
            };
            let questions = req.questions.clone();
            let explain = ctx.config.explain;
            let retries = ctx.config.limits.retry_attempts;
            let quota = &ctx.quota;
            let sem = &ctx.semaphore;
            let backend = backend.clone();
            futures.push(async move {
                let _permit = sem.acquire().await.ok();
                juror::run_juror(&backend, req_inner, &questions, explain, retries, quota).await
            });
        }
    }
    let mut runs = Vec::new();
    while let Some(r) = futures.next().await {
        runs.push(r);
    }

    // 4. Vote.
    let ok_ballots: Vec<(&str, &juror::BallotMap)> = runs
        .iter()
        .filter(|r| r.status == "ok")
        .filter_map(|r| r.ballots.as_ref().map(|b| (r.juror.as_str(), b)))
        .collect();
    if ok_ballots.is_empty() {
        return Err(Error::NoValidJuror);
    }
    let mut answers: BTreeMap<String, AnswerOut> = BTreeMap::new();
    let mut hung: Vec<String> = Vec::new();
    for (key, q) in &req.questions {
        let mut votes: Vec<(f64, Ballot)> = Vec::new();
        for (juror_name, ballots) in &ok_ballots {
            if let Some(b) = ballots.get(key) {
                let w = ctx
                    .store
                    .as_ref()
                    .map(|s| s.juror_weight(juror_name, &q.qid()).unwrap_or(1.0))
                    .unwrap_or(1.0);
                votes.push((w, b.clone()));
            }
        }
        let out = vote::tally(q, &votes);
        // Below quorum (too few valid ballots — e.g. jurors timed out) the
        // question is hung even though `confidence` is `None`: a lone
        // surviving juror must not silently decide.
        if votes.len() < ctx.config.min_quorum
            || vote::is_hung(&out, ctx.config.hung_threshold)
        {
            hung.push(key.clone());
        }
        answers.insert(key.clone(), out);
    }

    // 5. Escalation.
    let juror_ballots: Vec<(String, BTreeMap<String, Ballot>)> = runs
        .iter()
        .filter_map(|r| {
            r.ballots
                .as_ref()
                .map(|b| (r.juror.clone(), b.clone()))
        })
        .collect();
    let mut decided_by = DecidedBy::Jury;
    let mut judge_usage: Option<JudgeUsage> = None;
    if !hung.is_empty() {
        match ctx.config.escalate {
            Escalate::Sync => {
                let (call, mut ju) = judge::judge_call(
                    ctx,
                    req,
                    &retrieval.block,
                    &juror_ballots,
                    &answers,
                    &hung,
                    &tag,
                    ws_path.as_deref(),
                )
                .await;
                if let Some(call) = call {
                    if let Some(store) = &ctx.store {
                        ju.wrote = judge::commit_judge(
                            store,
                            ctx.config.memory.readonly,
                            req,
                            &call,
                            &hung,
                            &juror_ballots,
                            &answers,
                            ctx.config.hung_threshold,
                            ctx.config.memory.provisional_trust,
                            repo_id.as_deref(),
                            ws_path.as_deref(),
                            &ctx.config.judge,
                        )
                        .unwrap_or_default();
                    }
                    for (key, verdict) in &call.judged {
                        if let Some(a) = answers.get_mut(key) {
                            apply_judge(a, verdict);
                        }
                    }
                    decided_by = DecidedBy::Judge;
                    hung.clear();
                }
                judge_usage = Some(ju);
            }
            Escalate::Queue => { /* enqueued after the response exists */ }
            Escalate::Off => {}
        }
    }

    // 6. Response, decisions log, cache.
    let response = Response {
        id: crate::util::new_decision_id(),
        decided_by,
        answers,
        hung: hung.clone(),
        memory: retrieval.used.clone(),
        usage: Usage {
            wall_ms: started.elapsed().as_millis() as u64,
            jurors: runs.iter().map(juror_usage).collect(),
            judge: judge_usage.clone(),
            est_cost_usd: est_cost(ctx, &runs, judge_usage.as_ref()),
        },
    };
    if let Some(store) = &ctx.store
        && !ctx.config.memory.readonly
    {
        let req_json = serde_json::to_value(req_canonical_json(req)).unwrap_or_default();
        let resp_json = serde_json::to_value(&response).unwrap_or_default();
        let _ = store.record_decision(
            &response.id,
            &crate::util::sha256_str(&req.canonical()),
            &req_json,
            &resp_json,
            decided_by_str(decided_by),
        );
        let _ = store.mark_used(&retrieval.entry_ids);
        if matches!(ctx.config.escalate, Escalate::Queue) && !hung.is_empty() {
            let _ = store.enqueue(&response.id, "hung jury");
        }
    }
    if (ws_stamp.is_some() || !workspace_mode)
        && let Ok(v) = serde_json::to_value(&response) {
            let _ = ctx.cache.put(&cache_key, &v);
        }
    let code = response.exit_code();
    Ok((response, code))
}

/// Estimated USD cost of this decision from `[costs]` — `None` unless at
/// least one backend has a configured price.
fn est_cost(
    ctx: &DecideCtx,
    runs: &[juror::JurorRun],
    judge: Option<&JudgeUsage>,
) -> Option<f64> {
    if ctx.config.costs.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let mut any = false;
    for r in runs {
        if let Some(p) = ctx.config.cost_per_call(&r.juror) {
            total += p * (1.0 + r.retries as f64);
            any = true;
        }
    }
    if let Some(j) = judge
        && let Some(p) = ctx.config.cost_per_call(&j.model)
    {
        total += p;
        any = true;
    }
    any.then_some(total)
}

fn decided_by_str(d: DecidedBy) -> &'static str {
    match d {
        DecidedBy::Jury => "jury",
        DecidedBy::Judge => "judge",
        DecidedBy::Cache => "cache",
    }
}

/// Serialize the request for the decisions log (state text included for
/// text states; workspace → path + hint only).
fn req_canonical_json(req: &Request) -> serde_json::Value {
    let state = match &req.state {
        State::Text(t) => serde_json::json!(t),
        State::Workspace { path, hint } => serde_json::json!({
            "workspace": path.display().to_string(),
            "hint": hint,
        }),
    };
    serde_json::json!({
        "state": state,
        "questions": req.questions,
    })
}

/// Memory retrieval honoring `memory.enabled` / `readonly` / facts
/// verification. Returns an empty block when disabled.
fn retrieve_memory(
    ctx: &DecideCtx,
    req: &Request,
    repo_id: Option<&str>,
    ws_path: Option<&std::path::Path>,
) -> Result<Retrieval> {
    let empty = Retrieval::default();
    if !ctx.config.memory.enabled && !ctx.config.memory.readonly {
        return Ok(empty);
    }
    let Some(store) = &ctx.store else {
        return Ok(empty);
    };
    let facts = match (repo_id, ws_path) {
        (Some(r), Some(p)) => Some(workspace::verify_facts(store, p, r)?),
        _ => None,
    };
    retrieve::retrieve(store, req, &ctx.config.memory, facts)
}

/// The short system prompt — full contract lives in the user message.
fn system_prompt(_ctx: &DecideCtx) -> Option<String> {
    Some(
        "You are a juror in a decision jury. Answer the questions in the user \
         message about the state it contains. Output ONLY a single JSON \
         object — no prose, no markdown fences."
            .to_string(),
    )
}

/// The `{{policy_block}}` prompt section: a `## Domain policy` block when
/// `config.policy` is set, else empty. Shared by juror + judge prompts.
pub fn policy_block(ctx: &DecideCtx) -> String {
    ctx.config
        .policy
        .as_deref()
        .map(|p| {
            format!(
                "## Domain policy\n\n{p}\n\nApply these rules over your own defaults.\n\n"
            )
        })
        .unwrap_or_default()
}

/// Render the juror prompt via the template.
fn render_juror_prompt(
    ctx: &DecideCtx,
    req: &Request,
    memory_block: &str,
    schema: &serde_json::Value,
    tag: &str,
) -> Result<String> {
    let tmpl = ctx.prompts.load("juror.md")?;
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("schema_block", schema_block_text(schema));
    vars.insert("policy_block", policy_block(ctx));
    vars.insert("questions_block", questions_render(req));
    vars.insert("memory_block", memory_block.to_string());
    vars.insert("workspace_block", workspace_render(req));
    vars.insert("state_tag", format!("state-{tag}"));
    vars.insert("state", state_render(req));
    Ok(crate::prompt::render(&tmpl, &vars))
}

/// The JSON schema contract block embedded in prompts (devin needs it —
/// it has no schema flag; harmless for the others).
pub fn schema_block_text(schema: &serde_json::Value) -> String {
    format!(
        "Your response MUST be a single valid JSON object matching this JSON Schema:\n\
         ```json\n{}\n```\n\
         Do not include any text before or after the JSON. Do not wrap the JSON in \
         markdown code fences. Every `required` field must be present.",
        serde_json::to_string_pretty(schema).unwrap_or_default()
    )
}

/// `## Questions` body: every key, type, instructions, criteria.
pub fn questions_render(req: &Request) -> String {
    req.questions
        .iter()
        .map(|(k, q)| q.describe(k))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The workspace note (empty for text states).
pub fn workspace_render(req: &Request) -> String {
    match &req.state {
        State::Text(_) => String::new(),
        State::Workspace { hint, .. } => {
            let hint_line = hint
                .as_deref()
                .map(|h| format!("\nThe caller's hint: \"{h}\""))
                .unwrap_or_default();
            format!(
                "## Workspace\n\nThe repository is your current working directory. \
                 Explore it READ-ONLY — do not modify any files.{hint_line}\n\n"
            )
        }
    }
}

/// State text inlined in the prompt; workspace states inline the hint only
/// (the agent reads the repo itself).
pub fn state_render(req: &Request) -> String {
    match &req.state {
        State::Text(t) => t.clone(),
        State::Workspace { hint, .. } => format!(
            "(workspace state — see the Workspace section above){}",
            hint.as_deref()
                .map(|h| format!(" Hint: {h}"))
                .unwrap_or_default()
        ),
    }
}

/// Map a `JurorRun` to its `usage` record.
fn juror_usage(r: &juror::JurorRun) -> JurorUsage {
    JurorUsage {
        juror: r.juror.clone(),
        sample: r.sample,
        status: r.status.to_string(),
        ms: r.ms,
        retries: r.retries,
        answers: r.ballots.as_ref().map(|b| {
            b.iter()
                .map(|(k, v)| (k.clone(), v.to_json()))
                .collect()
        }),
        error: r.error.clone(),
        input_tokens: r.usage.as_ref().map(|u| u.input),
        output_tokens: r.usage.as_ref().map(|u| u.output),
    }
}

/// Apply a judge verdict to an answer: the top-level value follows the
/// judge; probabilities/confidence stay the jury's.
fn apply_judge(a: &mut AnswerOut, verdict: &crate::judge::JudgeOut) {
    match (&mut *a, &verdict.ballot) {
        (AnswerOut::Choice { choice, .. }, Ballot::Choice(c)) => *choice = c.clone(),
        (AnswerOut::Score { score, .. }, Ballot::Score(s)) => *score = *s as f64,
        (AnswerOut::Noul { noul, .. }, Ballot::Noul(b)) => *noul = if *b { 1.0 } else { 0.0 },
        _ => return,
    }
    a.set_judge(verdict.verdict.clone());
}
