//! Typed questions: `Choice` / `Score` / `Noul`, their qids, the juror
//! answer schema, and ballot validation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// One question, keyed in `Request.questions`.
///
/// ```json
/// {"type":"choice","id":"support.department","instructions":"…",
///  "criteria":{"billing":"…","technical":"…"}}
/// {"type":"score","instructions":"…","criteria":["calm","civil","angry"]}
/// {"type":"noul","instructions":"…"}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Pick exactly one of the criteria keys.
    Choice {
        /// Optional stable id for memory scoping.
        #[serde(default)]
        id: Option<String>,
        /// What to judge.
        instructions: String,
        /// `key → description`, declaration order matters (tie-break).
        criteria: BTreeMap<String, String>,
    },
    /// Rate on an ordinal scale; criteria describe each level.
    Score {
        /// Optional stable id for memory scoping.
        #[serde(default)]
        id: Option<String>,
        /// What to rate.
        instructions: String,
        /// Description of each level `0..n`.
        criteria: Vec<String>,
    },
    /// True/false proposition ("noul" = null oui/non — a boolean vote).
    Noul {
        /// Optional stable id for memory scoping.
        #[serde(default)]
        id: Option<String>,
        /// The proposition to test.
        instructions: String,
    },
}

/// A juror's answer to one question, already type-checked.
#[derive(Debug, Clone, PartialEq)]
pub enum Ballot {
    /// Chosen criteria key.
    Choice(String),
    /// Level index `0..n`.
    Score(usize),
    /// Boolean vote.
    Noul(bool),
    /// The juror declined to answer — the state lacks the information.
    /// Counts as *no ballot* for that question, so a unanimous abstain
    /// lands under `min_quorum` and hangs.
    Abstain,
}

impl Ballot {
    /// The raw JSON value as the juror emitted it (for `usage.jurors[].answers`).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Ballot::Choice(c) => serde_json::Value::String(c.clone()),
            Ballot::Score(s) => serde_json::json!(s),
            Ballot::Noul(b) => serde_json::Value::Bool(*b),
            Ballot::Abstain => serde_json::Value::String("abstain".into()),
        }
    }
}

/// Reserved answer string — `{"<key>": "abstain"}` from a juror is a
/// decline, not a choice. (Don't name a criterion `abstain` — it would
/// be unreachable.)
pub const ABSTAIN: &str = "abstain";

