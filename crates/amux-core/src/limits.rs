//! Anti-livelock execution limits (Invariants 47 + 49, RR-0028f).
//!
//! No-stall (Invariant 10) guarantees forward motion. Without a
//! complementary no-thrash guarantee, "forward motion" degenerates into
//! retrying the same verification forever — no-stall without no-thrash
//! means burning tokens indefinitely. Every task carries
//! [`ExecutionLimits`]; [`check`] is the single exhaustion predicate the
//! orchestrator consults before granting another attempt.
//!
//! The companion rule is Invariant 49: **retries are not re-rolls.** Each
//! [`AttemptRecord`] carries WHY the previous attempt failed — the failure
//! reason, the evidence that was rejected, whether decomposition was tried —
//! and the orchestrator constructs attempt N+1's context from the prior
//! records. The agent cannot opt out of seeing them; re-rolls are how
//! autonomous systems spend $40k on the same bug.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Per-task execution budget (Invariant 47). Exhausting ANY limit stops the
/// retry loop: first automatic decomposition, then `Quarantined` — never a
/// silent retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLimits {
    pub max_attempts: u32,
    pub max_tokens: u64,
    pub max_wall_clock_secs: u64,
}

impl Default for ExecutionLimits {
    /// Invariant 47's stated defaults: 5 attempts, 500k tokens, 4 hours.
    fn default() -> Self {
        ExecutionLimits {
            max_attempts: 5,
            max_tokens: 500_000,
            max_wall_clock_secs: 4 * 3600,
        }
    }
}

/// The durable record of one failed attempt — what attempt N+1 is REQUIRED
/// to see (Invariant 49). A record that only said "failed" would make the
/// next attempt a re-roll; the fields here are the discriminators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    /// The specific verification failure, not just "failed".
    pub failure_reason: String,
    /// What was tried and why it did not satisfy the gate — so the next
    /// attempt does not repeat it.
    pub rejected_evidence: Vec<String>,
    pub tokens_spent: u64,
    pub wall_clock_secs: u64,
    /// Whether decomposition was attempted (Invariant 47 step 1); two failed
    /// decompositions lead to `Quarantined`.
    pub decomposition_attempted: bool,
    /// `git status` snapshot at failure time, REQUIRED under
    /// `IsolationPolicy::Shared` (RR-0028k, Invariant 33): a shared tree
    /// means the failure may be another worker's breakage, and the record is
    /// actively misleading unless the next attempt can distinguish "my code
    /// broke" from "the tree was dirty when I started". `None` under
    /// Worktree/Container, where the tree is the worker's own.
    pub tree_status: Option<String>,
    pub at: DateTime<Utc>,
}

/// Retry pacing for externally-blocked or flaky checks. The schedule bounds
/// the retry loop the same way `ExecutionLimits` bounds the task: an
/// interval with no `max_attempts` is an infinite loop wearing a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrySchedule {
    pub interval_secs: u64,
    pub max_attempts: u32,
}

/// Which budget ran out. Named so the diagnostic report and the
/// decomposition decision can discriminate — "exhausted" without WHICH is
/// the kind of answer ethos rule 4 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustedLimit {
    Attempts,
    Tokens,
    WallClock,
}

/// Result of [`check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LimitCheck {
    WithinLimits,
    Exhausted { which: ExhaustedLimit },
}

