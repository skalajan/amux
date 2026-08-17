//! Revisioned state: global revision, state events, mutation truthfulness
//! (RR-0008, Invariants 35 and 37).
//!
//! Invariant 35: the backend database is authoritative, and every mutating
//! transaction increments ONE monotonic global revision. The revision is the
//! single ordering for all state changes across all entity types — the UI
//! never infers "latest" from whichever SSE message arrived last, it compares
//! `rev`. Each entity additionally carries its own `version` (staleness of
//! THIS entity); global `rev` drives sync ordering, entity `version` drives
//! optimistic concurrency.
//!
//! Invariant 37: a mutation response states exactly what was applied, and
//! `rev` increments IFF authoritative state changed. The Python defect this
//! fixes: PATCHes returned 200 with a bumped rev on no-ops, so "did it
//! change?" was unanswerable from the response. Here the type enforces it —
//! [`MutationResult::noop`] structurally cannot carry a new revision.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The monotonic global revision. `u64`, never reset, incremented in the
/// same transaction as the mutation it stamps. Public inner value because
/// the persistence layer reads/writes it directly; monotonicity is the
/// STORE's contract (single-row `global_rev` table updated transactionally)
/// — this type gives callers the arithmetic that cannot get it wrong.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct StateRevision(pub u64);

impl StateRevision {
    /// The pre-history revision: no mutation has ever happened.
    pub const ZERO: StateRevision = StateRevision(0);

    /// The successor revision. The ONLY way forward — there is deliberately
    /// no `prev()`: a revision that can move backwards is not an ordering
    /// (Invariant 35).
    pub fn next(self) -> StateRevision {
        StateRevision(self.0 + 1)
    }
}

impl std::fmt::Display for StateRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Which kind of entity a state event touched. `Other(String)` keeps the
/// enum open so a new entity kind does not require recompiling every
/// consumer of the event stream (the same reasoning as Invariant 8's open
/// provider IDs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EntityType {
    Worker,
    Task,
    Message,
    Schedule,
    Group,
    Memory,
    Gate,
    Session,
    Turn,
    Verification,
    Other(String),
}

/// What a mutation did. `StatusChanged` names both sides because "status
/// changed" without from/to forces every consumer to re-fetch to learn what
/// happened — the discriminator belongs in the event (ethos rule 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationKind {
    Created,
    Updated,
    Deleted,
    StatusChanged { from: String, to: String },
}

/// One revisioned mutation notification (Invariant 35). SSE carries these;
/// the delta-sync endpoint replays them. `entity_id` is a string because the
/// stream is heterogeneous across entity types — the typed IDs live on the
/// entities themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEvent {
    /// Global revision stamped by the mutating transaction.
    pub rev: StateRevision,
    pub entity_type: EntityType,
    pub entity_id: String,
    pub mutation: MutationKind,
    /// When the mutation committed. Supplied by the caller — core never
    /// reads a clock.
    pub at: DateTime<Utc>,
}

/// Are these events in strictly increasing revision order? Strict, because
/// two mutations sharing a revision would mean two transactions stamped the
/// same rev — a store bug this helper exists to catch, not to tolerate.
/// Used by tests and by the sync layer's self-checks (ethos rule 7: a check
/// that cannot fail is theatre; this one fails on duplicates AND regressions).
pub fn revisions_monotonic(events: &[StateEvent]) -> bool {
    events.windows(2).all(|w| w[0].rev < w[1].rev)
}

/// What a mutation request actually did (Invariant 37). The contract:
///
/// - `applied == true`  => `rev` and `version` are `Some`, freshly bumped.
/// - `applied == false` => `rev` and `version` are `None`: a no-op DID NOT
///   move the world, and the response cannot pretend it did. (The Python
///   system returned 200 + bumped rev on no-ops; a client could not
///   distinguish "I changed it" from "it was already there".)
///
/// `ignored_fields` names request fields that were not applied. Per
/// Invariant 37 unknown fields should be REJECTED at the API boundary
/// (`deny_unknown_fields`); this vec is for the residual legitimate cases
/// (e.g. a field dropped by policy), and it exists so the caller can SEE the
/// drop — carrying it and never reading it was itself a logged incident
/// (cold-outbound, 2026-08-07).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationResult {
    pub applied: bool,
    /// The new global revision — only present when state actually changed.
    pub rev: Option<StateRevision>,
    /// The entity's new version — only present when state actually changed.
    pub version: Option<u64>,
    pub ignored_fields: Vec<String>,
    pub entity_id: String,
}

