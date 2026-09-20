//! Vote aggregation — pure functions, heavily tested.
//!
//! `w_j` is a juror weight (1.0 until `juror_stats` earns one), `n` the
//! number of valid ballots. Formulas per PLAN §5.4:
//!
//! - **choice**: `p_c = Σw_j[vote=c] / Σw_j`; winner = argmax (ties break by
//!   criteria declaration order and always hang); `confidence = p1 − p2`.
//! - **score**: `score = Σw_j·s_j / Σw_j`; `legend = criteria[round(score)]`;
//!   `confidence = min(1 − stddev / ((L−1)/2), bucket_support)` where
//!   `bucket_support` is the weight share voting exactly `round(score)` —
//!   a bimodal vote (0 and 2 on a 0–2 scale) reports legend `1` almost
//!   nobody picked, so support → 0 and the question hangs.
//! - **noul**: `noul = p_true`; `confidence = |2·p_true − 1|`.
//! - `n < 2` ⇒ `confidence = None`, never hung.

use std::collections::BTreeMap;

use crate::question::{Ballot, Question};
use crate::response::AnswerOut;

/// `(weight, ballot)` pairs for one question.
pub type Votes = [(f64, Ballot)];

/// Aggregate ballots for one question into an [`AnswerOut`].
/// Ballots that don't match the question type are skipped defensively.
pub fn tally(q: &Question, votes: &Votes) -> AnswerOut {
    let total_w: f64 = votes.iter().map(|(w, _)| w).sum();
    let n = votes.len();
    match q {
        Question::Choice { criteria, .. } => {
            let mut probs: BTreeMap<String, f64> =
                criteria.keys().map(|k| (k.clone(), 0.0)).collect();
            for (w, b) in votes {
                if let Ballot::Choice(c) = b
                    && let Some(p) = probs.get_mut(c)
                {
                    *p += w;
                }
            }
            if total_w > 0.0 {
                for p in probs.values_mut() {
                    *p /= total_w;
                }
            }
            // argmax; BTreeMap iteration is alphabetical, NOT declaration
            // order — so rank explicitly over `criteria` order.
            let mut ranked: Vec<(&String, f64)> = criteria
                .keys()
                .map(|k| (k, probs.get(k).copied().unwrap_or(0.0)))
                .collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let (top1, top2) = (
                ranked.first().map(|(_, p)| *p).unwrap_or(0.0),
                ranked.get(1).map(|(_, p)| *p).unwrap_or(0.0),
            );
            // Stable pick: declaration order among ties — but the caller
            // must know it was a tie (always hangs via conf=0 anyway).
            let winner = ranked
                .first()
                .map(|(k, _)| (*k).clone())
                .unwrap_or_default();
            let winner = criteria
                .keys()
                .find(|k| probs.get(*k).copied().unwrap_or(0.0) == top1)
                .cloned()
                .unwrap_or(winner);
            AnswerOut::Choice {
                choice: winner,
                probabilities: probs,
                confidence: (n >= 2).then_some((top1 - top2).max(0.0)),
                judge: None,
            }
        }
        Question::Score { criteria, .. } => {
            let valid: Vec<(f64, usize)> = votes
                .iter()
                .filter_map(|(w, b)| match b {
                    Ballot::Score(s) => Some((*w, *s)),
                    _ => None,
                })
                .collect();
            let vw: f64 = valid.iter().map(|(w, _)| w).sum();
            let score = if vw > 0.0 {
                valid.iter().map(|(w, s)| w * (*s as f64)).sum::<f64>() / vw
            } else {
                0.0
            };
            let legend_idx =
                (score.round().max(0.0) as usize).min(criteria.len().saturating_sub(1));
            let legend = criteria
                .get(legend_idx)
                .or_else(|| criteria.last())
                .cloned()
                .unwrap_or_default();
            let confidence = if n >= 2 && criteria.len() >= 2 && vw > 0.0 {
                let var = valid
                    .iter()
                    .map(|(w, s)| w * (*s as f64 - score).powi(2))
                    .sum::<f64>()
                    / vw;
                let stddev = var.sqrt();
                let max_stddev = (criteria.len() as f64 - 1.0) / 2.0;
                // Weight share that voted exactly the level we report —
                // a mean that lands between two modes reports a legend
                // nobody picked; bucket support pulls confidence to ~0.
                let spread_conf = (1.0 - stddev / max_stddev).clamp(0.0, 1.0);
                let bucket_support: f64 = valid
                    .iter()
                    .filter(|(_, s)| *s == legend_idx)
                    .map(|(w, _)| w)
                    .sum::<f64>()
                    / vw;
                Some(spread_conf.min(bucket_support))
            } else {
                None
            };
            AnswerOut::Score {
                score,
                legend,
                confidence,
                judge: None,
            }
        }
        Question::Noul { .. } => {
            let yes: f64 = votes
                .iter()
                .map(|(w, b)| match b {
                    Ballot::Noul(true) => *w,
                    _ => 0.0,
                })
                .sum();
            let p_true = if total_w > 0.0 { yes / total_w } else { 0.0 };
            AnswerOut::Noul {
                noul: p_true,
                confidence: (n >= 2).then_some((2.0 * p_true - 1.0).abs()),
                judge: None,
            }
        }
    }
}

