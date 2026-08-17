//! Runtime jobs (Phase 3, RR-0058..RR-0062): two DELIBERATELY separate kinds
//! of "run something later".
//!
//! # DurableSchedule vs PeriodicTask — why two types
//!
//! The plan is explicit ("these are different things with different
//! semantics") and the separation is type-level on purpose:
//!
//! - [`scheduler::DurableSchedule`] is USER data. It is DB-backed (the live
//!   `schedules` table the Python server also reads/writes), survives
//!   restarts, has run history (`schedule_runs` with a `source` field), an
//!   audit trail (`schedule_audit` with X-Amux-Session attribution),
//!   missed-run policy, and timezone semantics. Losing one is losing a
//!   user's intent; every mutation must be attributable.
//! - [`PeriodicTask`] is INTERNAL plumbing. In-memory only, no run history,
//!   no audit, gone on restart by design. It exists so maintenance loops
//!   (rot sweeps, cache refresh, heartbeats) never masquerade as user
//!   schedules — the 451-folds/journal-card lesson generalized: if internal
//!   ticks were rows in `schedules`, the user's schedule list would
//!   accumulate machinery it can neither own nor delete (ethos rule 8), and
//!   every internal tick would demand fake audit attribution (ethos rule 3).
//!
//! There is intentionally NO conversion between the two. A `PeriodicTask`
//! cannot be persisted; a `DurableSchedule` cannot be constructed without a
//! DB row. If you find yourself wanting a hybrid, one of the two lists above
//! tells you which properties your job actually needs.
//!
//! # DUAL-SCHEDULER SAFETY (critical while the strangler fig is live)
//!
//! The Python server on :8822 keeps running and OWNS schedule firing. Both
//! servers read/write the SAME `schedules` / `schedule_runs` rows, so two
//! live firing loops would double-deliver every schedule. Therefore:
//!
//! - The Rust firing loop ships DISABLED. `AMUX_RS_SCHEDULER=1` in the
//!   environment enables real firing; anything else means SHADOW MODE.
//! - In shadow mode [`scheduler::run_scheduler`] still computes next-run
//!   times and, for every schedule that comes due, records what it WOULD
//!   have fired as an `_amux_state_events` row with entity type
//!   `Other("schedule_shadow")` — one event per (schedule, occurrence),
//!   deduped in memory. It touches NEITHER `schedules` nor `schedule_runs`,
//!   so Python's loop is entirely undisturbed. Phase 11 validation compares
//!   these shadow events against Python's actual `schedule_runs` fires.
//! - The CRUD API (`api::schedules`) is always live — creates, edits,
//!   deletes and audit rows are shared state both servers agree on. The ONE
//!   exception is run-now: with firing disabled it answers 409
//!   `{"error":"rust scheduler is in shadow mode","enable":"AMUX_RS_SCHEDULER=1"}`
//!   instead of silently recording a run that delivered nothing (ethos rule
//!   3: the honest refusal beats the quiet lie).

pub mod autofix;
pub mod board_drive;
pub mod commit_nudge;
pub mod ghost_rescue;
pub mod pane_size;
/// The live registry of the jobs below — see [`registry`] for why it is
/// derived from the spawn sites rather than declared alongside them.
pub mod registry;
pub mod scheduler;
pub mod storage;
pub mod token_ledger;

pub use scheduler::{
    firing_enabled, run_scheduler, scheduler_tick, DueRuns, DurableSchedule, ExprParseError,
    MissedRunPolicy, RetryPolicy, ScheduleExpr, TickReport, SCHEDULER_TICK_SECS,
};

use std::time::Duration;
use tokio::time::MissedTickBehavior;

/// An internal, ephemeral periodic job. See the module docs for why this is
/// a different type from [`scheduler::DurableSchedule`] and must stay one:
/// no persistence, no history, no audit — by design, not by omission.
///
/// Each task runs on its OWN spawned tokio task driven by
/// `tokio::time::interval`, so a slow or wedged task cannot delay any other
/// task (the plan's non-blocking requirement; contrast Python's shared
/// `_JOB_REGISTRY` thread). A run that overshoots its interval delays only
/// its own next tick (`MissedTickBehavior::Delay` — no catch-up burst),
/// mirroring the Python registry's `_running` skip guard.
pub struct PeriodicTask {
    name: String,
    interval: Duration,
    handle: tokio::task::JoinHandle<()>,
}

