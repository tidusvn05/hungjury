//! `hungjury` — typed decisions from a jury of headless CLI agents.
//!
//! `stdout` carries only the result JSON (decision / query output);
//! diagnostics go to `stderr`. Exit codes: `0` ok, `2` hung unresolved,
//! `1` error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use hungjury::config::{CliOverrides, Config, Escalate};
use hungjury::doctor;
use hungjury::eval;
use hungjury::jury::{self, DecideCtx};
use hungjury::learn;
use hungjury::memory;
use hungjury::memory::store::{Kind, Store};
use hungjury::request::{Request, State};

/// Typed decisions from a jury of CLI agents.
#[derive(Parser)]
#[command(name = "hungjury", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    #[command(flatten)]
    global: Global,
}

/// Flags accepted by every subcommand.
#[derive(Args, Debug, Default)]
struct Global {
    /// Config file (default: ./hungjury.toml when present).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Named profile (TOML [profiles.X] or built-in backend name).
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Juror model strings, comma-separated (`backend:model[@effort]`).
    #[arg(long, global = true, value_delimiter = ',')]
    jurors: Option<Vec<String>>,
    /// Judge model string.
    #[arg(long, global = true)]
    judge: Option<String>,
    /// Ballots per juror.
    #[arg(long, global = true)]
    samples: Option<u32>,
    /// Hung-jury escalation policy.
    #[arg(long, global = true, value_enum)]
    escalate: Option<Escalate>,
    /// Confidence below this marks a question hung.
    #[arg(long, global = true)]
    hung_threshold: Option<f64>,
    /// Minimum valid ballots per question — fewer ⇒ hung (default 2).
    #[arg(long, global = true)]
    min_quorum: Option<usize>,
    /// Disable memory entirely.
    #[arg(long, global = true)]
    no_memory: bool,
    /// Read memory but never write.
    #[arg(long, global = true)]
    memory_readonly: bool,
    /// Jurors include a short `_why` per answer (leak channel — review).
    #[arg(long, global = true)]
    explain: bool,
    /// Disable the decision cache.
    #[arg(long, global = true)]
    no_cache: bool,
    /// Skip cache reads, still write.
    #[arg(long, global = true)]
    refresh: bool,
    /// Prompt-template override directory.
    #[arg(long, global = true)]
    prompts_dir: Option<PathBuf>,
    /// Domain policy file — injected into juror + judge prompts.
    #[arg(long, global = true)]
    policy_file: Option<PathBuf>,
    /// Memory db path.
    #[arg(long, global = true)]
    memory_db: Option<PathBuf>,
    /// Memory namespace — soft-partition scopes as `ns:q:<qid>`.
    #[arg(long, global = true)]
    namespace: Option<String>,
    /// Verbosity (-v info, -vv debug).
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Cmd {
    /// Decide the questions over a state.
    Decide(DecideArgs),
    /// Attach a human verdict to a past decision (trust 1.0).
    Feedback(FeedbackArgs),
    /// Offline learning: queue / audit / consolidate.
    Learn(LearnArgs),
    /// Run the memory go/no-go experiment over labeled cases.
    Eval(EvalArgs),
    /// Decide every case in an unlabeled JSONL file, in parallel.
    Batch(BatchArgs),
    /// Inspect and manage local memory.
    Memory {
        #[command(subcommand)]
        cmd: MemoryCmd,
    },
    /// Environment self-check.
    Doctor {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Scaffold a `.hungjury/` project dir (config, policy, .gitignore).
    Init {
        /// Directory to initialize (default: cwd).
        dir: Option<PathBuf>,
        /// Write `~/.config/hungjury/config.toml` instead of a project dir.
        #[arg(long)]
        global: bool,
    },
}

#[derive(Args)]
struct DecideArgs {
    /// Request JSON file (`-` = stdin). Alternative to --state*/--questions.
    request: Option<PathBuf>,
    /// Inline state text.
    #[arg(long, conflicts_with = "request")]
    state: Option<String>,
    /// File whose contents are the state text (`-` = stdin).
    #[arg(long, conflicts_with_all = ["request", "state"])]
    state_file: Option<PathBuf>,
    /// Directory judged as a workspace (read-only tools).
    #[arg(long, conflicts_with_all = ["request", "state", "state_file"])]
    workspace: Option<PathBuf>,
    /// Hint passed with a workspace state.
    #[arg(long, requires = "workspace")]
    hint: Option<String>,
    /// Questions JSON, or `@file`.
    #[arg(long, conflicts_with = "request")]
    questions: Option<String>,
}

#[derive(Args)]
struct FeedbackArgs {
    /// `dec_…` id from a previous `decide`.
    decision_id: String,
    /// `key=value` human verdicts (value parsed as JSON, else string).
    #[arg(long = "set", value_name = "KEY=VALUE", required = true)]
    sets: Vec<String>,
    /// Rationale stored on the precedent.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Args)]
struct LearnArgs {
    /// Judge every queued hung decision (default when no flag given).
    #[arg(long)]
    queue: bool,
    /// Re-judge N jury decisions; disagreements earn precedents.
    #[arg(long)]
    audit: Option<usize>,
    /// With --audit: pick the N most recent decisions instead of random.
    #[arg(long, requires = "audit")]
    recent: bool,
    /// Merge over-cap ruling sets via the judge.
    #[arg(long)]
    consolidate: bool,
    /// Report what would happen without calling any CLI.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct BatchArgs {
    /// JSONL file: one `{"state": ..., "questions": {...}}` per line.
    cases: PathBuf,
    /// Output JSONL path (default: stdout).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct EvalArgs {
    /// JSONL labeled cases file.
    cases: PathBuf,
    /// Fraction used for training (rest is test).
    #[arg(long, default_value = "0.5")]
    train_frac: f64,
    /// Report output path.
    #[arg(long, default_value = "report.json")]
    report: PathBuf,
    /// Shuffle seed (deterministic).
    #[arg(long, default_value = "1")]
    seed: u64,
    /// Benchmark/domain label recorded in the report.
    #[arg(long)]
    label: Option<String>,
}

#[derive(Subcommand)]
enum MemoryCmd {
    /// FTS5 search over entries.
    Search {
        /// Query text (tokenized, OR-joined).
        query: String,
        /// Restrict to a scope (`q:<qid>` | `ws:<repo>`).
        #[arg(long)]
        scope: Option<String>,
    },
    /// List entries.
    List {
        /// ruling | precedent | fact.
        #[arg(long)]
        kind: Option<String>,
        /// Restrict to a scope.
        #[arg(long)]
        scope: Option<String>,
        /// Include superseded/stale/contested/forgotten.
        #[arg(long)]
        all: bool,
        /// Only this status (active|contested|superseded|stale|forgotten).
        #[arg(long)]
        status: Option<String>,
    },
    /// Recent decisions (ids for `feedback`).
    Decisions {
        /// How many to show, newest first.
        #[arg(long, default_value = "20")]
        last: usize,
    },
    /// List contested entries awaiting human review (with bodies).
    Review,
    /// Resolve a contested entry: --accept reactivates, --reject tombstones.
    Resolve {
        /// Entry id.
        id: String,
        /// Mark the entry active again.
        #[arg(long, conflicts_with = "reject", required_unless_present = "reject")]
        accept: bool,
        /// Tombstone the entry (id kept; imports cannot resurrect it).
        #[arg(long)]
        reject: bool,
    },
    /// Show one entry as JSON.
    Show {
        /// Entry id.
        id: String,
    },
    /// Tombstone an entry (id kept; imports cannot resurrect it).
    Forget {
        /// Entry id.
        id: String,
    },
    /// Counts, queue length, juror stats.
    Stats,
    /// Write a bundle file (rulings + facts; precedents need --include-cases).
    Export {
        /// Output path.
        out: PathBuf,
        /// Restrict to a scope.
        #[arg(long)]
        scope: Option<String>,
        /// Include precedents (they embed state excerpts).
        #[arg(long)]
        include_cases: bool,
    },
    /// Import one bundle.
    Import {
        /// Bundle file.
        file: PathBuf,
        /// Trust multiplier applied to imported entries.
        #[arg(long, default_value = "1.0")]
        trust_factor: f64,
        /// Count only — write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import several bundles in order.
    Merge {
        /// Bundle files.
        files: Vec<PathBuf>,
        /// Trust multiplier applied to imported entries.
        #[arg(long, default_value = "1.0")]
        trust_factor: f64,
        /// Count only — write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.global.verbose);
    let over = overrides(&cli.global);
    let cfg_path = cli.global.config.as_deref();

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fatal: tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };

    let code = rt.block_on(dispatch(cli.cmd, &over, cfg_path));
    match code {
        Ok(c) => ExitCode::from(c),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

async fn dispatch(
    cmd: Cmd,
    over: &CliOverrides,
    cfg_path: Option<&Path>,
) -> hungjury::error::Result<u8> {
    match cmd {
        Cmd::Decide(args) => cmd_decide(args, over, cfg_path).await,
        Cmd::Feedback(args) => {
            let (cfg, ctx) = load_ctx(over, cfg_path)?;
            let _ = cfg;
            let sets = parse_sets(&args.sets)?;
            let contested =
                learn::feedback(&ctx, &args.decision_id, &sets, args.note.as_deref())?;
            println!(
                "{}",
                serde_json::json!({"ok": true, "decision": args.decision_id, "contested": contested})
            );
            Ok(0)
        }
        Cmd::Learn(args) => {
            let (cfg, ctx) = load_ctx(over, cfg_path)?;
            if cfg.memory.ruling_ttl_days > 0
                && let Some(store) = &ctx.store
            {
                match store.expire_rulings(cfg.memory.ruling_ttl_days) {
                    Ok(0) => {}
                    Ok(n) => eprintln!("learn: {n} rulings expired (ttl {}d)", cfg.memory.ruling_ttl_days),
                    Err(e) => eprintln!("learn: ruling expiry failed: {e}"),
                }
            }
            let queue = args.queue || (args.audit.is_none() && !args.consolidate);
            if queue {
                learn::learn_queue(&ctx, args.dry_run).await?;
            }
            if let Some(n) = args.audit {
                learn::learn_audit(&ctx, n, args.recent, args.dry_run).await?;
            }
            if args.consolidate {
                learn::learn_consolidate(&ctx, args.dry_run).await?;
            }
            Ok(0)
        }
        Cmd::Batch(args) => {
            let (_cfg, ctx) = load_ctx(over, cfg_path)?;
            hungjury::batch::run(&ctx, &args.cases, args.out.as_deref()).await
        }
        Cmd::Eval(args) => {
            eval::run(
                &args.cases,
                args.train_frac,
                &args.report,
                over,
                cfg_path,
                args.seed,
                args.label.as_deref(),
            )
            .await?;
            Ok(0)
        }
        Cmd::Memory { cmd } => cmd_memory(cmd, over, cfg_path).await,
        Cmd::Doctor { json } => {
            let cfg = Config::load(over, cfg_path);
            match &cfg {
                Ok(c) => Ok(doctor::run(Some(c), None, json).await as u8),
                Err(e) => Ok(doctor::run(None, Some(&e.to_string()), json).await as u8),
            }
        }
        Cmd::Init { dir, global } => cmd_init(dir, global),
    }
}

/// `hungjury init` — scaffold `.hungjury/` (or the global config with
/// `--global`). Never overwrites existing files.
fn cmd_init(dir: Option<PathBuf>, global: bool) -> hungjury::error::Result<u8> {
    const CONFIG_SKEL: &str = r#"# hungjury project config — merge order: global ~/.config/hungjury/config.toml
# → this file → --profile → CLI flags.

# jurors = ["claude:haiku", "codex:gpt-5.6-terra@low", "devin:swe-2-medium"]
# judge = "claude:opus@high"
# escalate = "sync"        # sync | queue | off
# hung_threshold = 0.5
# min_quorum = 2
# policy_file = "policy.md"   # resolved relative to this dir (auto-detected anyway)
# namespace = "triage"        # soft-partition memory scopes as triage:q:<qid>

# Hard-isolated purpose: a profile gets its own memory db.
# [profiles.review]
# jurors = ["codex:gpt-5.6-terra@low"]
# memory_db = "memory-review.db"   # → .hungjury/memory-review.db
# policy_file = "policy-review.md"

# [costs]
# claude = 0.08
# codex = 0.05
# devin = 0.05
"#;
    const POLICY_SKEL: &str = "# Decision policy\n\nRules the jury and judge must apply when signals\nconflict — your labelling rubric, in order of precedence.\n\n## <question key>\n\n- <rule>\n";
    const GITIGNORE: &str = "# hungjury runtime state — never commit\nmemory*.db\ncache/\ncalls.jsonl\nstate.json\n";

    if global {
        let Some(cfg_dir) =
            directories::ProjectDirs::from("", "", "hungjury").map(|p| p.config_dir().to_path_buf())
        else {
            eprintln!("init: cannot resolve config dir");
            return Ok(1);
        };
        std::fs::create_dir_all(&cfg_dir).map_err(|e| hungjury::error::Error::io(&cfg_dir, e))?;
        let path = cfg_dir.join("config.toml");
        if path.exists() {
            println!("exists: {}", path.display());
        } else {
            std::fs::write(&path, CONFIG_SKEL).map_err(|e| hungjury::error::Error::io(&path, e))?;
            println!("created: {}", path.display());
        }
        return Ok(0);
    }

    let root = dir.unwrap_or_else(|| PathBuf::from("."));
    let hj = root.join(".hungjury");
    std::fs::create_dir_all(&hj).map_err(|e| hungjury::error::Error::io(&hj, e))?;
    for (name, content) in [
        ("config.toml", CONFIG_SKEL),
        ("policy.md", POLICY_SKEL),
        (".gitignore", GITIGNORE),
    ] {
        let path = hj.join(name);
        if path.exists() {
            println!("exists:  {}", path.display());
        } else {
            std::fs::write(&path, content).map_err(|e| hungjury::error::Error::io(&path, e))?;
            println!("created: {}", path.display());
        }
    }
    eprintln!("hungjury: project dir ready — edit .hungjury/config.toml and policy.md");
    Ok(0)
}

/// `Config` + `DecideCtx` for commands that need backends/memory.
fn load_ctx(over: &CliOverrides, cfg_path: Option<&Path>) -> hungjury::error::Result<(Config, DecideCtx)> {
    let cfg = Config::load(over, cfg_path)?;
    let ctx = DecideCtx::new(cfg.clone(), None)?;
    Ok((cfg, ctx))
}

async fn cmd_decide(
    args: DecideArgs,
    over: &CliOverrides,
    cfg_path: Option<&Path>,
) -> hungjury::error::Result<u8> {
    let req = build_request(&args)?;
    let (_cfg, ctx) = load_ctx(over, cfg_path)?;
    let (resp, code) = jury::decide(&ctx, &req).await?;
    println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
    Ok(code as u8)
}

/// Request from a file/stdin, or from the split flags.
fn build_request(args: &DecideArgs) -> hungjury::error::Result<Request> {
    if let Some(path) = &args.request {
        let text = if path.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin())
                .map_err(|e| hungjury::error::Error::io("stdin", e))?
        } else {
            std::fs::read_to_string(path).map_err(|e| hungjury::error::Error::io(path, e))?
        };
        return Request::from_json(&text);
    }
    let state = if let Some(t) = &args.state {
        State::Text(t.clone())
    } else if let Some(f) = &args.state_file {
        // `-` reads state text from stdin — pipes a CI log straight in.
        State::Text(if f.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin())
                .map_err(|e| hungjury::error::Error::io("stdin", e))?
        } else {
            std::fs::read_to_string(f).map_err(|e| hungjury::error::Error::io(f, e))?
        })
    } else if let Some(w) = &args.workspace {
        State::Workspace {
            path: w.clone(),
            hint: args.hint.clone(),
        }
    } else {
        return Err(hungjury::error::Error::Request(
            "provide a request file or --state/--state-file/--workspace".to_string(),
        ));
    };
    let questions = args.questions.as_deref().ok_or_else(|| {
        hungjury::error::Error::Request("missing --questions".to_string())
    })?;
    let qjson = match questions.strip_prefix('@') {
        Some(f) => std::fs::read_to_string(f).map_err(|e| hungjury::error::Error::io(f, e))?,
        None => questions.to_string(),
    };
    Request::from_parts(state, &qjson)
}

fn parse_sets(sets: &[String]) -> hungjury::error::Result<Vec<(String, String)>> {
    sets.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| {
                    hungjury::error::Error::Request(format!("--set '{s}' is not KEY=VALUE"))
                })
        })
        .collect()
}

