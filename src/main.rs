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
    /// Memory db path.
    #[arg(long, global = true)]
    memory_db: Option<PathBuf>,
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
}

#[derive(Args)]
struct DecideArgs {
    /// Request JSON file (`-` = stdin). Alternative to --state*/--questions.
    request: Option<PathBuf>,
    /// Inline state text.
    #[arg(long, conflicts_with = "request")]
    state: Option<String>,
    /// File whose contents are the state text.
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
    /// Re-judge N random jury decisions; disagreements earn precedents.
    #[arg(long)]
    audit: Option<usize>,
    /// Merge over-cap ruling sets via the judge.
    #[arg(long)]
    consolidate: bool,
    /// Report what would happen without calling any CLI.
    #[arg(long)]
    dry_run: bool,
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
            learn::feedback(&ctx, &args.decision_id, &sets, args.note.as_deref())?;
            println!("{}", serde_json::json!({"ok": true, "decision": args.decision_id}));
            Ok(0)
        }
        Cmd::Learn(args) => {
            let (_cfg, ctx) = load_ctx(over, cfg_path)?;
            let queue = args.queue || (args.audit.is_none() && !args.consolidate);
            if queue {
                learn::learn_queue(&ctx, args.dry_run).await?;
            }
            if let Some(n) = args.audit {
                learn::learn_audit(&ctx, n, args.dry_run).await?;
            }
            if args.consolidate {
                learn::learn_consolidate(&ctx, args.dry_run).await?;
            }
            Ok(0)
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
        Cmd::Memory { cmd } => cmd_memory(cmd, over, cfg_path),
        Cmd::Doctor { json } => {
            let cfg = Config::load(over, cfg_path);
            match &cfg {
                Ok(c) => Ok(doctor::run(Some(c), None, json).await as u8),
                Err(e) => Ok(doctor::run(None, Some(&e.to_string()), json).await as u8),
            }
        }
    }
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
        State::Text(
            std::fs::read_to_string(f).map_err(|e| hungjury::error::Error::io(f, e))?,
        )
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

fn cmd_memory(cmd: MemoryCmd, over: &CliOverrides, cfg_path: Option<&Path>) -> hungjury::error::Result<u8> {
    let cfg = Config::load(over, cfg_path)?;
    let store = Store::open(&cfg.memory_db)?;
    match cmd {
        MemoryCmd::Search { query, scope } => {
            let q = memory::retrieve::fts_query(&query);
            let entries = store.search(&q, scope.as_deref())?;
            print_entries(&entries);
        }
        MemoryCmd::List { kind, scope, all } => {
            let kind = kind
                .as_deref()
                .map(|k| {
                    Kind::parse(k).ok_or_else(|| {
                        hungjury::error::Error::Request(format!("unknown kind '{k}'"))
                    })
                })
                .transpose()?;
            print_entries(&store.list(kind, scope.as_deref(), !all)?);
        }
        MemoryCmd::Show { id } => match store.get(&id)? {
            Some(e) => print_entries(std::slice::from_ref(&e)),
            None => {
                eprintln!("no entry '{id}'");
                return Ok(1);
            }
        },
        MemoryCmd::Forget { id } => {
            let changed = store.forget(&id)?;
            println!("{}", serde_json::json!({"id": id, "forgotten": changed}));
        }
        MemoryCmd::Stats => {
            let counts = store.counts()?;
            let stats = store.juror_stats_rows()?;
            println!("{}", serde_json::json!({
                "entries": counts.iter().map(|(k, s, n)| serde_json::json!({
                    "kind": k, "status": s, "n": n,
                })).collect::<Vec<_>>(),
                "decisions": store.decisions_len()?,
                "queue_pending": store.queue_len()?,
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
        no_memory: g.no_memory,
        memory_readonly: g.memory_readonly,
        explain: g.explain,
        no_cache: g.no_cache,
        refresh: g.refresh,
        prompts_dir: g.prompts_dir.clone(),
        memory_db: g.memory_db.clone(),
        profile: g.profile.clone(),
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
