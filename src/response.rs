//! The `decide` response JSON — shape kept close to `typesafe_sdk`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Who produced the final answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecidedBy {
    /// Jury reached confident agreement.
    Jury,
    /// The judge resolved hung questions.
    Judge,
    /// Served from the decision cache.
    Cache,
}

/// The judge's verdict on one question (nested inside an answer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// Judge's pick (choice questions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// Judge's level (score questions; integer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    /// Judge's boolean (noul questions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noul: Option<bool>,
    /// ≤2 sentences from the judge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// One question's aggregated answer. Serializes with a `type` tag matching
/// the question kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnswerOut {
    /// `choice` question result.
    Choice {
        /// Winning option (judge's, when the judge ruled).
        choice: String,
        /// Vote share per option — always the jury's, honestly reporting
        /// disagreement even when the judge overruled.
        probabilities: BTreeMap<String, f64>,
        /// `p_top1 − p_top2`; `null` with <2 valid ballots.
        confidence: Option<f64>,
        /// Judge verdict, when the question was escalated.
        #[serde(skip_serializing_if = "Option::is_none")]
        judge: Option<JudgeVerdict>,
    },
    /// `score` question result.
    Score {
        /// Weighted mean level.
        score: f64,
        /// `criteria[round(score)]` — the level's description.
        legend: String,
        /// `1 − normalized stddev`; `null` with <2 valid ballots.
        confidence: Option<f64>,
        /// Judge verdict, when the question was escalated.
        #[serde(skip_serializing_if = "Option::is_none")]
        judge: Option<JudgeVerdict>,
    },
    /// `noul` question result: `noul` is the share of `true` votes, [0,1].
    Noul {
        /// Fraction of `true` ballots (judge's boolean when escalated).
        noul: f64,
        /// `|2·p_true − 1|`; `null` with <2 valid ballots.
        confidence: Option<f64>,
        /// Judge verdict, when the question was escalated.
        #[serde(skip_serializing_if = "Option::is_none")]
        judge: Option<JudgeVerdict>,
    },
}

impl AnswerOut {
    /// The confidence field, whichever variant this is.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            AnswerOut::Choice { confidence, .. }
            | AnswerOut::Score { confidence, .. }
            | AnswerOut::Noul { confidence, .. } => *confidence,
        }
    }

    /// Attach/replace the judge verdict.
    pub fn set_judge(&mut self, jv: JudgeVerdict) {
        match self {
            AnswerOut::Choice { judge, .. }
            | AnswerOut::Score { judge, .. }
            | AnswerOut::Noul { judge, .. } => *judge = Some(jv),
        }
    }
}

/// Which memory entries were injected into juror prompts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryUse {
    /// Rulings injected.
    pub rulings: usize,
    /// Precedents injected.
    pub precedents: usize,
    /// Facts injected.
    pub facts: usize,
    /// Entry ids used (auditability for injected guidance).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_ids: Vec<String>,
}

/// One juror ballot call, for `usage.jurors`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JurorUsage {
    /// `"<backend>:<model>"` string.
    pub juror: String,
    /// Sample index (`0..samples`).
    pub sample: u32,
    /// `ok` | `timeout` | `error` | `invalid`.
    pub status: String,
    /// Wall ms including retries.
    pub ms: u64,
    /// Extra attempts beyond the first.
    pub retries: u32,
    /// Validated answers (present on `ok`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<BTreeMap<String, serde_json::Value>>,
    /// Failure detail (present on non-ok).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Input tokens, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

/// The judge call, for `usage.judge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeUsage {
    /// Judge model string.
    pub model: String,
    /// `ok` | `timeout` | `error` | `invalid`.
    pub status: String,
    /// Wall ms.
    pub ms: u64,
    /// Memory entry ids written by this ruling.
    #[serde(default)]
    pub wrote: Vec<String>,
    /// Failure detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Call accounting for `usage`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Whole-decision wall clock.
    pub wall_ms: u64,
    /// Every juror attempt (one per model × sample).
    pub jurors: Vec<JurorUsage>,
    /// Judge call, when escalation ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeUsage>,
    /// Estimated USD for this decision (needs `[costs]` in config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est_cost_usd: Option<f64>,
}

/// The top-level response printed to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// `dec_…` id (referenced by `feedback`/`learn`).
    pub id: String,
    /// Who produced the final answers.
    pub decided_by: DecidedBy,
    /// Question key → aggregated answer.
    pub answers: BTreeMap<String, AnswerOut>,
    /// Hung questions left unresolved.
    pub hung: Vec<String>,
    /// Hung questions that were escalated to the judge (empty when no
    /// escalation ran). A subset may still be in `hung` if the judge
    /// abstained.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalated: Vec<String>,
    /// Per-key attribution: which decider produced each *decided*
    /// answer (`jury` | `judge` | `cache`). Hung keys are absent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, DecidedBy>,
    /// Memory usage summary.
    pub memory: MemoryUse,
    /// Call accounting.
    pub usage: Usage,
}

impl Response {
    /// Exit code: `0` decided, `2` questions still hung.
    pub fn exit_code(&self) -> i32 {
        if self.hung.is_empty() { 0 } else { 2 }
    }
}
