//! SQLite memory store — one file, synchronous queries (fast enough; it is
//! consulted in-process before any agent is spawned).
//!
//! `entries` is append-only and content-addressed (`id = sha256({kind,
//! scope, body})`), so merge is set-union and re-import never duplicates.
//! "Editing" = a new entry plus `superseded_by`; `forget` tombstones the id
//! (clears body/text, keeps the row) so a later import can't resurrect it.
//!
//! `entry_stats`, `decisions`, `queue`, `juror_stats`, `meta` are local
//! only — never exported.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::sha256_str;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  scope TEXT NOT NULL,
  body TEXT NOT NULL,
  text TEXT NOT NULL,
  source TEXT NOT NULL,
  trust REAL NOT NULL,
  author TEXT,
  origin TEXT,
  created_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  superseded_by TEXT
);
CREATE INDEX IF NOT EXISTS entries_scope ON entries(scope, kind, status);
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
  text, content='entries', content_rowid='rowid'
);
CREATE TRIGGER IF NOT EXISTS entries_ai AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS entries_ad AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, text) VALUES('delete', old.rowid, old.text);
END;
CREATE TRIGGER IF NOT EXISTS entries_au AFTER UPDATE OF text ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, text) VALUES('delete', old.rowid, old.text);
  INSERT INTO entries_fts(rowid, text) VALUES (new.rowid, new.text);
END;

CREATE TABLE IF NOT EXISTS entry_stats (
  entry_id TEXT PRIMARY KEY,
  used INTEGER NOT NULL DEFAULT 0,
  last_used_at TEXT
);
CREATE TABLE IF NOT EXISTS decisions (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  request TEXT NOT NULL,
  response TEXT NOT NULL,
  decided_by TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS decisions_created ON decisions(created_at);
CREATE TABLE IF NOT EXISTS queue (
  decision_id TEXT PRIMARY KEY,
  reason TEXT NOT NULL,
  created_at TEXT NOT NULL,
  done_at TEXT
);
CREATE TABLE IF NOT EXISTS juror_stats (
  juror TEXT NOT NULL,
  qid TEXT NOT NULL,
  n INTEGER NOT NULL DEFAULT 0,
  agree INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(juror, qid)
);
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

const SCHEMA_VERSION: &str = "1";

/// Entry kind — knowledge attaches to questions (`q:`) or workspaces (`ws:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// General interpretation rule distilled by the judge.
    Ruling,
    /// State excerpt + verdict + rationale; few-shot material.
    Precedent,
    /// Workspace knowledge with evidence files.
    Fact,
}

impl Kind {
    /// String form used in the db.
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Ruling => "ruling",
            Kind::Precedent => "precedent",
            Kind::Fact => "fact",
        }
    }

    /// Parse from the db string.
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "ruling" => Some(Kind::Ruling),
            "precedent" => Some(Kind::Precedent),
            "fact" => Some(Kind::Fact),
            _ => None,
        }
    }
}

/// Who wrote the entry — trust derives from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The high-tier judge model.
    Judge,
    /// A human (`feedback` command).
    Human,
    /// Imported from a bundle.
    Imported,
}

impl Source {
    /// String form used in the db.
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Judge => "judge",
            Source::Human => "human",
            Source::Imported => "imported",
        }
    }

    /// Parse from the db string.
    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "judge" => Some(Source::Judge),
            "human" => Some(Source::Human),
            "imported" => Some(Source::Imported),
            _ => None,
        }
    }

    /// Default trust for this source.
    pub fn base_trust(&self) -> f64 {
        match self {
            Source::Human => 1.0,
            Source::Judge => 0.8,
            Source::Imported => 0.4,
        }
    }
}

/// Lifecycle status of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Usable.
    Active,
    /// Replaced by a newer entry (`superseded_by`).
    Superseded,
    /// Contradicted by another precedent — pending judge resolution.
    Contested,
    /// Evidence files changed underneath a `fact`.
    Stale,
    /// Tombstoned by `forget` — body cleared, id kept.
    Forgotten,
}

impl Status {
    /// String form used in the db.
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Superseded => "superseded",
            Status::Contested => "contested",
            Status::Stale => "stale",
            Status::Forgotten => "forgotten",
        }
    }

    /// Parse from the db string.
    pub fn parse(s: &str) -> Status {
        match s {
            "superseded" => Status::Superseded,
            "contested" => Status::Contested,
            "stale" => Status::Stale,
            "forgotten" => Status::Forgotten,
            _ => Status::Active,
        }
    }
}

