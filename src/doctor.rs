//! `hungjury doctor` — environment self-check.
//!
//! Reports, without spending a single CLI call: which agent CLIs are on
//! PATH, what config resolved to, whether the memory db opens (and has
//! FTS5), the quota state, and any agent processes currently running.

use std::path::Path;

use crate::backend::BackendKind;
use crate::config::Config;
use crate::memory::store::Store;

/// One check line: name → ok?, detail.
struct Check {
    name: String,
    ok: bool,
    detail: String,
}

/// Run all checks against the resolved config (may be `None` when config
/// failed to load — that failure is itself reported).
pub async fn run(cfg: Option<&Config>, cfg_err: Option<&str>, json: bool) -> i32 {
    let mut checks: Vec<Check> = Vec::new();

    // 1. Agent CLIs on PATH.
    for kind in BackendKind::all_real() {
        let found = crate::sys::find_on_path(kind.as_str());
        checks.push(Check {
            name: format!("cli:{}", kind.as_str()),
            ok: found.is_some(),
            detail: found
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not on PATH".to_string()),
        });
    }
    let detected = BackendKind::detect_all();
    checks.push(Check {
        name: "autodetect".to_string(),
        ok: !detected.is_empty(),
        detail: if detected.is_empty() {
            "no agent CLI found".to_string()
        } else {
            detected
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        },
    });

    // 2. git (workspace identity depends on it).
    let git = crate::sys::find_on_path("git");
    checks.push(Check {
        name: "cli:git".to_string(),
        ok: git.is_some(),
        detail: git
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not on PATH — workspaces uncached".to_string()),
    });

    // 3. Config resolution.
    match (cfg, cfg_err) {
        (Some(c), _) => {
            checks.push(Check {
                name: "config".to_string(),
                ok: true,
                detail: format!(
                    "jurors=[{}] judge={} escalate={:?} threshold={}",
                    c.jurors.join(","),
                    c.judge,
                    c.escalate,
                    c.hung_threshold
                ),
            });
            checks.push(Check {
                name: "data_dir".to_string(),
                ok: c.data_dir.is_dir() || mk_writable(&c.data_dir),
                detail: c.data_dir.display().to_string(),
            });
        }
        (None, err) => checks.push(Check {
            name: "config".to_string(),
            ok: false,
            detail: err.unwrap_or("unresolved").to_string(),
        }),
    }

    // 4. Memory db + FTS5.
    if let Some(c) = cfg {
        match Store::open(&c.memory_db) {
            Ok(s) => {
                let fts = s.has_fts5();
                checks.push(Check {
                    name: "memory_db".to_string(),
                    ok: true,
                    detail: format!("{} ({} entries)", c.memory_db.display(), entry_count(&s)),
                });
                checks.push(Check {
                    name: "memory_fts5".to_string(),
                    ok: fts,
                    detail: if fts {
                        "available".to_string()
                    } else {
                        "missing — precedent search disabled".to_string()
                    },
                });
            }
            Err(e) => checks.push(Check {
                name: "memory_db".to_string(),
                ok: false,
                detail: format!("{}: {e}", c.memory_db.display()),
            }),
        }
    }

    // 5. Quota spend today.
    if let Some(c) = cfg {
        let q = crate::quota::Quota::new(&c.data_dir, c.limits.daily_cap);
        let spent = q.today_count().await;
        checks.push(Check {
            name: "quota".to_string(),
            ok: spent < c.limits.daily_cap,
            detail: format!("{spent}/{} calls today", c.limits.daily_cap),
        });
    }

    // 6. Running agent processes (informational — leaks would show here).
    let procs: Vec<String> = crate::sys::list_processes()
        .iter()
        .filter_map(|p| {
            crate::sys::agent_name(p).map(|n| {
                format!(
                    "{n}#{}{}",
                    p.pid,
                    p.etime
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                )
            })
        })
        .collect();
    checks.push(Check {
        name: "processes".to_string(),
        ok: true,
        detail: if procs.is_empty() {
            "none running".to_string()
        } else {
            procs.join(", ")
        },
    });

    // Render.
    let all_ok = checks.iter().all(|c| c.ok);
    if json {
        let v = serde_json::json!({
            "ok": all_ok,
            "checks": checks.iter().map(|c| serde_json::json!({
                "name": c.name, "ok": c.ok, "detail": c.detail,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        for c in &checks {
            eprintln!("{} {:<14} {}", if c.ok { "ok " } else { "FAIL" }, c.name, c.detail);
        }
        eprintln!("{}", if all_ok { "doctor: all checks passed" } else { "doctor: failures above" });
    }
    if all_ok { 0 } else { 1 }
}

fn entry_count(s: &Store) -> i64 {
    s.counts().map(|v| v.iter().map(|(_, _, n)| *n).sum()).unwrap_or(0)
}

fn mk_writable(p: &Path) -> bool {
    std::fs::create_dir_all(p).is_ok()
}