/// Resolve a full id or an unambiguous ≥4-char prefix to an entry id.
fn resolve_entry_id(store: &Store, id: &str) -> hungjury::error::Result<Option<String>> {
    if store.get(id)?.is_some() {
        return Ok(Some(id.to_string()));
    }
    store.id_by_prefix(id)
}

async fn cmd_memory(cmd: MemoryCmd, over: &CliOverrides, cfg_path: Option<&Path>) -> hungjury::error::Result<u8> {
    let cfg = Config::load(over, cfg_path)?;
    let store = Store::open(&cfg.memory_db)?;
    match cmd {
        MemoryCmd::Search { query, scope } => {
            let q = memory::retrieve::fts_query(&query);
            let entries = store.search(&q, scope.as_deref())?;
            print_entries(&entries);
        }
        MemoryCmd::List { kind, scope, all, status } => {
            let kind = kind
                .as_deref()
                .map(|k| {
                    Kind::parse(k).ok_or_else(|| {
                        hungjury::error::Error::Request(format!("unknown kind '{k}'"))
                    })
                })
                .transpose()?;
            let want = match status.as_deref() {
                Some(s) => {
                    let st = hungjury::memory::store::Status::parse(s);
                    if st.as_str() != s {
                        return Err(hungjury::error::Error::Request(format!(
                            "unknown status '{s}'"
                        )));
                    }
                    Some(st)
                }
                None => None,
            };
            let entries = store.list(kind, scope.as_deref(), !all || want.is_some())?;
            print_entries(
                &entries
                    .into_iter()
                    .filter(|e| want.as_ref().map(|w| e.status == *w).unwrap_or(true))
                    .collect::<Vec<_>>(),
            );
        }
        MemoryCmd::Decisions { last } => {
            for d in store.list_decisions(last)? {
                let resp = &d.response;
                let answers = resp["answers"].as_object().map(|m| {
                    m.iter()
                        .map(|(k, v)| {
                            let val = v
                                .get("choice")
                                .or_else(|| v.get("score"))
                                .or_else(|| v.get("noul"))
                                .cloned()
                                .unwrap_or(serde_json::Value::String("hung".into()));
                            (k.clone(), val)
                        })
                        .collect::<serde_json::Map<_, _>>()
                });
                println!(
                    "{}",
                    serde_json::json!({
                        "id": d.id,
                        "at": d.created_at,
                        "decided_by": d.decided_by,
                        "hung": resp["hung"],
                        "escalated": resp["escalated"],
                        "sources": resp["sources"],
                        "answers": answers,
                    })
                );
            }
        }
        MemoryCmd::Review => {
            let contested = store
                .list(None, None, false)?
                .into_iter()
                .filter(|e| e.status == hungjury::memory::store::Status::Contested)
                .collect::<Vec<_>>();
            if contested.is_empty() {
                eprintln!("review: no contested entries");
            } else {
                eprintln!(
                    "review: {} contested entries — `memory resolve <id> --accept|--reject`",
                    contested.len()
                );
            }
            print_entries(&contested);
        }
        MemoryCmd::Resolve { id, accept, reject: _ } => {
            let Some(id) = resolve_entry_id(&store, &id)? else {
                eprintln!("no entry matching '{id}' (need ≥4 unambiguous chars)");
                return Ok(1);
            };
            if accept {
                store.set_status(&id, hungjury::memory::store::Status::Active, None)?;
                println!("{}", serde_json::json!({"id": id, "status": "active"}));
            } else {
                let changed = store.forget(&id)?;
                println!("{}", serde_json::json!({"id": id, "forgotten": changed}));
            }
        }
        MemoryCmd::Show { id } => match resolve_entry_id(&store, &id)? {
            Some(id) => print_entries(std::slice::from_ref(&store.get(&id)?.unwrap())),
            None => {
                eprintln!("no entry matching '{id}' (need ≥4 unambiguous chars)");
                return Ok(1);
            }
        },
        MemoryCmd::Forget { id } => {
            let Some(id) = resolve_entry_id(&store, &id)? else {
                eprintln!("no entry matching '{id}' (need ≥4 unambiguous chars)");
                return Ok(1);
            };
            let changed = store.forget(&id)?;
            println!("{}", serde_json::json!({"id": id, "forgotten": changed}));
        }
        MemoryCmd::Stats => {
            let counts = store.counts()?;
            let stats = store.juror_stats_rows()?;
            let quota = hungjury::quota::Quota::new(&cfg.data_dir, cfg.limits.daily_cap);
            println!("{}", serde_json::json!({
                "entries": counts.iter().map(|(k, s, n)| serde_json::json!({
                    "kind": k, "status": s, "n": n,
                })).collect::<Vec<_>>(),
                "by_source": store.source_counts()?.iter().map(|(s, n)| serde_json::json!({
                    "source": s, "n": n,
                })).collect::<Vec<_>>(),
                "namespaces": store.namespaces()?.iter().map(|(s, n)| serde_json::json!({
                    "namespace": if s.is_empty() { "(default)" } else { s }, "n": n,
                })).collect::<Vec<_>>(),
                "decisions": store.decisions_len()?,
                "queue_pending": store.queue_len()?,
                "cache_entries": hungjury::cache::Cache::new(
                    &cfg.data_dir, cfg.no_cache, cfg.refresh,
                ).len(),
                "calls_today": quota.today_count().await,
                "daily_cap": cfg.limits.daily_cap,
                "juror_stats": stats.iter().map(|(j, q, n, a)| serde_json::json!({
                    "juror": j, "qid": q, "n": n, "agree": a,
                })).collect::<Vec<_>>(),
            }));
        }
        MemoryCmd::Export { out, scope, include_cases } => {
            let n = memory::bundle::export(&store, &out, scope.as_deref(), include_cases)?;
            println!("{}", serde_json::json!({"exported": n, "out": out}));
        }
        MemoryCmd::Import { file, trust_factor, dry_run } => {
            let r = memory::bundle::import(&store, &file, trust_factor, dry_run)?;
            println!("{}", import_json(&r, dry_run));
        }
        MemoryCmd::Merge { files, trust_factor, dry_run } => {
            let mut total = memory::bundle::ImportReport::default();
            for f in &files {
                let r = memory::bundle::import(&store, f, trust_factor, dry_run)?;
                total.new += r.new;
                total.duplicate += r.duplicate;
                total.tombstoned += r.tombstoned;
                total.contested += r.contested;
                total.rejected += r.rejected;
            }
            println!("{}", import_json(&total, dry_run));
        }
    }
    Ok(0)
}