/// Validate a question-map key: `[A-Za-z_][A-Za-z0-9_]*`.
pub fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Collapse whitespace runs + trim + lowercase — the normalization applied
/// before hashing for a qid.
fn normalized(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl Question {
    /// `id` when present, else 16 hex of `sha256(type ‖ instructions ‖
    /// criteria)` over normalized text — stable across rewording only via
    /// an explicit `id`.
    pub fn qid(&self) -> String {
        if let Some(id) = self.id() {
            return id.to_string();
        }
        let mut h = Sha256::new();
        h.update(self.kind_str().as_bytes());
        h.update(b"\x00");
        h.update(normalized(self.instructions()).as_bytes());
        h.update(b"\x00");
        match self {
            Question::Choice { criteria, .. } => {
                for (k, v) in criteria {
                    h.update(normalized(k).as_bytes());
                    h.update(b"\x01");
                    h.update(normalized(v).as_bytes());
                    h.update(b"\x02");
                }
            }
            Question::Score { criteria, .. } => {
                for c in criteria {
                    h.update(normalized(c).as_bytes());
                    h.update(b"\x01");
                }
            }
            Question::Noul { .. } => {}
        }
        hex::encode(h.finalize())[..16].to_string()
    }

    /// Explicit `id` if set.
    pub fn id(&self) -> Option<&str> {
        match self {
            Question::Choice { id, .. } | Question::Score { id, .. } | Question::Noul { id, .. } => {
                id.as_deref()
            }
        }
    }

    /// The instruction text.
    pub fn instructions(&self) -> &str {
        match self {
            Question::Choice { instructions, .. }
            | Question::Score { instructions, .. }
            | Question::Noul { instructions, .. } => instructions,
        }
    }

    /// `"choice" | "score" | "noul"`.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Question::Choice { .. } => "choice",
            Question::Score { .. } => "score",
            Question::Noul { .. } => "noul",
        }
    }

    /// Short type description for prompts (`{type}` info embedded).
    pub fn describe(&self, key: &str) -> String {
        let mut s = format!("### `{key}` ({})\n{}", self.kind_str(), self.instructions());
        match self {
            Question::Choice { criteria, .. } => {
                s.push_str("\nChoices:");
                for (k, v) in criteria {
                    s.push_str(&format!("\n- `{k}`: {v}"));
                }
            }
            Question::Score { criteria, .. } => {
                s.push_str("\nScale (answer with the level number):");
                for (i, c) in criteria.iter().enumerate() {
                    s.push_str(&format!("\n- {i}: {c}"));
                }
            }
            Question::Noul { .. } => {
                s.push_str("\nAnswer `true` if the proposition holds, else `false`.");
            }
        }
        s
    }

    /// The JSON Schema fragment for this question's answer value, in the
    /// strict subset codex accepts (enum/integer/boolean only). Every
    /// question also accepts the string `"abstain"`.
    pub fn answer_schema(&self) -> serde_json::Value {
        match self {
            Question::Choice { criteria, .. } => serde_json::json!({
                "type": "string",
                "enum": criteria.keys().map(String::as_str).chain([ABSTAIN])
                    .collect::<Vec<_>>(),
            }),
            Question::Score { criteria, .. } => serde_json::json!({
                "enum": (0..criteria.len())
                    .map(serde_json::Value::from)
                    .chain([serde_json::Value::String(ABSTAIN.into())])
                    .collect::<Vec<_>>(),
            }),
            Question::Noul { .. } => serde_json::json!({
                "enum": [true, false, ABSTAIN],
            }),
        }
    }

    /// Type-check one raw answer value into a [`Ballot`].
    pub fn validate_answer(&self, key: &str, v: &serde_json::Value) -> Result<Ballot> {
        // `"abstain"` is accepted for every question type — the juror is
        // saying the state doesn't contain enough to decide.
        if v.as_str() == Some(ABSTAIN) {
            return Ok(Ballot::Abstain);
        }
        match self {
            Question::Choice { criteria, .. } => {
                let s = v.as_str().ok_or_else(|| Error::Validation {
                    agent: key.to_string(),
                    message: format!("expected one of {:?}, got {v}", criteria.keys()),
                })?;
                if !criteria.contains_key(s) {
                    return Err(Error::Validation {
                        agent: key.to_string(),
                        message: format!("'{s}' not in choices {:?}", criteria.keys()),
                    });
                }
                Ok(Ballot::Choice(s.to_string()))
            }
            Question::Score { criteria, .. } => {
                let n = v.as_u64().ok_or_else(|| Error::Validation {
                    agent: key.to_string(),
                    message: format!("expected integer in 0..{}, got {v}", criteria.len()),
                })? as usize;
                if n >= criteria.len() {
                    return Err(Error::Validation {
                        agent: key.to_string(),
                        message: format!("{n} out of range 0..{}", criteria.len()),
                    });
                }
                Ok(Ballot::Score(n))
            }
            Question::Noul { .. } => v.as_bool().map(Ballot::Noul).ok_or_else(|| {
                Error::Validation {
                    agent: key.to_string(),
                    message: format!("expected boolean, got {v}"),
                }
            }),
        }
    }
}

/// The whole-ballot JSON Schema for one juror: every question key → its
/// answer schema. With `explain`, a `_why` object of per-key strings is
/// also required. Strict-mode friendly: `additionalProperties: false`,
/// every property required.
pub fn ballot_schema(
    questions: &BTreeMap<String, Question>,
    explain: bool,
) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (key, q) in questions {
        props.insert(key.clone(), q.answer_schema());
        required.push(serde_json::Value::String(key.clone()));
    }
    if explain {
        let why_props: serde_json::Map<String, serde_json::Value> = questions
            .keys()
            .map(|k| (k.clone(), serde_json::json!({"type": "string"})))
            .collect();
        props.insert(
            "_why".to_string(),
            serde_json::json!({
                "type": "object",
                "properties": why_props,
                "required": required.clone(),
                "additionalProperties": false,
            }),
        );
        required.push(serde_json::Value::String("_why".to_string()));
    }
    serde_json::json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

/// A validated ballot: `{key: Ballot}` + the optional `_why` map.
pub type ValidatedBallot = (BTreeMap<String, Ballot>, Option<BTreeMap<String, String>>);

