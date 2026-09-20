//! `hungjury lint` — static checks on a questions file.
//!
//! Evaluations showed vague rubrics are the single biggest accuracy
//! lever (score levels without marker words cost ~13 points). These
//! checks are deterministic and free — they can't prove a rubric is
//! good, but they reliably catch the three failure shapes we measured:
//!
//! - abstract score levels with no example markers,
//! - one-sided boundaries ("X counts" without "Y does not count"),
//! - overlapping choice criteria a model can't separate.

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::{Error, Result};

/// One lint finding, tied to a question key.
#[derive(Debug)]
struct Warn {
    key: String,
    msg: String,
}

/// Words that signal an explicit exclusion clause.
const EXCLUSIONS: &[&str] = &[
    "not", "never", "no ", "without", "doesn't", "don't", "except", "unless", "only", "alone",
];

/// Marker shapes that signal a concrete example was given.
fn has_example(text: &str) -> bool {
    text.contains('"') || text.contains('\'') || text.contains('(') || text.contains("e.g.")
}

fn has_exclusion(text: &str) -> bool {
    let t = text.to_lowercase();
    EXCLUSIONS.iter().any(|w| t.contains(w))
}

fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// Lint a questions file (`{"key": {"type": ..., "instructions": ...,
/// "criteria": ...}}`). Prints warnings; returns their count.
pub fn run(path: &Path) -> Result<u8> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let warns = lint_text(&text).map_err(|e| Error::Request(format!("{path:?}: {e}")))?;

    if warns.is_empty() {
        eprintln!("lint: no warnings");
        return Ok(0);
    }
    for w in &warns {
        eprintln!("warn [{}] {}", w.key, w.msg);
    }
    eprintln!("lint: {} warning(s)", warns.len());
    Ok(0)
}

/// Check parsed questions JSON text; one warning per finding.
fn lint_text(text: &str) -> std::result::Result<Vec<Warn>, String> {
    let qs: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|e| format!("invalid questions JSON: {e}"))?;

    let mut warns: Vec<Warn> = Vec::new();
    if qs.is_empty() {
        warns.push(Warn {
            key: "*".into(),
            msg: "no questions defined".into(),
        });
    }
    for (key, q) in &qs {
        let ty = q["type"].as_str().unwrap_or("");
        let instr = q["instructions"].as_str().unwrap_or("");
        if instr.len() < 10 {
            warns.push(Warn {
                key: key.clone(),
                msg: "instructions are missing or too short — state the decision rule, not just the topic".into(),
            });
        }
        match ty {
            "score" => {
                let levels: Vec<&str> = q["criteria"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if levels.is_empty() {
                    warns.push(Warn {
                        key: key.clone(),
                        msg: "score question has no level descriptions".into(),
                    });
                    continue;
                }
                for (i, lv) in levels.iter().enumerate() {
                    if lv.len() < 25 {
                        warns.push(Warn {
                            key: key.clone(),
                            msg: format!("level {i} is abstract ({lv:?}) — add marker words or example phrasings"),
                        });
                    } else if !has_example(lv) {
                        warns.push(Warn {
                            key: key.clone(),
                            msg: format!("level {i} has no concrete example markers — quote the words that mean this level"),
                        });
                    }
                }
                // Boundary must be stated both ways: the top level needs an
                // explicit "X does NOT count" clause or models over-trigger.
                if let Some(top) = levels.last()
                    && !has_exclusion(top)
                {
                    warns.push(Warn {
                        key: key.clone(),
                        msg: format!("top level {} states what counts but not what does NOT — add an exclusion ('a deadline alone is not …')", levels.len() - 1),
                    });
                }
            }
            "choice" => {
                let crit: Vec<(&str, &str)> = q["criteria"]
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|d| (k.as_str(), d)))
                            .collect()
                    })
                    .unwrap_or_default();
                for (name, desc) in &crit {
                    if desc.len() < 15 {
                        warns.push(Warn {
                            key: key.clone(),
                            msg: format!("option {name:?} description is thin — what distinguishes it from the others?"),
                        });
                    }
                }
                for i in 0..crit.len() {
                    for j in i + 1..crit.len() {
                        let sim = jaccard(&words(crit[i].1), &words(crit[j].1));
                        if sim > 0.5 {
                            warns.push(Warn {
                                key: key.clone(),
                                msg: format!("options {:?} and {:?} descriptions overlap ({sim:.0}%) — add distinguishing markers", crit[i].0, crit[j].0),
                            });
                        }
                    }
                }
            }
            "noul" => {
                if !has_exclusion(instr) {
                    warns.push(Warn {
                        key: key.clone(),
                        msg: "noul instruction has no exclusion clause — state what does NOT count (e.g. 'courtesy phrases do not count')".into(),
                    });
                }
            }
            other => warns.push(Warn {
                key: key.clone(),
                msg: format!("unknown question type {other:?} (expected choice|score|noul)"),
            }),
        }
    }

    Ok(warns)
}

#[cfg(test)]
mod tests {
    use super::lint_text;

    #[test]
    fn vague_rubric_warns_on_every_axis() {
        let q = r#"{
          "frustration": {"type":"score","instructions":"How frustrated",
            "criteria":["Calm, just stating facts","Frustrated but civil","Very angry, strong language"]},
          "urgent": {"type":"noul","instructions":"The message conveys urgency"}
        }"#;
        let w = lint_text(q).unwrap();
        // abstract levels + missing top-level exclusion + noul no-exclusion
        assert!(w.len() >= 4, "{w:?}");
        assert!(
            w.iter()
                .any(|x| x.msg.contains("abstract") || x.msg.contains("marker"))
        );
        assert!(w.iter().any(|x| x.msg.contains("exclusion")));
    }

    #[test]
    fn anchored_rubric_is_clean() {
        let q = r#"{
          "frustration": {"type":"score","instructions":"How frustrated the customer appears",
            "criteria":[
              "Calm or neutral — 'fyi', 'just checking in'; purely factual, no annoyance markers at all",
              "Mild annoyance or civil complaint ('a bit annoying', 'kind of a pain') — no strong wording",
              "Strong wording ('ridiculous', 'unacceptable') OR names a penalty landing if unresolved. A deadline alone does NOT count"
            ]},
          "route": {"type":"choice","instructions":"Which team owns this ticket",
            "criteria":{"billing":"Payment, charge or refund issues","technical":"Bugs, crashes or errors"}},
          "urgent": {"type":"noul","instructions":"Real urgency: explicit deadline or consequence if unresolved. Courtesy phrases do NOT count"}
        }"#;
        assert!(lint_text(q).unwrap().is_empty());
    }
}