fn import_json(r: &memory::bundle::ImportReport, dry_run: bool) -> serde_json::Value {
    serde_json::json!({
        "dry_run": dry_run,
        "new": r.new,
        "duplicate": r.duplicate,
        "tombstoned": r.tombstoned,
        "contested": r.contested,
        "rejected": r.rejected,
    })
}

/// Entries as a JSON array on stdout (memory output is data, not prose).
fn print_entries(entries: &[hungjury::memory::store::Entry]) {
    let v: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "kind": e.kind.as_str(),
                "scope": e.scope,
                "status": e.status.as_str(),
                "trust": e.trust,
                "source": e.source.as_str(),
                "author": e.author,
                "origin": e.origin,
                "created_at": e.created_at,
                "superseded_by": e.superseded_by,
                "text": e.text,
                "body": e.body,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn overrides(g: &Global) -> CliOverrides {
    CliOverrides {
        jurors: g.jurors.clone(),
        samples: g.samples,
        judge: g.judge.clone(),
        escalate: g.escalate,
        hung_threshold: g.hung_threshold,
        min_quorum: g.min_quorum,
        no_memory: g.no_memory,
        memory_readonly: g.memory_readonly,
        explain: g.explain,
        no_cache: g.no_cache,
        refresh: g.refresh,
        prompts_dir: g.prompts_dir.clone(),
        policy_file: g.policy_file.clone(),
        memory_db: g.memory_db.clone(),
        profile: g.profile.clone(),
        namespace: g.namespace.clone(),
    }
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