impl PeriodicTask {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// True once the driving task has exited (only via `abort`; the loop
    /// itself never returns).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Stop the task. Fire-and-forget tasks are NOT aborted on drop — an
    /// internal maintenance loop should outlive the handle that spawned it
    /// unless someone explicitly says stop.
    pub fn abort(&self) {
        self.handle.abort();
    }
}

/// Spawn an internal periodic job: `f` is invoked every `secs` seconds on a
/// dedicated tokio task (first invocation immediately, matching
/// `tokio::time::interval` and Python's job registry, whose jobs are due on
/// the first tick). Used by later phases for internal maintenance loops.
///
/// This is the ONLY constructor for [`PeriodicTask`] — there is no DB row
/// behind it and none is created; a job that needs history or attribution
/// belongs in `schedules` via the API instead.
pub fn spawn_periodic<F, Fut>(name: impl Into<String>, secs: u64, f: F) -> PeriodicTask
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    spawn_periodic_every(name, Duration::from_secs(secs.max(1)), f)
}

/// [`spawn_periodic`] with a raw `Duration`, for sub-second internal ticks
/// (and for tests, which drive real ~tens-of-ms intervals — the workspace
/// tokio has no `test-util`, so there is no paused clock to lean on).
pub fn spawn_periodic_every<F, Fut>(name: impl Into<String>, interval: Duration, mut f: F) -> PeriodicTask
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let name = name.into();
    // VISIBILITY IS NOT OPTIONAL, and it is not the job's responsibility.
    // Because this is the ONLY constructor of a PeriodicTask, registering here
    // means every internal job — including ones written years from now by
    // someone who has never read `registry` — appears on
    // `GET /api/system-jobs` with a real interval and real tick timing. The
    // wrapper below brackets the job's own closure, so the tick timestamps are
    // recorded by the SPAWNER: a job cannot forget to report, and cannot
    // report a tick it did not run. See registry's docs for the three loops
    // that were dead for hours with nothing visible anywhere.
    let job_id = name.clone();
    let handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        // Delay, not Burst: a run that overshoots must not be "paid back"
        // with a rapid-fire catch-up volley (the spin-catcher lesson — a
        // detector/maintainer must not amplify the load it exists to manage).
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            registry::tick_start(&job_id);
            f().await;
            registry::tick_end(&job_id);
        }
    });
    registry::register(&name, "periodic", Some(interval), Some(handle.abort_handle()));
    PeriodicTask {
        name,
        interval,
        handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Type separation is a compile-time property; this test documents it as
    // an executable statement rather than prose: PeriodicTask's public
    // surface has NO persistence/history/audit affordances to call, and
    // DurableSchedule (scheduler.rs) has no spawn affordance. If someone
    // adds a `PeriodicTask::persist` or `DurableSchedule::spawn`, this
    // comment is the tripwire explaining why review should bounce it.
    #[tokio::test]
    async fn periodic_task_exposes_only_ephemeral_surface() {
        let t = spawn_periodic("noop", 60, || async {});
        assert_eq!(t.name(), "noop");
        assert_eq!(t.interval(), Duration::from_secs(60));
        assert!(!t.is_finished());
        t.abort();
    }

    #[tokio::test]
    async fn ticks_at_interval() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let t = spawn_periodic_every("fast", Duration::from_millis(25), move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let n = count.load(Ordering::SeqCst);
        // Immediate first tick + ~7 more; generous bounds for CI jitter.
        assert!((4..=12).contains(&n), "expected ~8 ticks in 200ms, got {n}");
        t.abort();
    }

    #[tokio::test]
    async fn slow_task_does_not_block_others() {
        let fast = Arc::new(AtomicUsize::new(0));
        let slow = Arc::new(AtomicUsize::new(0));
        let f = fast.clone();
        let tf = spawn_periodic_every("fast", Duration::from_millis(20), move || {
            let f = f.clone();
            async move {
                f.fetch_add(1, Ordering::SeqCst);
            }
        });
        let s = slow.clone();
        let ts = spawn_periodic_every("slow", Duration::from_millis(20), move || {
            let s = s.clone();
            async move {
                s.fetch_add(1, Ordering::SeqCst);
                // Far longer than everyone's interval: if tasks shared a
                // driver, this would starve "fast".
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        let nf = fast.load(Ordering::SeqCst);
        let ns = slow.load(Ordering::SeqCst);
        assert!(nf >= 8, "fast task starved by slow one: {nf} ticks in 400ms");
        assert!((1..=2).contains(&ns), "slow task should be mid-first-run: {ns}");
        tf.abort();
        ts.abort();
    }
}
