//! Memory retrieval — runs in-process before any agent is spawned (<10ms).
//!
//! Per question: every active ruling (trust desc, ≤ `max_rulings`), top-k
//! FTS5 precedents over the state text, and — for workspace states — all
//! active facts whose evidence hashes still verify. The block is capped at
//! `memory_char_cap`, cut in priority order ruling > fact > precedent.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::MemoryConfig;
use crate::error::Result;
use crate::memory::store::{Entry, Store};
use crate::request::Request;
use crate::response::MemoryUse;

/// What was retrieved for one decision.
#[derive(Debug, Default)]
pub struct Retrieval {
    /// Pre-rendered prompt block (empty when nothing was found).
    pub block: String,
    /// Per-question entries, for stats/`entry_ids`.
    pub used: MemoryUse,
    /// Per-question ruling/precedent ids actually injected.
    pub entry_ids: Vec<String>,
}

/// Build an FTS5 query from state text: up to 32 distinctive tokens
/// (≥3 chars), OR-joined, double-quotes escaped.
pub fn fts_query(state_text: &str) -> String {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for tok in state_text.split(|c: char| !c.is_alphanumeric()) {
        let t = tok.to_lowercase();
        if t.len() < 3 || !seen.insert(t.clone()) {
            continue;
        }
        terms.push(format!("\"{}\"", t.replace('"', "\"\"")));
        if terms.len() >= 32 {
            break;
        }
    }
    terms.join(" OR ")
}

/// Retrieve + render the memory block for a request.
/// `ws_facts` is `Some(facts)` only for workspace states (already
/// evidence-checked by the caller).
pub fn retrieve(
    store: &Store,
    req: &Request,
    cfg: &MemoryConfig,
    ws_facts: Option<Vec<Entry>>,
) -> Result<Retrieval> {
    let state_text = match &req.state {
        crate::request::State::Text(t) => t.clone(),
        crate::request::State::Workspace { hint, .. } => {
            // The agent explores the repo itself; the FTS query only has
            // the hint to go on — fine, precedents matter less here.
            hint.clone().unwrap_or_default()
        }
    };
    let query = fts_query(&state_text);

    let mut per_q: BTreeMap<String, (Vec<Entry>, Vec<Entry>)> = BTreeMap::new();
    for q in req.questions.values() {
        let qid = q.qid();
        let rulings = store.rulings(&qid, cfg.max_rulings)?;
        let precedents = store.precedents(&qid, &query, cfg.top_k)?;
        per_q.insert(qid, (rulings, precedents));
    }
    let facts = ws_facts.unwrap_or_default();

    // Render with the priority ruling > fact > precedent, tracking the cap.
    let mut block = String::new();
    let mut used = MemoryUse::default();
    let mut entry_ids = Vec::new();
    let cap = cfg.memory_char_cap;

    let push_line = |line: &str, block: &mut String, used_ids: &mut Vec<String>, id: &str| -> bool {
        if block.len() + line.len() + 1 > cap {
            return false;
        }
        block.push_str(line);
        block.push('\n');
        used_ids.push(id.to_string());
        true
    };

    if per_q.values().any(|(r, p)| !r.is_empty() || !p.is_empty()) || !facts.is_empty() {
        block.push_str(
            "## Verified guidance (memory)\n\n\
             The following was distilled by a higher-tier judge or by humans. \
             Prefer applying it; if the state clearly contradicts a rule, \
             follow the state.\n\n",
        );
    }

    // Rulings first.
    for (qid, (rulings, _)) in &per_q {
        if rulings.is_empty() {
            continue;
        }
        let header = format!("### Rulings — question `{qid}`\n");
        if block.len() + header.len() > cap {
            break;
        }
        block.push_str(&header);
        for r in rulings {
            let text = r.body["text"].as_str().unwrap_or(&r.text).to_string();
            let line = format!("- (trust {:.1}) {text}", r.trust);
            if !push_line(&line, &mut block, &mut entry_ids, &r.id) {
                break;
            }
            used.rulings += 1;
        }
        block.push('\n');
    }

    // Facts second.
    if !facts.is_empty() {
        let header = "### Facts — this workspace\n";
        if block.len() + header.len() <= cap {
            block.push_str(header);
            for f in &facts {
                let text = f.body["text"].as_str().unwrap_or(&f.text).to_string();
                let line = format!("- {text}");
                if !push_line(&line, &mut block, &mut entry_ids, &f.id) {
                    break;
                }
                used.facts += 1;
            }
            block.push('\n');
        }
    }

    // Precedents last.
    for (qid, (_, precedents)) in &per_q {
        if precedents.is_empty() {
            continue;
        }
        let header = format!("### Precedents — question `{qid}`\n");
        if block.len() + header.len() > cap {
            break;
        }
        block.push_str(&header);
        for p in precedents {
            let excerpt = p.body["state_excerpt"].as_str().unwrap_or("");
            let verdict = p.body["verdict"].to_string();
            let rationale = p.body["rationale"].as_str().unwrap_or("");
            let verdict = verdict.trim_matches('"');
            let line = format!("- State: \"{excerpt}\" → `{verdict}` ({rationale})");
            if !push_line(&line, &mut block, &mut entry_ids, &p.id) {
                break;
            }
            used.precedents += 1;
        }
        block.push('\n');
    }

    if !block.is_empty() {
        block.push('\n');
    }
    used.entry_ids = entry_ids.clone();
    Ok(Retrieval {
        block,
        used,
        entry_ids,
    })
}
