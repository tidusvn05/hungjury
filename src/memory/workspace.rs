//! Workspace identity + fact-evidence verification.
//!
//! `repo_id` is the repo's root commit (`git rev-list --max-parents=0
//! HEAD`) so the same repo cloned on another machine still matches; a
//! non-git dir falls back to `sha256(absolute path)`.
//!
//! `workspace_stamp` feeds the cache key: `HEAD` + hashes of
//! `git status --porcelain` and `git diff`. Non-git ⇒ `None` ⇒ the
//! decision is never cached (a plain dir offers no stable identity).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::Result;
use crate::memory::store::{Entry, Status, Store};
use crate::util::sha256_hex;

/// Root commit hash, or `None` when `path` isn't inside a git repo.
pub fn root_commit(path: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Multiple roots (octopus history) → take the first; stable enough.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `ws:<id>` identity for a workspace path.
pub fn repo_id(path: &Path) -> String {
    if let Some(root) = root_commit(path) {
        return root;
    }
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::util::sha256_str(&abs.display().to_string())
}

/// Current `HEAD` (for the cache stamp).
fn head(path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Hash of a git command's stdout (status/diff snapshot).
fn hash_of_git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(path).args(args).output();
    match out {
        Ok(o) if o.status.success() => sha256_hex(&o.stdout),
        _ => String::new(),
    }
}

/// `Some(stamp)` for git workspaces, `None` for plain dirs (never cached).
pub fn workspace_stamp(path: &Path) -> Option<String> {
    let head = head(path)?;
    let status = hash_of_git(path, &["status", "--porcelain"]);
    let diff = hash_of_git(path, &["diff"]);
    Some(format!("{head}:{status}:{diff}"))
}

/// `sha256` of a file's content, `None` when unreadable/missing.
fn file_sha256(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| sha256_hex(&b))
}

/// Re-check every active fact's evidence files against the workspace.
/// A fact whose evidence hash drifted becomes `stale` and is excluded
/// from what this call returns.
pub fn verify_facts(store: &Store, ws: &Path, repo_id: &str) -> Result<Vec<Entry>> {
    let facts = store.facts(repo_id)?;
    let mut good = Vec::new();
    for f in facts {
        let evidence_ok = f.body["evidence"]
            .as_array()
            .map(|ev| {
                ev.iter().all(|e| {
                    let path = e["path"].as_str().unwrap_or("");
                    let want = e["sha256"].as_str().unwrap_or("");
                    !path.is_empty()
                        && file_sha256(&ws.join(path)).as_deref() == Some(want)
                })
            })
            // Facts without evidence can't drift.
            .unwrap_or(true);
        if evidence_ok {
            good.push(f);
        } else {
            let _ = store.set_status(&f.id, Status::Stale, None);
        }
    }
    Ok(good)
}

/// Compute a fact's evidence list: `path` relative to the workspace +
/// current sha256. Skips files that don't exist.
pub fn evidence_for(ws: &Path, paths: &[PathBuf]) -> Vec<serde_json::Value> {
    paths
        .iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(ws).ok().unwrap_or(p);
            file_sha256(p).map(|h| {
                serde_json::json!({"path": rel.display().to_string(), "sha256": h})
            })
        })
        .collect()
}
