//! Acceptance criteria, authored by someone other than the executor
//! (Invariant 50, RR-0028i).
//!
//! With no human reviewer in the loop, self-graded homework is the failure
//! mode: an executor who writes its own acceptance criteria can always
//! satisfy them. The enforcement is STRUCTURAL, not procedural —
//! [`validate_authorship`] rejects `CriteriaAuthor::Worker(id)` equal to the
//! executor before work starts, rather than asking anyone to remember a
//! rule (the board-gate lesson of ethos rule 3: constraints that cannot be
//! satisfied honestly teach the model to lie; this one always has an honest
//! path — get a different worker, or the document, to author).
//!
//! The reviewer is capped too: Invariant 47 limits the executor's retries,
//! and [`ReviewRounds`] limits the reviewer to 3 rejections, else the
//! reviewer becomes the unbounded loop. Exhaustion is recorded as a
//! [`PolicyDecision`] with `review_rounds_exhausted: true` — the escape is
//! AUDITED, not silent (ethos rule 6).

use crate::circuit::PolicyDecision;
use crate::ids::{CriterionId, WorkerId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Who authored the criteria. `Document` exists for the bootstrap rule:
/// RR checklist items' criteria are pre-authored by the plan document
/// itself (the `Requirement` and `Tests` fields), which satisfies the
/// separation requirement by construction — a document is never the
/// executor. The `CriteriaReviewer` role applies only to runtime-created
/// tasks (decompositions, discovered items, auto-captured prompts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CriteriaAuthor {
    /// A (different) worker wrote these.
    Worker { id: WorkerId },
    /// Pre-authored by the plan document (bootstrap rule, Invariant 50).
    Document,
}

/// One falsifiable acceptance criterion. A criterion without a verifier is
/// not falsifiable ("works correctly"), which is exactly what the
/// adversarial `CriteriaReviewer` rejects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    pub id: CriterionId,
    pub description: String,
    /// The typed verifier that proves this criterion (RR-0015). Typed, not
    /// a name string: a criterion whose verifier cannot execute is a
    /// criterion that cannot fail, and that is theatre (ethos rule 7).
    pub verifier: crate::verification::VerifierKind,
    /// Optional criteria may fail without blocking verification; required
    /// ones cannot.
    pub required: bool,
}

/// The criteria set for one task. A task cannot leave `todo` without at
/// least one criterion (Invariant 50 rule 1 — enforced at the board, which
/// owns task transitions; this type carries the data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriteria {
    pub criteria: Vec<Criterion>,
    pub authored_by: CriteriaAuthor,
    /// Bumped on amendment. Post-start edits are a distinct audited
    /// transition that resets verification to `needs_reverification`
    /// (Invariant 50 rule 3) — the executor must SEE the goalposts move.
    pub version: u32,
}

impl AcceptanceCriteria {
    /// Replace the criteria, bumping `version`. Amendment is an audited
    /// event at the board layer; core owns the version discipline so a
    /// changed criteria set is never mistaken for the one that was reviewed.
    pub fn amend(&mut self, criteria: Vec<Criterion>) {
        self.criteria = criteria;
        self.version += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorshipError {
    /// The executor authored its own acceptance criteria (Invariant 50
    /// rule 2). Structurally rejected — self-graded homework.
    #[error("worker {worker} cannot execute against criteria it authored itself (Invariant 50)")]
    SelfAuthored { worker: WorkerId },
}

/// The structural separation check (Invariant 50 rule 2): the executor
/// cannot be the author. `Document` always satisfies separation — a
/// document cannot execute anything.
pub fn validate_authorship(
    criteria: &AcceptanceCriteria,
    executor: &WorkerId,
) -> Result<(), AuthorshipError> {
    match &criteria.authored_by {
        CriteriaAuthor::Worker { id } if id == executor => Err(AuthorshipError::SelfAuthored {
            worker: executor.clone(),
        }),
        CriteriaAuthor::Worker { .. } | CriteriaAuthor::Document => Ok(()),
    }
}

/// Outcome of recording a reviewer rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundOutcome {
    /// The reviewer may reject again.
    Continue,
    /// The cap is reached: the criteria are ACCEPTED with the reviewer's
    /// objections recorded (see [`ReviewRounds::exhaustion_decision`]) —
    /// the reviewer does not get an unbounded veto.
    Exhausted,
}

/// Reviewer round counter (Invariant 50 rule 5). Invariant 47 bounds the
/// executor; this bounds the reviewer — without it, reject/revise is an
/// infinite loop with two well-behaved participants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRounds {
    pub rejections: u32,
}

impl ReviewRounds {
    /// Max rejections per task (Invariant 50 rule 5).
    pub const MAX_REJECTIONS: u32 = 3;