/// A stored `decisions` row (parsed `request`/`response` JSON).
pub struct DecisionRow {
    /// `dec_…` id.
    pub id: String,
    /// RFC3339 timestamp.
    pub created_at: String,
    /// `jury` | `judge` | `cache`.
    pub decided_by: String,
    /// The original request JSON.
    pub request: serde_json::Value,
    /// The stored response JSON.
    pub response: serde_json::Value,
}

/// An entry as stored.
#[derive(Debug, Clone)]
pub struct Entry {
    /// `sha256({kind,scope,body})` — content-addressed.
    pub id: String,
    /// ruling | precedent | fact.
    pub kind: Kind,
    /// `q:<qid>` | `ws:<repo_id>`.
    pub scope: String,
    /// Kind-specific JSON body.
    pub body: serde_json::Value,
    /// Plain text indexed by FTS.
    pub text: String,
    /// judge | human | imported.
    pub source: Source,
    /// Trust in [0,1].
    pub trust: f64,
    /// Model string or human name.
    pub author: Option<String>,
    /// Machine/bundle origin.
    pub origin: Option<String>,
    /// RFC3339.
    pub created_at: String,
    /// Lifecycle.
    pub status: Status,
    /// Replacement entry id, when superseded.
    pub superseded_by: Option<String>,
}

/// Fields needed to create an entry; `id`/`created_at` are derived.
#[derive(Debug, Clone)]
pub struct NewEntry {
    /// Kind.
    pub kind: Kind,
    /// Scope string.
    pub scope: String,
    /// Kind-specific body.
    pub body: serde_json::Value,
    /// FTS text.
    pub text: String,
    /// Source.
    pub source: Source,
    /// Trust (base trust × import factor when relevant).
    pub trust: f64,
    /// Author.
    pub author: Option<String>,
    /// Origin tag.
    pub origin: Option<String>,
}

impl NewEntry {
    /// Content-addressed id: `sha256` of canonical `{kind,scope,body}`.
    pub fn id(&self) -> String {
        let canonical = serde_json::json!({
            "kind": self.kind.as_str(),
            "scope": self.scope,
            "body": self.body,
        })
        .to_string();
        sha256_str(&canonical)
    }
}

/// Scope helpers.
pub fn q_scope(qid: &str) -> String {
    format!("q:{qid}")
}

/// Scope helpers.
pub fn ws_scope(repo_id: &str) -> String {
    format!("ws:{repo_id}")
}

