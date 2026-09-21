//! The `decide` orchestration: cache → retrieve → jurors in parallel →
//! vote → escalate → record.

pub mod juror;
pub mod vote;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::sync::Semaphore;

use crate::backend::{AgentBackend, AgentRequest, BackendKind, ToolPolicy, for_kind};
use crate::cache::Cache;
use crate::config::{Config, Escalate};
use crate::error::{Error, Result};
use crate::judge;
use crate::memory::retrieve::{self, Retrieval};
use crate::memory::store::Store;
use crate::memory::workspace;
use crate::prompt::PromptLoader;
use crate::question::{Ballot, ballot_schema, packed_ballot_schema};
use crate::quota::Quota;
use crate::request::{Request, State};
use crate::response::{AnswerOut, DecidedBy, JudgeUsage, JurorUsage, Response, Usage};

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
    pub fn new(
        config: Config,
        backends: Option<HashMap<BackendKind, Arc<dyn AgentBackend>>>,
    ) -> Result<Self> {
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

/// Jury config folded into cache keys — answers can differ between
/// packed and per-item contexts, so `pack` size is part of the key.
fn jury_config(ctx: &DecideCtx, workspace_mode: bool, tmpl: &str, pack: usize) -> String {
    serde_json::json!({
        "jurors": ctx.config.jurors,
        "samples": ctx.config.samples,
        "judge": ctx.config.judge,
        "threshold": ctx.config.hung_threshold,
        "min_quorum": ctx.config.min_quorum,
        "tools": workspace_mode,
        "explain": ctx.config.explain,
        "memory": ctx.config.memory.enabled,
        "pack": pack,
        // Policy + prompt text change answers — fold their hashes into
        // the cache key so edits can't hit stale verdicts.
        "policy": ctx.config.policy.as_deref().map(crate::util::sha256_str),
        "prompt": crate::util::sha256_str(tmpl),
    })
    .to_string()
}

/// Scopes a request touches: `q:<qid>` per question (+ `ws:<repo>`).
fn req_scopes(ctx: &DecideCtx, req: &Request, repo_id: Option<&str>) -> Vec<String> {
    let mut scopes: Vec<String> = req
        .questions
        .values()
        .map(|q| crate::memory::store::q_scope(ctx.config.namespace.as_deref(), &q.qid()))
        .collect();
    if let Some(r) = repo_id {
        scopes.push(crate::memory::store::ws_scope(
            ctx.config.namespace.as_deref(),
            r,
        ));
    }
    scopes
}

/// Memory epoch for cache keys: hash of active entry ids in scope.
fn memory_epoch(ctx: &DecideCtx, scopes: &[String]) -> String {
    match &ctx.store {
        Some(s) => {
            let ids = s.active_ids(scopes).unwrap_or_default();
            crate::util::sha256_str(&ids.join(","))
        }
        None => String::new(),
    }
}

/// Serve a cached response when present (marks decided_by/sources).
fn cached_response(ctx: &DecideCtx, cache_key: &str) -> Option<(Response, i32)> {
    let hit = ctx.cache.get(cache_key)?;
    let mut resp = serde_json::from_value::<Response>(hit.response.clone()).ok()?;
    resp.decided_by = DecidedBy::Cache;
    resp.sources
        .values_mut()
        .for_each(|s| *s = DecidedBy::Cache);
    let code = resp.exit_code();
    Some((resp, code))
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
    let scopes = req_scopes(ctx, req, repo_id.as_deref());
    let mem_epoch = memory_epoch(ctx, &scopes);
    let ws_stamp = ws_path.as_deref().and_then(workspace::workspace_stamp);
    if workspace_mode && ws_stamp.is_none() {
        tracing::info!("non-git workspace: decision will not be cached");
    }
    // Load the juror template once — its hash joins the cache key so a
    // prompt edit can't hit stale verdicts (same argument as `policy`).
    let juror_tmpl = ctx.prompts.load("juror.md")?;
    let cfg_key = jury_config(ctx, workspace_mode, &juror_tmpl, 1);
    let cache_key = Cache::key(&req.canonical(), &cfg_key, &mem_epoch, ws_stamp.as_deref());
    if (ws_stamp.is_some() || !workspace_mode)
        && let Some(hit) = cached_response(ctx, &cache_key)
    {
        return Ok(hit);
    }

    // 1. Retrieve memory (in-process, before any spawn).
    let retrieval = retrieve_memory(ctx, req, repo_id.as_deref(), ws_path.as_deref())?;

    // 2. Render prompts.
    let schema = ballot_schema(&req.questions, ctx.config.explain);
    let tag = crate::util::nonce();
    let prompt_with_mem =
        render_juror_prompt(ctx, req, &retrieval.block, &schema, &tag, &juror_tmpl)?;
    let prompt_blind = if ctx.config.memory.blind_juror {
        Some(render_juror_prompt(
            ctx,
            req,
            "",
            &schema,
            &tag,
            &juror_tmpl,
        )?)
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

    finish_decision(
        ctx,
        req,
        runs,
        &retrieval,
        &tag,
        ws_path.as_deref(),
        ws_stamp.as_deref(),
        repo_id.as_deref(),
        &cache_key,
        1,
        started,
    )
    .await
}

/// Vote → escalate → response → record → cache — the tail shared by
/// `decide` (one case) and `decide_pack` (per item of a pack).
/// `cost_share` amortizes the juror-call cost across packed items.
#[allow(clippy::too_many_arguments)]
async fn finish_decision(
    ctx: &DecideCtx,
    req: &Request,
    runs: Vec<juror::JurorRun>,
    retrieval: &Retrieval,
    tag: &str,
    ws_path: Option<&std::path::Path>,
    ws_stamp: Option<&str>,
    repo_id: Option<&str>,
    cache_key: &str,
    cost_share: usize,
    started: Instant,
) -> Result<(Response, i32)> {
    let workspace_mode = req.state.is_workspace();
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
            // An abstention is *no ballot* for this question — it must
            // not count toward quorum nor the vote denominator.
            if let Some(b) = ballots.get(key)
                && *b != Ballot::Abstain
            {
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
        if votes.len() < ctx.config.min_quorum || vote::is_hung(&out, ctx.config.hung_threshold) {
            hung.push(key.clone());
        }
        answers.insert(key.clone(), out);
    }

    // 5. Escalation.
    let juror_ballots: Vec<(String, BTreeMap<String, Ballot>)> = runs
        .iter()
        .filter_map(|r| r.ballots.as_ref().map(|b| (r.juror.clone(), b.clone())))
        .collect();
    let mut decided_by = DecidedBy::Jury;
    let mut judge_usage: Option<JudgeUsage> = None;
    let mut escalated: Vec<String> = Vec::new();
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
                    tag,
                    ws_path,
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
                            ctx.config.namespace.as_deref(),
                            repo_id,
                            ws_path,
                            &ctx.config.judge,
                        )
                        .unwrap_or_default();
                    }
                    for (key, verdict) in &call.judged {
                        if let Some(a) = answers.get_mut(key) {
                            apply_judge(a, verdict);
                        }
                    }
                    if !call.judged.is_empty() {
                        decided_by = DecidedBy::Judge;
                    }
                    // Keys sent up for ruling; keys the judge skipped or
                    // abstained on stay hung.
                    escalated = hung.clone();
                    hung.retain(|k| !call.judged.contains_key(k));
                }
                judge_usage = Some(ju);
            }
            Escalate::Queue => { /* enqueued after the response exists */ }
            Escalate::Off => {}
        }
    }

    // 6. Response, decisions log, cache. Per-key attribution: every
    // decided key remembers whether jury or judge produced the verdict
    // (hung keys are undecided — absent from `sources`).
    let sources: BTreeMap<String, DecidedBy> = answers
        .keys()
        .filter(|k| !hung.contains(*k))
        .map(|k| {
            (
                k.clone(),
                if escalated.contains(k) {
                    DecidedBy::Judge
                } else {
                    DecidedBy::Jury
                },
            )
        })
        .collect();
    let response = Response {
        id: crate::util::new_decision_id(),
        decided_by,
        answers,
        hung: hung.clone(),
        escalated,
        sources,
        memory: retrieval.used.clone(),
        usage: Usage {
            wall_ms: started.elapsed().as_millis() as u64,
            jurors: runs.iter().map(juror_usage).collect(),
            judge: judge_usage.clone(),
            est_cost_usd: est_cost(ctx, &runs, judge_usage.as_ref(), cost_share),
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
        && let Ok(v) = serde_json::to_value(&response)
    {
        let _ = ctx.cache.put(cache_key, &v);
    }
    let code = response.exit_code();
    Ok((response, code))
}

/// One case inside a `--pack` group. All items in a pack MUST share the
/// same questions — `batch` splits groups at question boundaries.
pub struct PackItem {
    /// The case's request (text states only — workspaces can't share a
    /// prompt, `batch` rejects them before grouping).
    pub req: Request,
}

/// Decide a pack of same-questions cases with ONE juror call per juror
/// (`batch --pack N`). Cache, memory retrieval, vote/quorum/hung and
/// escalation all stay per-item — only the ballot *collection* is packed.
/// Returns one result per item, in input order.
pub async fn decide_pack(ctx: &DecideCtx, items: Vec<PackItem>) -> Vec<Result<(Response, i32)>> {
    let started = Instant::now();
    let n = items.len();

    // Per-item cache keys; cached items short-circuit without a call.
    let juror_tmpl = match ctx.prompts.load("juror_pack.md") {
        Ok(t) => t,
        Err(e) => return fail_all(items, e),
    };
    let cfg_key = jury_config(ctx, false, &juror_tmpl, n);
    let mut results: Vec<Option<Result<(Response, i32)>>> = (0..n).map(|_| None).collect();
    let mut cache_keys = Vec::with_capacity(n);
    let mut pending: Vec<usize> = Vec::new();
    for (pos, it) in items.iter().enumerate() {
        let scopes = req_scopes(ctx, &it.req, None);
        let epoch = memory_epoch(ctx, &scopes);
        let key = Cache::key(&it.req.canonical(), &cfg_key, &epoch, None);
        cache_keys.push(key);
        match cached_response(ctx, &cache_keys[pos]) {
            Some(hit) => results[pos] = Some(Ok(hit)),
            None => pending.push(pos),
        }
    }

    // Per-item memory retrieval — for uncached items only, aligned with
    // `pending` (retrievals[j] ↔ items[pending[j]]).
    let mut retrievals: Vec<Retrieval> = Vec::with_capacity(pending.len());
    for &pos in &pending {
        match retrieve_memory(ctx, &items[pos].req, None, None) {
            Ok(r) => retrievals.push(r),
            Err(e) => return fail_pending(results, &pending, e),
        }
    }
    if pending.is_empty() {
        return collect(results);
    }

    // Render the packed prompt: item ids carry a nonce so state content
    // can't forge a neighbouring item's tag.
    let tag = crate::util::nonce();
    let item_ids: Vec<String> = (0..pending.len()).map(|j| format!("{tag}-i{j}")).collect();
    let schema = packed_ballot_schema(
        &items[pending[0]].req.questions,
        &item_ids,
        ctx.config.explain,
    );
    let prompt_with_mem = match render_pack_prompt(
        ctx,
        &items,
        &pending,
        &item_ids,
        &retrievals,
        &schema,
        &juror_tmpl,
        true,
    ) {
        Ok(p) => p,
        Err(e) => return fail_pending(results, &pending, e),
    };
    let prompt_blind = if ctx.config.memory.blind_juror {
        match render_pack_prompt(
            ctx,
            &items,
            &pending,
            &item_ids,
            &retrievals,
            &schema,
            &juror_tmpl,
            false,
        ) {
            Ok(p) => Some(p),
            Err(e) => return fail_pending(results, &pending, e),
        }
    } else {
        None
    };
    let system_prompt = system_prompt(ctx);
    let questions = items[pending[0]].req.questions.clone();

    // Spawn juror × sample packed calls in parallel.
    let mut futures = FuturesUnordered::new();
    let n_jurors = ctx.config.jurors.len();
    for (i, model_str) in ctx.config.jurors.iter().enumerate() {
        let (kind, model) = match BackendKind::parse(model_str) {
            Ok(km) => km,
            Err(e) => return fail_pending(results, &pending, e),
        };
        let backend = ctx.backend(kind);
        for sample in 0..ctx.config.samples {
            let blind = prompt_blind.is_some() && i == n_jurors - 1;
            let prompt = if blind {
                prompt_blind.clone().unwrap_or_default()
            } else {
                prompt_with_mem.clone()
            };
            let req_inner = AgentRequest {
                prompt,
                system_prompt: system_prompt.clone(),
                model: model.clone(),
                cwd: ctx.empty_cwd.path().to_path_buf(),
                tools: ToolPolicy::None,
                timeout: ctx.config.juror_timeout(false),
                agent: format!("juror:{model_str}#{sample}"),
                json_schema: Some(schema.clone()),
            };
            let questions = questions.clone();
            let item_ids = item_ids.clone();
            let explain = ctx.config.explain;
            let retries = ctx.config.limits.retry_attempts;
            let quota = &ctx.quota;
            let sem = &ctx.semaphore;
            let backend = backend.clone();
            futures.push(async move {
                let _permit = sem.acquire().await.ok();
                juror::run_juror_packed(
                    &backend, req_inner, &questions, &item_ids, explain, retries, quota,
                )
                .await
            });
        }
    }
    let mut packed_runs = Vec::new();
    while let Some(r) = futures.next().await {
        packed_runs.push(r);
    }

    // Per item: view the packed runs as ordinary JurorRuns, then reuse
    // the whole vote/escalate/record tail.
    for (j, pos) in pending.iter().enumerate() {
        let item_runs: Vec<juror::JurorRun> = packed_runs
            .iter()
            .map(|pr| juror::JurorRun {
                juror: pr.juror.clone(),
                sample: pr.sample,
                status: pr.status,
                ms: pr.ms,
                retries: pr.retries,
                ballots: pr
                    .item_ballots
                    .as_ref()
                    .and_then(|ib| ib.get(j).cloned().flatten()),
                why: None,
                error: pr.error.clone(),
                usage: pr.usage.clone(),
            })
            .collect();
        let it = &items[*pos];
        let item_tag = format!("{tag}-i{j}");
        let res = finish_decision(
            ctx,
            &it.req,
            item_runs,
            &retrievals[j],
            &item_tag,
            None,
            None,
            None,
            &cache_keys[*pos],
            pending.len(),
            started,
        )
        .await;
        results[*pos] = Some(res);
    }
    collect(results)
}

/// Unwrap the per-item option slots — every position is filled by the
/// time this runs.
fn collect(results: Vec<Option<Result<(Response, i32)>>>) -> Vec<Result<(Response, i32)>> {
    results
        .into_iter()
        .map(|r| r.expect("every item resolved"))
        .collect()
}

/// Every item fails — pack-level setup error before any slot resolved.
fn fail_all(items: Vec<PackItem>, e: Error) -> Vec<Result<(Response, i32)>> {
    let msg = e.to_string();
    items
        .into_iter()
        .map(|_| Err(Error::Request(msg.clone())))
        .collect()
}

/// Pack-level error after some items resolved from cache — only pending
/// positions get the error.
fn fail_pending(
    mut results: Vec<Option<Result<(Response, i32)>>>,
    pending: &[usize],
    e: Error,
) -> Vec<Result<(Response, i32)>> {
    let msg = e.to_string();
    for &pos in pending {
        results[pos] = Some(Err(Error::Request(msg.clone())));
    }
    collect(results)
}

/// Render `juror_pack.md`: one `<item>` per uncached case, memory block
/// embedded inside its own item tag (precedents must not bleed across
/// items). `with_memory=false` renders the blind-juror variant.
#[allow(clippy::too_many_arguments)]
fn render_pack_prompt(
    ctx: &DecideCtx,
    items: &[PackItem],
    pending: &[usize],
    item_ids: &[String],
    retrievals: &[Retrieval],
    schema: &serde_json::Value,
    tmpl: &str,
    with_memory: bool,
) -> Result<String> {
    let items_block = pending
        .iter()
        .zip(item_ids)
        .enumerate()
        .map(|(j, (&pos, id))| {
            let state = state_render(&items[pos].req);
            let mem = if with_memory {
                retrievals[j].block.as_str()
            } else {
                ""
            };
            format!("<item id=\"{id}\">\n{state}\n{mem}</item>")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("schema_block", schema_block_text(schema));
    vars.insert("policy_block", policy_block(ctx));
    vars.insert("questions_block", questions_render(&items[pending[0]].req));
    vars.insert("items_block", items_block);
    Ok(crate::prompt::render(tmpl, &vars))
}

/// Estimated USD cost of this decision from `[costs]` — `None` unless at
/// least one backend has a configured price. `share` amortizes a packed
/// call's cost across the items it answered (1 for a single decide).
fn est_cost(
    ctx: &DecideCtx,
    runs: &[juror::JurorRun],
    judge: Option<&JudgeUsage>,
    share: usize,
) -> Option<f64> {
    if ctx.config.costs.is_empty() {
        return None;
    }
    let share = share.max(1) as f64;
    let mut total = 0.0;
    let mut any = false;
    for r in runs {
        if let Some(p) = ctx.config.cost_per_call(&r.juror) {
            total += p * (1.0 + r.retries as f64) / share;
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
        (Some(r), Some(p)) => Some(workspace::verify_facts(
            store,
            p,
            ctx.config.namespace.as_deref(),
            r,
        )?),
        _ => None,
    };
    retrieve::retrieve(
        store,
        req,
        &ctx.config.memory,
        ctx.config.namespace.as_deref(),
        facts,
    )
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
            format!("## Domain policy\n\n{p}\n\nApply these rules over your own defaults.\n\n")
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
    tmpl: &str,
) -> Result<String> {
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("schema_block", schema_block_text(schema));
    vars.insert("policy_block", policy_block(ctx));
    vars.insert("questions_block", questions_render(req));
    vars.insert("memory_block", memory_block.to_string());
    vars.insert("workspace_block", workspace_render(req));
    vars.insert("state_tag", format!("state-{tag}"));
    vars.insert("state", state_render(req));
    Ok(crate::prompt::render(tmpl, &vars))
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
        answers: r
            .ballots
            .as_ref()
            .map(|b| b.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
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