/// Validate a whole ballot object → `{key: Ballot}` + optional `_why` map.
/// Extra/missing keys are errors — the caller retries with feedback.
pub fn validate_ballot(
    questions: &BTreeMap<String, Question>,
    v: &serde_json::Value,
    explain: bool,
) -> Result<ValidatedBallot> {
    let obj = v.as_object().ok_or_else(|| Error::Validation {
        agent: "juror".to_string(),
        message: format!("expected a JSON object of answers, got {}", crate::backend::tail(&v.to_string(), 200)),
    })?;
    let mut ballots = BTreeMap::new();
    let mut missing = Vec::new();
    for (key, q) in questions {
        match obj.get(key) {
            Some(val) => {
                ballots.insert(key.clone(), q.validate_answer(key, val)?);
            }
            None => missing.push(key.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(Error::Validation {
            agent: "juror".to_string(),
            message: format!("missing answers for: {}", missing.join(", ")),
        });
    }
    let why = if explain {
        let w = obj.get("_why").and_then(|x| x.as_object()).ok_or_else(|| {
            Error::Validation {
                agent: "juror".to_string(),
                message: "missing `_why` object".to_string(),
            }
        })?;
        Some(
            w.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect(),
        )
    } else {
        None
    };
    Ok((ballots, why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn choice() -> Question {
        serde_json::from_value(json!({
            "type": "choice", "id": "support.department",
            "instructions": "Which team should handle this",
            "criteria": {"billing": "Money", "technical": "Bugs", "sales": "Pricing"}
        }))
        .unwrap()
    }

    fn score() -> Question {
        serde_json::from_value(json!({
            "type": "score", "instructions": "Frustration",
            "criteria": ["calm", "civil", "angry"]
        }))
        .unwrap()
    }

    #[test]
    fn qid_uses_explicit_id() {
        assert_eq!(choice().qid(), "support.department");
    }

    #[test]
    fn qid_stable_under_whitespace_case() {
        let a: Question = serde_json::from_value(json!({
            "type": "noul", "instructions": "The message  conveys URGENCY"
        }))
        .unwrap();
        let b: Question = serde_json::from_value(json!({
            "type": "noul", "instructions": "the message conveys urgency"
        }))
        .unwrap();
        assert_eq!(a.qid(), b.qid());
        assert_eq!(a.qid().len(), 16);
    }

    #[test]
    fn qid_differs_by_type() {
        let n: Question = serde_json::from_value(json!({
            "type": "noul", "instructions": "x"
        }))
        .unwrap();
        let s: Question = serde_json::from_value(json!({
            "type": "score", "instructions": "x", "criteria": ["a", "b"]
        }))
        .unwrap();
        assert_ne!(n.qid(), s.qid());
    }

    #[test]
    fn valid_keys() {
        assert!(valid_key("department"));
        assert!(valid_key("_x9"));
        assert!(!valid_key("9x"));
        assert!(!valid_key("a-b"));
        assert!(!valid_key(""));
    }

    #[test]
    fn answer_schema_variants() {
        assert_eq!(
            choice().answer_schema()["enum"],
            json!(["billing", "sales", "technical", "abstain"])
        );
        assert_eq!(score().answer_schema()["enum"], json!([0, 1, 2, "abstain"]));
        assert_eq!(
            serde_json::from_value::<Question>(json!({"type":"noul","instructions":"i"}))
                .unwrap()
                .answer_schema()["enum"],
            json!([true, false, "abstain"])
        );
    }

    #[test]
    fn abstain_validates_for_every_type() {
        assert_eq!(
            choice().validate_answer("d", &json!("abstain")).unwrap(),
            Ballot::Abstain
        );
        assert_eq!(
            score().validate_answer("f", &json!("abstain")).unwrap(),
            Ballot::Abstain
        );
        let noul: Question =
            serde_json::from_value(json!({"type":"noul","instructions":"i"})).unwrap();
        assert_eq!(
            noul.validate_answer("u", &json!("abstain")).unwrap(),
            Ballot::Abstain
        );
    }

    #[test]
    fn validate_choice_score_noul() {
        let mut qs = BTreeMap::new();
        qs.insert("d".to_string(), choice());
        qs.insert("f".to_string(), score());
        qs.insert(
            "u".to_string(),
            serde_json::from_value::<Question>(json!({"type":"noul","instructions":"i"})).unwrap(),
        );
        let (b, why) = validate_ballot(
            &qs,
            &json!({"d": "technical", "f": 1, "u": true}),
            false,
        )
        .unwrap();
        assert_eq!(b["d"], Ballot::Choice("technical".to_string()));
        assert_eq!(b["f"], Ballot::Score(1));
        assert_eq!(b["u"], Ballot::Noul(true));
        assert!(why.is_none());

        assert!(validate_ballot(&qs, &json!({"d": "nope", "f": 1, "u": true}), false).is_err());
        assert!(validate_ballot(&qs, &json!({"d": "technical", "f": 9, "u": true}), false).is_err());
        assert!(validate_ballot(&qs, &json!({"d": "technical", "f": 1}), false).is_err());
        assert!(validate_ballot(&qs, &json!([1, 2]), false).is_err());
    }

    #[test]
    fn explain_requires_why() {
        let mut qs = BTreeMap::new();
        qs.insert("u".to_string(), serde_json::from_value::<Question>(
            json!({"type":"noul","instructions":"i"}),
        ).unwrap());
        let schema = ballot_schema(&qs, true);
        assert!(schema["properties"]["_why"].is_object());
        assert!(validate_ballot(&qs, &json!({"u": true}), true).is_err());
        let (_, why) = validate_ballot(&qs, &json!({"u": true, "_why": {"u": "sounds urgent"}}), true)
            .unwrap();
        assert_eq!(why.unwrap()["u"], "sounds urgent");
    }
}