/// The single exhaustion predicate (Invariant 47). Pure: elapsed wall clock
/// is a parameter, never a clock read.
///
/// - attempts exhausted when `attempts.len() >= max_attempts`
/// - tokens exhausted when the SUM of `tokens_spent` reaches `max_tokens`
///   (saturating — an overflowing spend total must read as exhausted, not
///   wrap back to "plenty of budget left")
/// - wall clock exhausted when `elapsed_secs >= max_wall_clock_secs`
///
/// Checked in that order; the first exhausted limit is reported. The order
/// only decides which discriminator wins when several are exhausted at once.
pub fn check(
    limits: &ExecutionLimits,
    attempts: &[AttemptRecord],
    elapsed_secs: u64,
) -> LimitCheck {
    if attempts.len() as u64 >= limits.max_attempts as u64 {
        return LimitCheck::Exhausted {
            which: ExhaustedLimit::Attempts,
        };
    }
    let spent = attempts
        .iter()
        .fold(0u64, |acc, a| acc.saturating_add(a.tokens_spent));
    if spent >= limits.max_tokens {
        return LimitCheck::Exhausted {
            which: ExhaustedLimit::Tokens,
        };
    }
    if elapsed_secs >= limits.max_wall_clock_secs {
        return LimitCheck::Exhausted {
            which: ExhaustedLimit::WallClock,
        };
    }
    LimitCheck::WithinLimits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        "2026-08-01T00:00:00Z".parse().unwrap()
    }

    fn attempt(n: u32, tokens: u64) -> AttemptRecord {
        AttemptRecord {
            attempt: n,
            failure_reason: format!("verifier rejected evidence on attempt {n}"),
            rejected_evidence: vec!["cargo test exit 101: assertion failed in gate_eval".into()],
            tokens_spent: tokens,
            wall_clock_secs: 60,
            decomposition_attempted: false,
            tree_status: None,
            at: t0(),
        }
    }

    fn limits() -> ExecutionLimits {
        ExecutionLimits {
            max_attempts: 3,
            max_tokens: 1_000,
            max_wall_clock_secs: 100,
        }
    }

    #[test]
    fn attempts_exhaustion_at_threshold_not_below() {
        let l = limits();
        let two: Vec<_> = (1..=2).map(|n| attempt(n, 10)).collect();
        assert_eq!(check(&l, &two, 0), LimitCheck::WithinLimits);

        let three: Vec<_> = (1..=3).map(|n| attempt(n, 10)).collect();
        assert_eq!(
            check(&l, &three, 0),
            LimitCheck::Exhausted {
                which: ExhaustedLimit::Attempts
            }
        );
    }

    #[test]
    fn tokens_exhaustion_at_threshold_not_below() {
        let l = limits();
        assert_eq!(
            check(&l, &[attempt(1, 500), attempt(2, 499)], 0),
            LimitCheck::WithinLimits
        );
        assert_eq!(
            check(&l, &[attempt(1, 500), attempt(2, 500)], 0),
            LimitCheck::Exhausted {
                which: ExhaustedLimit::Tokens
            }
        );
    }

    #[test]
    fn wall_clock_exhaustion_at_threshold_not_below() {
        let l = limits();
        assert_eq!(check(&l, &[attempt(1, 1)], 99), LimitCheck::WithinLimits);
        assert_eq!(
            check(&l, &[attempt(1, 1)], 100),
            LimitCheck::Exhausted {
                which: ExhaustedLimit::WallClock
            }
        );
    }

    #[test]
    fn token_summing_saturates_instead_of_wrapping() {
        let l = limits();
        // A wrapped sum would read as a tiny spend and grant another
        // attempt; saturation must report Exhausted::Tokens.
        let records = [attempt(1, u64::MAX), attempt(2, u64::MAX)];
        assert_eq!(
            check(&l, &records, 0),
            LimitCheck::Exhausted {
                which: ExhaustedLimit::Tokens
            }
        );
    }

    #[test]
    fn attempt_record_serde_round_trip() {
        let mut r = attempt(2, 1234);
        r.tree_status = Some("M amux-server.py\n?? scratch.txt".into());
        r.decomposition_attempted = true;
        let json = serde_json::to_string(&r).unwrap();
        let back: AttemptRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn limit_check_serde_shape() {
        let json = serde_json::to_string(&LimitCheck::Exhausted {
            which: ExhaustedLimit::Tokens,
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"exhausted","which":"tokens"}"#);
        let back: LimitCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            LimitCheck::Exhausted {
                which: ExhaustedLimit::Tokens
            }
        );
    }

    #[test]
    fn defaults_match_invariant_47() {
        let d = ExecutionLimits::default();
        assert_eq!(d.max_attempts, 5);
        assert_eq!(d.max_tokens, 500_000);
        assert_eq!(d.max_wall_clock_secs, 4 * 3600);
    }
}