    /// Record one rejection. The THIRD rejection returns [`RoundOutcome::
    /// Exhausted`]: the criteria are accepted over the reviewer's objection,
    /// and the caller must write the [`PolicyDecision`] so the override is
    /// audited, not silent.
    pub fn record_rejection(&mut self) -> RoundOutcome {
        self.rejections = self.rejections.saturating_add(1);
        if self.rejections >= Self::MAX_REJECTIONS {
            RoundOutcome::Exhausted
        } else {
            RoundOutcome::Continue
        }
    }

    /// The PolicyDecision-shaped signal for exhaustion (RR-0028h's
    /// `review_rounds_exhausted` flag exists for exactly this producer).
    /// `rationale` must carry the reviewer's standing objections — the
    /// override is only auditable if what was overridden is on the record
    /// (ethos rule 6). `reversible: true` because the criteria remain
    /// amendable afterward (amendment bumps the version and resets
    /// verification); the decision closes the review LOOP, not the criteria.
    pub fn exhaustion_decision(
        &self,
        objections: impl Into<String>,
        at: DateTime<Utc>,
    ) -> PolicyDecision {
        PolicyDecision {
            decision: "criteria_review_rounds_exhausted".into(),
            chosen: "accept_criteria_over_reviewer_objection".into(),
            rationale: objections.into(),
            reversible: true,
            at,
            review_rounds_exhausted: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(ulid: &str) -> WorkerId {
        WorkerId::from_ulid(ulid.parse().unwrap())
    }

    fn criterion(ulid: &str) -> Criterion {
        Criterion {
            id: CriterionId::from_ulid(ulid.parse().unwrap()),
            description: "GET /api/search returns PagedResponse with truncated computed".into(),
            verifier: crate::verification::VerifierKind::Command {
                cmd: "true".into(),
                expected_exit: 0,
            },
            required: true,
        }
    }

    fn criteria_by(author: CriteriaAuthor) -> AcceptanceCriteria {
        AcceptanceCriteria {
            criteria: vec![criterion("01JGXV0000000000000000TEST")],
            authored_by: author,
            version: 1,
        }
    }

    #[test]
    fn executor_cannot_author_its_own_criteria() {
        let w = worker("01JGXV0000000000000000AAAA");
        let c = criteria_by(CriteriaAuthor::Worker { id: w.clone() });
        let err = validate_authorship(&c, &w).unwrap_err();
        assert!(matches!(err, AuthorshipError::SelfAuthored { .. }));
    }

    #[test]
    fn different_worker_author_satisfies_separation() {
        let author = worker("01JGXV0000000000000000AAAA");
        let executor = worker("01JGXV0000000000000000BBBB");
        let c = criteria_by(CriteriaAuthor::Worker { id: author });
        assert!(validate_authorship(&c, &executor).is_ok());
    }

    #[test]
    fn document_author_satisfies_separation() {
        // Bootstrap rule: the plan document pre-authors RR items' criteria.
        let executor = worker("01JGXV0000000000000000AAAA");
        let c = criteria_by(CriteriaAuthor::Document);
        assert!(validate_authorship(&c, &executor).is_ok());
    }

    #[test]
    fn reviewer_round_cap_at_exactly_three() {
        let mut rounds = ReviewRounds::default();
        assert_eq!(rounds.record_rejection(), RoundOutcome::Continue);
        assert_eq!(rounds.record_rejection(), RoundOutcome::Continue);
        // Third rejection exhausts the reviewer, not the second or fourth.
        assert_eq!(rounds.record_rejection(), RoundOutcome::Exhausted);
        assert_eq!(rounds.rejections, 3);
        // Further rejections stay exhausted — the cap does not reset itself.
        assert_eq!(rounds.record_rejection(), RoundOutcome::Exhausted);
    }

    #[test]
    fn exhaustion_produces_flagged_policy_decision() {
        let mut rounds = ReviewRounds::default();
        for _ in 0..3 {
            let _ = rounds.record_rejection();
        }
        let at: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
        let d = rounds.exhaustion_decision("criterion 2 still not falsifiable", at);
        assert!(d.review_rounds_exhausted);
        assert_eq!(d.decision, "criteria_review_rounds_exhausted");
        assert!(d.rationale.contains("not falsifiable"));
    }

    #[test]
    fn amendment_bumps_version() {
        let mut c = criteria_by(CriteriaAuthor::Document);
        assert_eq!(c.version, 1);
        c.amend(vec![
            criterion("01JGXV0000000000000000AAAA"),
            criterion("01JGXV0000000000000000BBBB"),
        ]);
        assert_eq!(c.version, 2);
        assert_eq!(c.criteria.len(), 2);
    }

    #[test]
    fn serde_round_trip() {
        let w = worker("01JGXV0000000000000000AAAA");
        let c = criteria_by(CriteriaAuthor::Worker { id: w });
        let json = serde_json::to_string(&c).unwrap();
        let back: AcceptanceCriteria = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);

        // Document author serializes as a tagged unit variant.
        let d = criteria_by(CriteriaAuthor::Document);
        let json = serde_json::to_string(&d.authored_by).unwrap();
        assert_eq!(json, r#"{"kind":"document"}"#);
    }
}
