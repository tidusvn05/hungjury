//! Shareable memory bundles — JSONL, diffable in git.
//!
//! Line 1 is a manifest:
//! `{"hungjury_bundle":1,"exported_at":"…","origin":"<machine_id>",
//!   "count":42,"includes_cases":false}`
//! Every following line is one entry (all `entries` columns except local
//! stats).
//!
//! Export defaults to `ruling` + `fact` only — precedents embed state
//! excerpts (possibly customer data) and need `--include-cases`. Import
//! verifies `id == sha256(content)`, skips duplicates and tombstones,
//! marks `source = imported`, `trust = original × trust_factor`, and
//! deterministically flags `contested` precedents (same scope + same
//! `state_digest` + different verdict — both sides contested, enqueued
//! for the judge).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::memory::store::{Entry, Kind, NewEntry, Source, Status, Store};

/// One bundle line: the serialized form of an `entries` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleEntry {
    /// Content-addressed id.
    pub id: String,
    /// ruling | precedent | fact.
    pub kind: String,
    /// `q:<qid>` | `ws:<repo_id>`.
    pub scope: String,
    /// Kind-specific body.
    pub body: serde_json::Value,
    /// FTS text.
    pub text: String,
    /// Original source (judge|human|imported) — re-stamped on import.
    pub source: String,
    /// Original trust — scaled by `trust_factor` on import.
    pub trust: f64,
    /// Original author.
    #[serde(default)]
    pub author: Option<String>,
    /// Original machine/bundle tag.
    #[serde(default)]
    pub origin: Option<String>,
    /// RFC3339.
    pub created_at: String,
}

impl From<&Entry> for BundleEntry {
    fn from(e: &Entry) -> Self {
        BundleEntry {
            id: e.id.clone(),
            kind: e.kind.as_str().to_string(),
            scope: e.scope.clone(),
            body: e.body.clone(),
            text: e.text.clone(),
            source: e.source.as_str().to_string(),
            trust: e.trust,
            author: e.author.clone(),
            origin: e.origin.clone(),
            created_at: e.created_at.clone(),
        }
    }
}

impl BundleEntry {
    /// Recompute the content id — must equal `self.id`.
    fn computed_id(&self) -> String {
        let canonical = serde_json::json!({
            "kind": self.kind,
            "scope": self.scope,
            "body": self.body,
        })
        .to_string();
        crate::util::sha256_str(&canonical)
    }

    fn to_new_entry(&self, trust_factor: f64, origin: &str) -> Option<NewEntry> {
        let kind = Kind::parse(&self.kind)?;
        Some(NewEntry {
            kind,
            scope: self.scope.clone(),
            body: self.body.clone(),
            text: self.text.clone(),
            source: Source::Imported,
            trust: self.trust * trust_factor,
            author: self.author.clone(),
            origin: Some(origin.to_string()),
        })
    }
}

/// Manifest line of a bundle file.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Bundle format version (1).
    pub hungjury_bundle: u32,
    /// RFC3339 export time.
    pub exported_at: String,
    /// Exporting machine id.
    pub origin: String,
    /// Entry lines that follow.
    pub count: usize,
    /// Whether precedents (case excerpts) are included.
    pub includes_cases: bool,
}

/// Export `ruling` + `fact` (+ `precedent` when `include_cases`) to JSONL.
/// `scope` limits the export to one scope.
/// Refuses facts whose evidence path is absolute (privacy rule §5.11).
pub fn export(
    store: &Store,
    out: &Path,
    scope: Option<&str>,
    include_cases: bool,
) -> Result<usize> {
    let mut entries = Vec::new();
    for kind in [Kind::Ruling, Kind::Fact] {
        entries.extend(store.list(Some(kind), scope, false)?);
    }
    if include_cases {
        entries.extend(store.list(Some(Kind::Precedent), scope, false)?);
    }
    // Tombstones travel: they must suppress resurrection on the far side.
    entries.retain(|e| {
        if e.kind == Kind::Fact
            && let Some(ev) = e.body["evidence"].as_array()
        {
            return ev.iter().all(|x| {
                !std::path::Path::new(x["path"].as_str().unwrap_or("")).is_absolute()
            });
        }
        true
    });
    let mut lines = String::new();
    let manifest = Manifest {
        hungjury_bundle: 1,
        exported_at: crate::quota::now_rfc3339(),
        origin: store.machine_id()?,
        count: entries.len(),
        includes_cases: include_cases,
    };
    lines.push_str(&serde_json::to_string(&manifest).map_err(|e| Error::Memory(e.to_string()))?);
    lines.push('\n');
    for e in &entries {
        lines.push_str(
            &serde_json::to_string(&BundleEntry::from(e))
                .map_err(|e2| Error::Memory(e2.to_string()))?,
        );
        lines.push('\n');
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(out, &lines).map_err(|e| Error::io(out, e))?;
    Ok(entries.len())
}

/// What an import would do / did.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Entries actually inserted.
    pub new: usize,
    /// Already-known ids skipped.
    pub duplicate: usize,
    /// Ids that are local tombstones (skipped).
    pub tombstoned: usize,
    /// Precedents that contradict a same-digest local one.
    pub contested: usize,
    /// Malformed lines / bad hashes skipped.
    pub rejected: usize,
}

