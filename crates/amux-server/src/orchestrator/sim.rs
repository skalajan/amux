//! Deterministic simulation primitives (RR-0027, Invariant 22).
//!
//! The simulation owns time and randomness, so a failing 100-sequence fuzz
//! run can be replayed from its seed and fail identically. The fake backend
//! arrives with the SessionBackend trait in Phase 1; the clock and RNG land
//! first because every later component must be written against them.

use super::Clock;
use chrono::{DateTime, TimeZone, Utc};
use std::sync::atomic::{AtomicI64, Ordering};

/// A clock the test advances by hand. Starts at a fixed epoch so two runs
/// of the same scenario see identical timestamps.
pub struct FakeClock {
    // Milliseconds since epoch; atomic so sim tasks on multiple threads
    // share one timeline without locking.
    now_ms: AtomicI64,
}

impl FakeClock {
    pub fn new() -> Self {
        // 2026-01-01T00:00:00Z — arbitrary, fixed, obviously fake in logs.
        Self::at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
    }

    pub fn at(start: DateTime<Utc>) -> Self {
        FakeClock {
            now_ms: AtomicI64::new(start.timestamp_millis()),
        }
    }

    pub fn advance_ms(&self, ms: i64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn advance_secs(&self, secs: i64) {
        self.advance_ms(secs * 1000);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self.now_ms.load(Ordering::SeqCst))
            .single()
            .expect("fake clock in range")
    }
}

/// Small, dependency-free splitmix64 PRNG. Deterministic across platforms
/// and Rust versions — `rand` upgrades must never change a recorded
/// simulation's outcome, so the simulation does not use `rand`.
pub struct SimRng {
    state: u64,
}

impl SimRng {
    pub fn new(seed: u64) -> Self {
        SimRng { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, bound). Bound of 0 returns 0.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        self.next_u64() % bound
    }

    /// Deterministic ULID from sim randomness — entity IDs inside a
    /// simulation replay identically.
    pub fn ulid(&mut self, clock: &dyn Clock) -> ulid::Ulid {
        let ts = clock.now().timestamp_millis() as u64;
        let hi = self.next_u64() as u128;
        let lo = self.next_u64() as u128;
        ulid::Ulid::from_parts(ts, (hi << 64 | lo) & ((1u128 << 80) - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_is_deterministic_and_advances() {
        let c1 = FakeClock::new();
        let c2 = FakeClock::new();
        assert_eq!(c1.now(), c2.now());
        c1.advance_secs(90);
        assert_eq!((c1.now() - c2.now()).num_seconds(), 90);
    }

    #[test]
    fn rng_replays_identically_from_seed() {
        let a: Vec<u64> = {
            let mut r = SimRng::new(42);
            (0..100).map(|_| r.next_u64()).collect()
        };
        let b: Vec<u64> = {
            let mut r = SimRng::new(42);
            (0..100).map(|_| r.next_u64()).collect()
        };
        assert_eq!(a, b);
        let c: Vec<u64> = {
            let mut r = SimRng::new(43);
            (0..100).map(|_| r.next_u64()).collect()
        };
        assert_ne!(a, c, "different seeds must diverge");
    }

    #[test]
    fn sim_ulids_are_deterministic() {
        let clock = FakeClock::new();
        let mut r1 = SimRng::new(7);
        let mut r2 = SimRng::new(7);
        assert_eq!(r1.ulid(&clock), r2.ulid(&clock));
    }
}