impl MutationResult {
    /// A mutation that changed authoritative state: carries the bumped
    /// revision and entity version.
    pub fn applied(entity_id: impl Into<String>, rev: StateRevision, version: u64) -> Self {
        Self {
            applied: true,
            rev: Some(rev),
            version: Some(version),
            ignored_fields: Vec::new(),
            entity_id: entity_id.into(),
        }
    }

    /// A no-op: the requested state already held. Structurally carries no
    /// new rev/version — this constructor is WHY the Invariant 37 defect
    /// cannot be reintroduced by a forgetful call site.
    pub fn noop(entity_id: impl Into<String>) -> Self {
        Self {
            applied: false,
            rev: None,
            version: None,
            ignored_fields: Vec::new(),
            entity_id: entity_id.into(),
        }
    }

    /// Attach the names of fields that were not applied, so the drop is
    /// visible in the response body rather than silent.
    pub fn with_ignored_fields(mut self, fields: Vec<String>) -> Self {
        self.ignored_fields = fields;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: u32) -> DateTime<Utc> {
        chrono::Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, secs)
            .unwrap()
    }

    fn ev(rev: u64) -> StateEvent {
        StateEvent {
            rev: StateRevision(rev),
            entity_type: EntityType::Task,
            entity_id: "tsk_01JGXV0000000000000000TEST".into(),
            mutation: MutationKind::Updated,
            at: t(rev as u32),
        }
    }

    #[test]
    fn revision_next_is_strictly_increasing() {
        let mut rev = StateRevision::ZERO;
        for expected in 1..=5u64 {
            let next = rev.next();
            assert!(next > rev);
            assert_eq!(next, StateRevision(expected));
            rev = next;
        }
    }

    #[test]
    fn monotonic_helper_accepts_ordered_streams() {
        assert!(revisions_monotonic(&[]));
        assert!(revisions_monotonic(&[ev(1)]));
        assert!(revisions_monotonic(&[ev(1), ev(2), ev(7)])); // gaps are fine (filtered streams)
    }

    #[test]
    fn monotonic_helper_detects_regression_and_duplicates() {
        // Regression: an out-of-order rev means the stream cannot be trusted
        // for "latest wins".
        assert!(!revisions_monotonic(&[ev(2), ev(1)]));
        // Duplicate: two transactions stamped the same rev — a store bug,
        // not a tolerable tie.
        assert!(!revisions_monotonic(&[ev(3), ev(3)]));
    }

    #[test]
    fn noop_mutation_carries_no_new_rev() {
        // Invariant 37: applied=false, rev/version absent. This is the
        // response shape the Python system could not produce (200 + bumped
        // rev on a no-op made "did it change?" unanswerable).
        let r = MutationResult::noop("tsk_01JGXV0000000000000000TEST");
        assert!(!r.applied);
        assert_eq!(r.rev, None);
        assert_eq!(r.version, None);
        assert!(r.ignored_fields.is_empty());
    }

    #[test]
    fn applied_mutation_carries_rev_and_version() {
        let r = MutationResult::applied("tsk_01JGXV0000000000000000TEST", StateRevision(42), 7)
            .with_ignored_fields(vec!["legacy_field".into()]);
        assert!(r.applied);
        assert_eq!(r.rev, Some(StateRevision(42)));
        assert_eq!(r.version, Some(7));
        assert_eq!(r.ignored_fields, vec!["legacy_field".to_string()]);
    }

    #[test]
    fn state_event_round_trips_with_status_change() {
        let e = StateEvent {
            rev: StateRevision(9),
            entity_type: EntityType::Other("webhook".into()),
            entity_id: "wh-1".into(),
            mutation: MutationKind::StatusChanged {
                from: "todo".into(),
                to: "doing".into(),
            },
            at: t(9),
        };
        let json = serde_json::to_string(&e).unwrap();
        // from/to are IN the event: a consumer never has to re-fetch to
        // learn what a status change was (ethos rule 4).
        assert!(json.contains("\"from\":\"todo\""), "{json}");
        let back: StateEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