/// Parse a bundle file → `(manifest, entries, bad_line_count)`.
/// Line 1 must be the manifest.
pub fn read_bundle_counting(path: &Path) -> Result<(Manifest, Vec<BundleEntry>, usize)> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let mut lines = text.lines();
    let manifest_line = lines
        .next()
        .ok_or_else(|| Error::Memory(format!("{}: empty bundle", path.display())))?;
    let manifest: Manifest = serde_json::from_str(manifest_line).map_err(|e| {
        Error::Memory(format!("{}: bad manifest: {e}", path.display()))
    })?;
    if manifest.hungjury_bundle != 1 {
        return Err(Error::Memory(format!(
            "{}: not a hungjury bundle",
            path.display()
        )));
    }
    let mut entries = Vec::new();
    let mut bad = 0usize;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<BundleEntry>(line) {
            Ok(e) => entries.push(e),
            Err(_) => bad += 1,
        }
    }
    Ok((manifest, entries, bad))
}

/// Import one bundle file. `dry_run` reports without writing.
/// Rulings over 300 chars are rejected (§6.2 injection hygiene).
pub fn import(
    store: &Store,
    path: &Path,
    trust_factor: f64,
    dry_run: bool,
) -> Result<ImportReport> {
    let (manifest, entries, rejected) = read_bundle_counting(path)?;
    let origin = manifest.origin.clone();
    let mut report = ImportReport {
        rejected,
        ..Default::default()
    };
    for e in entries {
        if e.computed_id() != e.id {
            report.rejected += 1;
            continue;
        }
        if e.kind == "ruling"
            && e.body["text"].as_str().map(|t| t.len() > 300).unwrap_or(false)
        {
            report.rejected += 1;
            continue;
        }
        if let Some(existing) = store.get(&e.id)? {
            if existing.status == Status::Forgotten {
                report.tombstoned += 1;
            } else {
                report.duplicate += 1;
            }
            continue;
        }
        let Some(ne) = e.to_new_entry(trust_factor, &origin) else {
            report.rejected += 1;
            continue;
        };
        // Contested check: a precedent with the same scope + state_digest
        // but a different verdict ⇒ both become contested, enqueued.
        if ne.kind == Kind::Precedent {
            let digest = ne.body["state_digest"].as_str().unwrap_or("");
            let verdict = ne.body["verdict"].to_string();
            if !digest.is_empty() {
                let clash = store
                    .list(Some(Kind::Precedent), Some(&ne.scope), false)?
                    .into_iter()
                    .filter(|l| l.status == Status::Active || l.status == Status::Contested)
                    .find(|l| {
                        l.body["state_digest"].as_str() == Some(digest)
                            && l.body["verdict"] != verdict
                    });
                if let Some(local) = clash {
                    report.contested += 1;
                    if !dry_run {
                        let _ = store.set_status(&local.id, Status::Contested, None);
                        let (id, _) = store.insert(&ne)?;
                        let _ = store.set_status(&id, Status::Contested, None);
                        let _ = store.enqueue(&id, "contested precedent (import)");
                    }
                    continue;
                }
            }
        }
        report.new += 1;
        if !dry_run {
            let _ = store.insert(&ne)?;
        }
    }
    Ok(report)
}