/// Is this answer hung under `threshold`? `None` confidence (n<2) is not
/// hung — one juror can't disagree with itself.
pub fn is_hung(a: &AnswerOut, threshold: f64) -> bool {
    a.confidence().is_some_and(|c| c < threshold)
}

/// For the judge's ballot table: per question, a compact vote summary
/// (`billing×1, technical×2`) plus each juror's pick.
pub fn ballot_table<'a>(
    questions: impl Iterator<Item = (&'a String, &'a Question)>,
    juror_ballots: &[(String, BTreeMap<String, Ballot>)],
    answers: &BTreeMap<String, AnswerOut>,
    hung: &[String],
) -> String {
    let mut out = String::new();
    for (key, q) in questions {
        let votes: Vec<(&String, &Ballot)> = juror_ballots
            .iter()
            .filter_map(|(j, bs)| bs.get(key).map(|b| (j, b)))
            .collect();
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for (_, b) in &votes {
            *counts.entry(ballot_label(b)).or_default() += 1;
        }
        let tally_str = counts
            .iter()
            .map(|(v, c)| format!("{v}×{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let conf = answers
            .get(key)
            .and_then(|a| a.confidence())
            .map(|c| format!("{c:.2}"))
            .unwrap_or_else(|| "n/a".to_string());
        let mark = if hung.contains(key) { " (HUNG)" } else { "" };
        out.push_str(&format!(
            "Question `{key}` ({}): {tally_str} — confidence {conf}{mark}\n",
            q.kind_str()
        ));
        for (j, b) in &votes {
            out.push_str(&format!("- {j} → {}\n", ballot_label(b)));
        }
        out.push('\n');
    }
    out
}

/// Printable form of a ballot for the judge's table.
fn ballot_label(b: &Ballot) -> String {
    match b {
        Ballot::Choice(c) => c.clone(),
        Ballot::Score(s) => s.to_string(),
        Ballot::Noul(v) => v.to_string(),
        Ballot::Abstain => "abstain".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn choice_q() -> Question {
        serde_json::from_value(json!({
            "type":"choice","instructions":"i",
            "criteria":{"billing":"x","technical":"y","sales":"z"}
        }))
        .unwrap()
    }

    fn score_q(n: usize) -> Question {
        let crit: Vec<String> = (0..n).map(|i| format!("lv{i}")).collect();
        serde_json::from_value(json!({
            "type":"score","instructions":"i","criteria":crit
        }))
        .unwrap()
    }

    fn noul_q() -> Question {
        serde_json::from_value(json!({"type":"noul","instructions":"i"})).unwrap()
    }

    fn cb(c: &str) -> (f64, Ballot) {
        (1.0, Ballot::Choice(c.to_string()))
    }

    #[test]
    fn choice_unanimous() {
        let a = tally(
            &choice_q(),
            &[cb("technical"), cb("technical"), cb("technical")],
        );
        let AnswerOut::Choice {
            choice,
            probabilities,
            confidence,
            ..
        } = &a
        else {
            panic!()
        };
        assert_eq!(choice.as_str(), "technical");
        assert_eq!(probabilities["technical"], 1.0);
        assert_eq!(probabilities["billing"], 0.0);
        assert_eq!(*confidence, Some(1.0));
        assert!(!is_hung(&a, 0.5));
    }

    #[test]
    fn choice_2_1_hangs() {
        let a = tally(
            &choice_q(),
            &[cb("technical"), cb("technical"), cb("billing")],
        );
        let AnswerOut::Choice {
            choice, confidence, ..
        } = &a
        else {
            panic!()
        };
        assert_eq!(choice.as_str(), "technical");
        // 0.67 − 0.33 ≈ 0.33 < 0.5 → hung.
        assert!((confidence.unwrap() - 1.0 / 3.0).abs() < 1e-6);
        assert!(is_hung(&a, 0.5));
    }

    #[test]
    fn choice_4_1_passes() {
        let a = tally(
            &choice_q(),
            &[
                cb("technical"),
                cb("technical"),
                cb("technical"),
                cb("technical"),
                cb("billing"),
            ],
        );
        // 0.8 − 0.2 = 0.6 ≥ 0.5 → not hung.
        assert!(!is_hung(&a, 0.5));
        assert!((a.confidence().unwrap() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn choice_tie_breaks_declaration_order_and_hangs() {
        // "billing" declared before "technical": a 1–1–0 tie picks billing,
        // confidence 0 → always hung.
        let a = tally(&choice_q(), &[cb("technical"), cb("billing")]);
        let AnswerOut::Choice {
            choice, confidence, ..
        } = &a
        else {
            panic!()
        };
        assert_eq!(choice.as_str(), "billing");
        assert_eq!(*confidence, Some(0.0));
        assert!(is_hung(&a, 0.5));
    }

    #[test]
    fn choice_weights_shift_winner() {
        let a = tally(
            &choice_q(),
            &[
                (0.9, Ballot::Choice("billing".into())),
                (0.1, Ballot::Choice("technical".into())),
            ],
        );
        let AnswerOut::Choice {
            choice,
            probabilities,
            ..
        } = a
        else {
            panic!()
        };
        assert_eq!(choice, "billing");
        assert!((probabilities["billing"] - 0.9).abs() < 1e-9);
    }

    #[test]
    fn score_mean_legend_confidence() {
        let q = score_q(3);
        let votes = [
            (1.0, Ballot::Score(1)),
            (1.0, Ballot::Score(2)),
            (1.0, Ballot::Score(1)),
        ];
        let a = tally(&q, &votes);
        let AnswerOut::Score {
            score,
            legend,
            confidence,
            ..
        } = a
        else {
            panic!()
        };
        assert!((score - 4.0 / 3.0).abs() < 1e-9);
        assert_eq!(legend, "lv1");
        // stddev = sqrt(((1-1.33)²·2 + (2-1.33)²)/3) ≈ 0.471; max = 1 → conf ≈ 0.53.
        assert!((confidence.unwrap() - 0.528).abs() < 0.01);
    }

    #[test]
    fn score_unanimous_confidence_one() {
        let a = tally(
            &score_q(3),
            &[(1.0, Ballot::Score(2)), (1.0, Ballot::Score(2))],
        );
        assert_eq!(a.confidence(), Some(1.0));
    }

    #[test]
    fn score_bimodal_vote_hangs() {
        // 0 and 2 on a 0–2 scale: mean 1.33 reports legend "1" that
        // nobody voted — bucket support 0 ⇒ hung.
        let q = score_q(3);
        let votes = [
            (1.0, Ballot::Score(0)),
            (1.0, Ballot::Score(2)),
            (1.0, Ballot::Score(2)),
        ];
        let a = tally(&q, &votes);
        assert_eq!(a.confidence(), Some(0.0));
        assert!(is_hung(&a, 0.5));

        // Majority still carries the legend: {1,1,2} reports "1" with
        // 67% support — decided, not hung.
        let votes = [
            (1.0, Ballot::Score(1)),
            (1.0, Ballot::Score(1)),
            (1.0, Ballot::Score(2)),
        ];
        let a = tally(&q, &votes);
        assert!(!is_hung(&a, 0.5));
        assert!((a.confidence().unwrap() - 0.528).abs() < 0.01);
    }

    #[test]
    fn noul_prob_and_confidence() {
        let votes = [
            (1.0, Ballot::Noul(true)),
            (1.0, Ballot::Noul(true)),
            (1.0, Ballot::Noul(false)),
        ];
        let a = tally(&noul_q(), &votes);
        let AnswerOut::Noul {
            noul, confidence, ..
        } = a
        else {
            panic!()
        };
        assert!((noul - 2.0 / 3.0).abs() < 1e-9);
        assert!((confidence.unwrap() - 1.0 / 3.0).abs() < 1e-6);
        assert!(is_hung(&a, 0.5));
    }

    #[test]
    fn single_ballot_no_confidence_never_hung() {
        let a = tally(&noul_q(), &[(1.0, Ballot::Noul(true))]);
        assert_eq!(a.confidence(), None);
        assert!(!is_hung(&a, 0.5));
        let a = tally(&choice_q(), &[cb("billing")]);
        assert_eq!(a.confidence(), None);
        assert!(!is_hung(&a, 0.5));
    }

    #[test]
    fn empty_votes_zeroed() {
        let a = tally(&choice_q(), &[]);
        assert_eq!(a.confidence(), None);
        assert!(!is_hung(&a, 0.5));
        let AnswerOut::Choice { choice, .. } = a else {
            panic!()
        };
        assert_eq!(choice, "billing"); // declaration-order fallback
    }
}
