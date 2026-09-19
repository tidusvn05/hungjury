//! Small shared helpers: atomic writes, hashing, ids.

use std::io::Write;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Write `bytes` to `path` atomically: temp file in the same directory,
/// then rename. A crash leaves either the old or the new content — a
/// reader never sees a torn file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(dir, e))?;
    tmp.write_all(bytes).map_err(|e| Error::io(path, e))?;
    tmp.persist(path).map_err(|e| Error::io(path, e.error))?;
    Ok(())
}

/// Lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `sha256_hex` of a string.
pub fn sha256_str(s: &str) -> String {
    sha256_hex(s.as_bytes())
}

/// Decision id: `dec_` + 24 hex chars of a time-seeded hash.
/// Sortable enough within a run, collision-safe enough for a local log.
pub fn new_decision_id() -> String {
    let mut h = Sha256::new();
    h.update(crate::quota::now_rfc3339().as_bytes());
    h.update(std::process::id().to_be_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_be_bytes())
            .unwrap_or([0; 16]),
    );
    format!("dec_{}", &hex::encode(h.finalize())[..24])
}

/// Random nonce for the state wrapper tag (6 hex chars), seeded from time +
/// pid — not cryptographic, just unpredictable to the state author.
pub fn nonce() -> String {
    let mut h = Sha256::new();
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_be_bytes())
            .unwrap_or([0; 16]),
    );
    h.update(std::process::id().to_be_bytes());
    hex::encode(h.finalize())[..6].to_string()
}
