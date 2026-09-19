//! Decision cache: `sha256(canonical request ‖ jury config ‖ memory_epoch ‖
//! SCHEMA_VERSION)` → `<data_dir>/cache/<key>.json` holding a full Response.
//!
//! `memory_epoch` is a hash of the active entry ids in the scopes the
//! request touches, so any memory write invalidates affected entries
//! automatically. Workspace states add `git HEAD` + hashes of
//! `git status --porcelain` and `git diff`; a non-git workspace is never
//! cached.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Bumped when response/prompt semantics change; part of every cache key.
pub const SCHEMA_VERSION: &str = "1";

/// On-disk record for one cached decision.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Serialized Response JSON.
    pub response: serde_json::Value,
    /// When it was produced (RFC3339).
    pub created_at: String,
}

/// Filesystem cache rooted at `<data_dir>/cache`.
pub struct Cache {
    dir: PathBuf,
    /// `--no-cache`: skip both reads and writes.
    disabled: bool,
    /// `--refresh`: skip reads, still write.
    no_read: bool,
}

impl Cache {
    /// Create a cache under `data_dir`.
    pub fn new(data_dir: &Path, disabled: bool, no_read: bool) -> Self {
        Self {
            dir: data_dir.join("cache"),
            disabled,
            no_read,
        }
    }

    /// The cache key for a decision.
    pub fn key(
        canonical_request: &str,
        jury_config: &str,
        memory_epoch: &str,
        workspace_stamp: Option<&str>,
    ) -> String {
        let mut h = Sha256::new();
        for part in [
            canonical_request,
            jury_config,
            memory_epoch,
            workspace_stamp.unwrap_or(""),
            SCHEMA_VERSION,
        ] {
            h.update(part.as_bytes());
            h.update(b"\x00");
        }
        hex::encode(h.finalize())
    }

    /// Look up a cached decision.
    pub fn get(&self, key: &str) -> Option<CacheEntry> {
        if self.disabled || self.no_read {
            return None;
        }
        let path = self.dir.join(format!("{key}.json"));
        let text = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Persist a decision.
    pub fn put(&self, key: &str, response: &serde_json::Value) -> Result<()> {
        if self.disabled {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        let path = self.dir.join(format!("{key}.json"));
        let body = serde_json::to_string(&CacheEntry {
            response: response.clone(),
            created_at: crate::quota::now_rfc3339(),
        })
        .map_err(|e| Error::Config(format!("cache serialize: {e}")))?;
        crate::util::write_atomic(&path, body.as_bytes())
    }
}