/// The store — wraps a `Mutex<Connection>` so `&Store` is `Sync`.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (and migrate) the db at `path`.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| Error::Memory(format!("open {}: {e}", path.display())))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Memory(format!("schema: {e}")))?;
        let store = Store {
            conn: Mutex::new(conn),
        };
        store.set_meta_if_absent("schema_version", SCHEMA_VERSION)?;
        store.machine_id()?;
        Ok(store)
    }

    /// In-memory store for tests.
    #[allow(dead_code)]
    pub fn open_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Memory(format!("open memory db: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Memory(format!("schema: {e}")))?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// Does this SQLite build have FTS5 (compile-time check for `doctor`)?
    pub fn has_fts5(&self) -> bool {
        self.with_conn(|c| {
            c.execute_batch(
                "CREATE VIRTUAL TABLE temp.fts5_probe USING fts5(x);
                 DROP TABLE temp.fts5_probe;",
            )
            .map_err(|e| Error::Memory(e.to_string()))
        })
        .is_ok()
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Memory(format!("lock: {e}")))?;
        f(&conn)
    }

    fn set_meta_if_absent(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO meta(key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|e| Error::Memory(format!("meta insert: {e}")))?;
            Ok(())
        })
    }

    /// Meta value.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT value FROM meta WHERE key = ?1")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let mut rows = st
                .query_map([key], |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.next().transpose().map_err(|e| {
                Error::Memory(e.to_string())
            })
        })
    }

    /// Stable machine id (created on first open), used as entry `origin`.
    pub fn machine_id(&self) -> Result<String> {
        if let Some(id) = self.meta("machine_id")? {
            return Ok(id);
        }
        let id = format!("m_{}", &crate::util::sha256_str(&crate::quota::now_rfc3339())[..16]);
        self.set_meta_if_absent("machine_id", &id)?;
        Ok(id)
    }

    /// Insert an entry; idempotent by content id. Returns `(id, inserted?)`.
    /// Tombstoned ids stay tombstones — a re-import never resurrects them.
    pub fn insert(&self, e: &NewEntry) -> Result<(String, bool)> {
        let id = e.id();
        self.with_conn(|c| {
            let existing: Option<String> = c
                .query_row(
                    "SELECT status FROM entries WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .ok();
            if existing.is_some() {
                return Ok((id, false));
            }
            c.execute(
                "INSERT INTO entries(id, kind, scope, body, text, source, trust,
                                     author, origin, created_at, status)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'active')",
                params![
                    id,
                    e.kind.as_str(),
                    e.scope,
                    e.body.to_string(),
                    e.text,
                    e.source.as_str(),
                    e.trust,
                    e.author,
                    e.origin,
                    crate::quota::now_rfc3339(),
                ],
            )
            .map_err(|e2| Error::Memory(format!("insert: {e2}")))?;
            Ok((id, true))
        })
    }

    /// Fetch one entry by id.
    pub fn get(&self, id: &str) -> Result<Option<Entry>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare(
                    "SELECT id, kind, scope, body, text, source, trust, author,
                            origin, created_at, status, superseded_by
                     FROM entries WHERE id = ?1",
                )
                .map_err(|e| Error::Memory(e.to_string()))?;
            let mut rows = st
                .query_map([id], row_to_entry)
                .map_err(|e| Error::Memory(e.to_string()))?;
            match rows.next() {
                Some(r) => Ok(Some(r.map_err(|e| Error::Memory(e.to_string()))?)),
                None => Ok(None),
            }
        })
    }

    /// Active rulings for `q:<qid>`, trust desc, capped.
    pub fn rulings(&self, qid: &str, limit: usize) -> Result<Vec<Entry>> {
        self.select_entries(
            "SELECT id, kind, scope, body, text, source, trust, author, origin,
                    created_at, status, superseded_by
             FROM entries
             WHERE scope = ?1 AND kind = 'ruling' AND status = 'active'
             ORDER BY trust DESC, created_at ASC
             LIMIT ?2",
            params![q_scope(qid), limit as i64],
        )
    }

    /// ALL active rulings for a scope (no limit — for consolidate/bundle).
    pub fn all_rulings(&self, qid: &str) -> Result<Vec<Entry>> {
        self.select_entries(
            "SELECT id, kind, scope, body, text, source, trust, author, origin,
                    created_at, status, superseded_by
             FROM entries
             WHERE scope = ?1 AND kind = 'ruling' AND status = 'active'
             ORDER BY trust DESC, created_at ASC",
            params![q_scope(qid)],
        )
    }

    /// FTS5 BM25 precedents for `q:<qid>` matching `fts_query`, top `k`.
    pub fn precedents(&self, qid: &str, fts_query: &str, k: usize) -> Result<Vec<Entry>> {
        if fts_query.trim().is_empty() {
            return Ok(vec![]);
        }
        self.select_entries(
            "SELECT e.id, e.kind, e.scope, e.body, e.text, e.source, e.trust,
                    e.author, e.origin, e.created_at, e.status, e.superseded_by
             FROM entries e
             JOIN entries_fts f ON e.rowid = f.rowid
             WHERE e.scope = ?1 AND e.kind = 'precedent' AND e.status = 'active'
               AND entries_fts MATCH ?2
             ORDER BY bm25(entries_fts)
             LIMIT ?3",
            params![q_scope(qid), fts_query, k as i64],
        )
    }

    /// Active facts for `ws:<repo_id>`.
    pub fn facts(&self, repo_id: &str) -> Result<Vec<Entry>> {
        self.select_entries(
            "SELECT id, kind, scope, body, text, source, trust, author, origin,
                    created_at, status, superseded_by
             FROM entries
             WHERE scope = ?1 AND kind = 'fact' AND status = 'active'
             ORDER BY trust DESC, created_at ASC",
            params![ws_scope(repo_id)],
        )
    }

    /// FTS search across all active entries (for `memory search`).
    pub fn search(&self, fts_query: &str, scope: Option<&str>) -> Result<Vec<Entry>> {
        if fts_query.trim().is_empty() {
            return Ok(vec![]);
        }
        let scope_clause = scope
            .map(|_| "AND e.scope = ?3")
            .unwrap_or("");
        let sql = format!(
            "SELECT e.id, e.kind, e.scope, e.body, e.text, e.source, e.trust,
                    e.author, e.origin, e.created_at, e.status, e.superseded_by
             FROM entries e
             JOIN entries_fts f ON e.rowid = f.rowid
             WHERE e.status = 'active' AND entries_fts MATCH ?1 {scope_clause}
             ORDER BY bm25(entries_fts)
             LIMIT ?2"
        );
        match scope {
            Some(s) => self.select_entries(&sql, params![fts_query, 50i64, s]),
            None => self.select_entries(&sql, params![fts_query, 50i64]),
        }
    }

    /// List entries (for `memory list` / export), newest last.
    pub fn list(&self, kind: Option<Kind>, scope: Option<&str>, active_only: bool) -> Result<Vec<Entry>> {
        let mut sql = String::from(
            "SELECT id, kind, scope, body, text, source, trust, author, origin,
                    created_at, status, superseded_by FROM entries WHERE 1=1",
        );
        let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(k) = kind {
            sql.push_str(" AND kind = ?");
            bind.push(Box::new(k.as_str().to_string()));
        }
        if let Some(s) = scope {
            sql.push_str(" AND scope = ?");
            bind.push(Box::new(s.to_string()));
        }
        if active_only {
            sql.push_str(" AND status = 'active'");
        }
        sql.push_str(" ORDER BY created_at ASC");
        self.with_conn(|c| {
            let mut st = c.prepare(&sql).map_err(|e| Error::Memory(e.to_string()))?;
            let refs: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
            let rows = st
                .query_map(refs.as_slice(), row_to_entry)
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Ids of active entries inside the given scopes (for `memory_epoch`).
    pub fn active_ids(&self, scopes: &[String]) -> Result<Vec<String>> {
        if scopes.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = scopes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id FROM entries WHERE status = 'active' AND scope IN ({placeholders}) ORDER BY id"
        );
        self.with_conn(|c| {
            let mut st = c.prepare(&sql).map_err(|e| Error::Memory(e.to_string()))?;
            let refs: Vec<&dyn rusqlite::types::ToSql> = scopes
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            let rows = st
                .query_map(refs.as_slice(), |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Count active precedents in a scope (consolidate trigger).
    #[allow(dead_code)]
    pub fn count_active(&self, scope: &str, kind: Kind) -> Result<usize> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT count(*) FROM entries WHERE scope = ?1 AND kind = ?2 AND status = 'active'",
                params![scope, kind.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Resolve an id prefix (the short `[id:…]` tags shown to the judge)
    /// to a full entry id — `None` when absent, ambiguous, or the prefix
    /// is too short to be safe (<4 chars).
    pub fn id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        if prefix.len() < 4 {
            return Ok(None);
        }
        let like = format!("{}%", prefix.replace(['%', '_'], ""));
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT id FROM entries WHERE id LIKE ?1 LIMIT 2")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([like], |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            let ids: Vec<String> = rows
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| Error::Memory(e.to_string()))?;
            Ok((ids.len() == 1).then(|| ids.into_iter().next().unwrap()))
        })
    }

    /// Same prefix rules as `id_by_prefix`, for the `decisions` table —
    /// lets `feedback`/`memory` commands take the short id printed by
    /// `decisions --last`.
    pub fn decision_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        if prefix.len() < 4 {
            return Ok(None);
        }
        let like = format!("{}%", prefix.replace(['%', '_'], ""));
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT id FROM decisions WHERE id LIKE ?1 LIMIT 2")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([like], |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            let ids: Vec<String> = rows
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| Error::Memory(e.to_string()))?;
            Ok((ids.len() == 1).then(|| ids.into_iter().next().unwrap()))
        })
    }

    /// Raise judge-sourced rulings on `scope` up to `trust`. Only
    /// judge-written entries below the target are touched — human or
    /// imported rulings keep their own trust.
    pub fn promote_rulings(&self, scope: &str, trust: f64) -> Result<usize> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE entries SET trust = ?2
                 WHERE scope = ?1 AND kind = 'ruling' AND status = 'active'
                   AND source = 'judge' AND trust < ?2",
                params![scope, trust],
            )
            .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Active rulings older than `days` become `stale`. `0` ⇒ no-op.
    /// Returns how many entries expired.
    pub fn expire_rulings(&self, days: u32) -> Result<usize> {
        if days == 0 {
            return Ok(0);
        }
        let secs = days as i64 * 86_400;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        // RFC3339 UTC — lexical order matches chronological order.
        self.expire_rulings_before(&crate::util::rfc3339_at(now - secs))
    }

    /// Active rulings created before `cutoff` (RFC3339) become `stale`.
    /// Split out from [`expire_rulings`] so tests can pick the cutoff.
    pub fn expire_rulings_before(&self, cutoff: &str) -> Result<usize> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE entries SET status = 'stale'
                 WHERE kind = 'ruling' AND status = 'active' AND created_at < ?1",
                params![cutoff],
            )
            .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Change an entry's status (and optional `superseded_by`).
    pub fn set_status(&self, id: &str, status: Status, superseded_by: Option<&str>) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE entries SET status = ?2, superseded_by = ?3 WHERE id = ?1",
                params![id, status.as_str(), superseded_by],
            )
            .map_err(|e| Error::Memory(e.to_string()))?;
            Ok(())
        })
    }

    /// `forget`: tombstone — clear body/text, keep the id.
    pub fn forget(&self, id: &str) -> Result<bool> {
        self.with_conn(|c| {
            let n = c
                .execute(
                    "UPDATE entries SET status = 'forgotten', body = '', text = '',
                                       superseded_by = NULL
                     WHERE id = ?1 AND status != 'forgotten'",
                    params![id],
                )
                .map_err(|e| Error::Memory(e.to_string()))?;
            Ok(n > 0)
        })
    }

    /// Record entry use (local stats only).
    pub fn mark_used(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.with_conn(|c| {
            let now = crate::quota::now_rfc3339();
            for id in ids {
                c.execute(
                    "INSERT INTO entry_stats(entry_id, used, last_used_at) VALUES (?1, 1, ?2)
                     ON CONFLICT(entry_id) DO UPDATE SET used = used + 1, last_used_at = ?2",
                    params![id, now],
                )
                .map_err(|e| Error::Memory(e.to_string()))?;
            }
            Ok(())
        })
    }

    /// Record a finished decision.
    pub fn record_decision(
        &self,
        id: &str,
        request_hash: &str,
        request: &serde_json::Value,
        response: &serde_json::Value,
        decided_by: &str,
    ) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO decisions(id, created_at, request_hash, request, response, decided_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    crate::quota::now_rfc3339(),
                    request_hash,
                    request.to_string(),
                    response.to_string(),
                    decided_by,
                ],
            )
            .map_err(|e| Error::Memory(e.to_string()))?;
            Ok(())
        })
    }

    /// A stored decision `(request_json, response_json, decided_by)`.
    pub fn get_decision(&self, id: &str) -> Result<Option<(serde_json::Value, serde_json::Value, String)>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT request, response, decided_by FROM decisions WHERE id = ?1")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let mut rows = st
                .query_map([id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| Error::Memory(e.to_string()))?;
            match rows.next() {
                Some(r) => {
                    let (req, resp, by) = r.map_err(|e| Error::Memory(e.to_string()))?;
                    Ok(Some((
                        serde_json::from_str(&req).unwrap_or(serde_json::Value::Null),
                        serde_json::from_str(&resp).unwrap_or(serde_json::Value::Null),
                        by,
                    )))
                }
                None => Ok(None),
            }
        })
    }

    /// `n` random jury-decided decisions (for `learn --audit`).
    pub fn sample_jury_decisions(&self, n: usize) -> Result<Vec<String>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare(
                    "SELECT id FROM decisions WHERE decided_by = 'jury' ORDER BY RANDOM() LIMIT ?1",
                )
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([n as i64], |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// `n` most recent jury-decided decisions (for `learn --audit --recent`).
    pub fn recent_jury_decisions(&self, n: usize) -> Result<Vec<String>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare(
                    "SELECT id FROM decisions WHERE decided_by = 'jury'
                     ORDER BY created_at DESC, rowid DESC LIMIT ?1",
                )
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([n as i64], |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Recent decisions, newest first.
    pub fn list_decisions(&self, limit: usize) -> Result<Vec<DecisionRow>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare(
                    "SELECT id, created_at, decided_by, request, response FROM decisions
                     ORDER BY created_at DESC, rowid DESC LIMIT ?1",
                )
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([limit as i64], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
                .map(|v| {
                    v.into_iter()
                        .map(|(id, ts, by, req, resp)| DecisionRow {
                            id,
                            created_at: ts,
                            decided_by: by,
                            request: serde_json::from_str(&req)
                                .unwrap_or(serde_json::Value::Null),
                            response: serde_json::from_str(&resp)
                                .unwrap_or(serde_json::Value::Null),
                        })
                        .collect()
                })
        })
    }

    /// Enqueue a decision for offline learning.
    pub fn enqueue(&self, decision_id: &str, reason: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO queue(decision_id, reason, created_at) VALUES (?1, ?2, ?3)",
                params![decision_id, reason, crate::quota::now_rfc3339()],
            )
            .map_err(|e| Error::Memory(e.to_string()))?;
            Ok(())
        })
    }

    /// Pending queue ids.
    pub fn queue_pending(&self) -> Result<Vec<String>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT decision_id FROM queue WHERE done_at IS NULL ORDER BY created_at")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Mark a queue item done.
    pub fn queue_done(&self, decision_id: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE queue SET done_at = ?2 WHERE decision_id = ?1",
                params![decision_id, crate::quota::now_rfc3339()],
            )
            .map_err(|e| Error::Memory(e.to_string()))?;
            Ok(())
        })
    }

    /// Update `juror_stats`: the juror agreed (or not) with ground truth.
    pub fn juror_stats_update(&self, juror: &str, qid: &str, agreed: bool) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO juror_stats(juror, qid, n, agree) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(juror, qid) DO UPDATE SET n = n + 1, agree = agree + ?3",
                params![juror, qid, i64::from(agreed)],
            )
            .map_err(|e| Error::Memory(e.to_string()))?;
            Ok(())
        })
    }

    /// Vote weight for `(juror, qid)`: `(agree+1)/(n+2)` once `n ≥ 10`,
    /// else 1.0.
    pub fn juror_weight(&self, juror: &str, qid: &str) -> Result<f64> {
        self.with_conn(|c| {
            let row = c
                .query_row(
                    "SELECT n, agree FROM juror_stats WHERE juror = ?1 AND qid = ?2",
                    params![juror, qid],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .ok();
            Ok(match row {
                Some((n, agree)) if n >= 10 => (agree as f64 + 1.0) / (n as f64 + 2.0),
                _ => 1.0,
            })
        })
    }

    /// Raw juror stats rows for `memory stats`.
    pub fn juror_stats_rows(&self) -> Result<Vec<(String, String, i64, i64)>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT juror, qid, n, agree FROM juror_stats ORDER BY juror, qid")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Entry counts by `(kind, status)` for `memory stats`.
    pub fn counts(&self) -> Result<Vec<(String, String, i64)>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT kind, status, count(*) FROM entries GROUP BY kind, status")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Entry counts grouped by source.
    pub fn source_counts(&self) -> Result<Vec<(String, i64)>> {
        self.with_conn(|c| {
            let mut st = c
                .prepare("SELECT source, count(*) FROM entries GROUP BY source")
                .map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Queue length (pending).
    pub fn queue_len(&self) -> Result<usize> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT count(*) FROM queue WHERE done_at IS NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    /// Decisions count.
    pub fn decisions_len(&self) -> Result<usize> {
        self.with_conn(|c| {
            c.query_row("SELECT count(*) FROM decisions", [], |r| r.get::<_, i64>(0))
                .map(|n| n as usize)
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }

    fn select_entries(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Entry>> {
        self.with_conn(|c| {
            let mut st = c.prepare(sql).map_err(|e| Error::Memory(e.to_string()))?;
            let rows = st
                .query_map(params, row_to_entry)
                .map_err(|e| Error::Memory(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Memory(e.to_string()))
        })
    }
}

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let body_str: String = r.get(3)?;
    Ok(Entry {
        id: r.get(0)?,
        kind: Kind::parse(&r.get::<_, String>(1)?).unwrap_or(Kind::Ruling),
        scope: r.get(2)?,
        body: serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null),
        text: r.get(4)?,
        source: Source::parse(&r.get::<_, String>(5)?).unwrap_or(Source::Imported),
        trust: r.get(6)?,
        author: r.get(7)?,
        origin: r.get(8)?,
        created_at: r.get(9)?,
        status: Status::parse(&r.get::<_, String>(10)?),
        superseded_by: r.get(11)?,
    })
}
