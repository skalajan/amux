//! Compaction lifecycle policy (RR-0069 skeleton, Invariant 31).
//!
//! Context exhaustion is not an error; it is a lifecycle event with a
//! defined protocol. This module is the PURE policy — which action the
//! protocol calls for at a given context level — consumed by the event
//! processor when `ContextLow(pct)` events arrive (that wiring lives with
//! orchestrator/events.rs, not here). The mechanics (building the summary,
//! swapping fragments, checkpoint + new session) land with the full
//! RR-0069.
//!
//! DIRECTION OF MEASUREMENT — the one thing to get right here: the plan's
//! thresholds are percent USED ("context 70% -> prepare, 85% -> compact,
//! 95% -> checkpoint + new session"), while this function takes percent
//! REMAINING, which is what providers report. 70/85/95 used = 30/15/5
//! remaining. A trigger fires when usage EXCEEDS its threshold (strictly
//! below in remaining terms), matching the plan's own test criterion
//! "compaction indicator on worker card when context > 70%": at exactly
//! 70% used (30 remaining) nothing fires yet.

use serde::{Deserialize, Serialize};

/// What the compaction protocol calls for at a context level (Invariant 31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionAction {
    /// >= 30% remaining (usage <= 70%): plenty of room, do nothing.
    None,
    /// < 30% remaining (usage > 70%): show the indicator and build the
    /// summary in the background, so compacting later is a swap, not a stall.
    PrepareIndicator,
    /// < 15% remaining (usage > 85%): compact — swap full history for the
    /// compacted representation. Source turns are never deleted; the
    /// compacted fragment supersedes them for assembly only.
    Compact,
    /// < 5% remaining (usage > 95%): checkpoint + new session. The worker is
    /// durable, the session is ephemeral; the new session hydrates from the
    /// compacted context and continues.
    ForceCompact,
}

/// Percent-remaining thresholds (= 100 - the plan's percent-used triggers).
/// Named so the policy, its tests, and any UI copy cannot drift apart.
pub const PREPARE_BELOW_PCT_REMAINING: u8 = 30;
pub const COMPACT_BELOW_PCT_REMAINING: u8 = 15;
pub const FORCE_BELOW_PCT_REMAINING: u8 = 5;

/// The action for a context level, measured as PERCENT REMAINING (0-100;
/// values above 100 clamp to "everything is fine"). Pure: same input, same
/// answer, no clock, no state — a `ContextLow(pct)` event carries everything
/// this needs.
pub fn compaction_action(context_pct_remaining: u8) -> CompactionAction {
    if context_pct_remaining < FORCE_BELOW_PCT_REMAINING {
        CompactionAction::ForceCompact
    } else if context_pct_remaining < COMPACT_BELOW_PCT_REMAINING {
        CompactionAction::Compact
    } else if context_pct_remaining < PREPARE_BELOW_PCT_REMAINING {
        CompactionAction::PrepareIndicator
    } else {
        CompactionAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_table() {
        // (percent remaining, expected action) — both sides of every
        // boundary, in the plan's usage terms:
        //   usage <= 70  (rem >= 30) -> None
        //   usage in (70, 85]  (rem in [15, 30)) -> PrepareIndicator
        //   usage in (85, 95]  (rem in [5, 15))  -> Compact
        //   usage  > 95  (rem < 5)   -> ForceCompact
        let table: &[(u8, CompactionAction)] = &[
            (100, CompactionAction::None),
            (50, CompactionAction::None),
            (31, CompactionAction::None),
            (30, CompactionAction::None), // exactly 70% used: not yet (plan: "> 70%")
            (29, CompactionAction::PrepareIndicator), // usage 71%: indicator on
            (16, CompactionAction::PrepareIndicator),
            (15, CompactionAction::PrepareIndicator), // exactly 85% used: still preparing
            (14, CompactionAction::Compact),          // usage 86%: compact
            (6, CompactionAction::Compact),
            (5, CompactionAction::Compact), // exactly 95% used: still compacting
            (4, CompactionAction::ForceCompact), // usage 96%: checkpoint + new session
            (1, CompactionAction::ForceCompact),
            (0, CompactionAction::ForceCompact),
        ];
        for (remaining, expected) in table {
            assert_eq!(
                compaction_action(*remaining),
                *expected,
                "at {remaining}% remaining ({}% used)",
                100u32.saturating_sub(u32::from(*remaining))
            );
        }
    }

    #[test]
    fn actions_are_monotonic_as_context_drains() {
        // Draining context must never step BACK to a milder action — a
        // policy that flaps between Compact and PrepareIndicator as the
        // number falls is a wrong answer that looks plausible (ethos 7).
        let severity = |a: CompactionAction| match a {
            CompactionAction::None => 0,
            CompactionAction::PrepareIndicator => 1,
            CompactionAction::Compact => 2,
            CompactionAction::ForceCompact => 3,
        };
        let mut last = severity(compaction_action(100));
        for remaining in (0..=100u8).rev() {
            let s = severity(compaction_action(remaining));
            assert!(s >= last, "severity dropped at {remaining}% remaining");
            last = s;
        }
    }

    #[test]
    fn serde_tokens_are_stable() {
        // The action rides in events/UI payloads; its wire spelling is a
        // contract, not an implementation detail.
        for (a, tok) in [
            (CompactionAction::None, "\"none\""),
            (CompactionAction::PrepareIndicator, "\"prepare_indicator\""),
            (CompactionAction::Compact, "\"compact\""),
            (CompactionAction::ForceCompact, "\"force_compact\""),
        ] {
            assert_eq!(serde_json::to_string(&a).unwrap(), tok);
            let back: CompactionAction = serde_json::from_str(tok).unwrap();
            assert_eq!(back, a);
        }
    }
}
