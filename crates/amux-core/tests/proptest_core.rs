//! Property tests (RR-0028, Invariant 22).
//!
//! Fuzzes the pure state machines with arbitrary inputs and asserts the
//! invariants that unit tests can only spot-check: no random transition
//! sequence reaches an illegal state, merge precedence never inverts, hash
//! determinism holds for every permutation, completeness flags never lie.
//! Board/disposition properties join this file when board.rs lands.

use amux_core::ids::{CommandId, WorkerId};
use amux_core::protocol::{
    next_state, CommandState, CommandTransition, DeliveryTiming, QueuedCommand, WorkerCommand,
};
use amux_core::scope::{effective_config, LayeredMap, Mergeable, ScopeLevel};
use amux_core::search::PagedResponse;
use amux_core::turn::{ContextFragment, ContextSnapshot};
use chrono::TimeZone;
use proptest::prelude::*;

fn arb_transition() -> impl Strategy<Value = CommandTransition> {
    prop_oneof![
        Just(CommandTransition::Dispatch),
        Just(CommandTransition::Deliver),
        Just(CommandTransition::Confirm),
        ".{0,12}".prop_map(|reason| CommandTransition::Fail { reason }),
        Just(CommandTransition::Retry),
    ]
}

fn fixed_ulid() -> ulid::Ulid {
    "01JGXV0000000000000000TEST".parse().unwrap()
}

proptest! {
    /// Whatever sequence of transitions arrives, the command never lands in
    /// a state the contract forbids, terminal states stay terminal, and
    /// attempts only grow.
    #[test]
    fn command_state_machine_never_reaches_illegal_state(
        transitions in proptest::collection::vec(arb_transition(), 0..40),
        max_attempts in 1u32..5,
    ) {
        let mut cmd = QueuedCommand::new(
            CommandId::from_ulid(fixed_ulid()),
            WorkerId::from_ulid(fixed_ulid()),
            WorkerCommand::Continue,
            "key".into(),
            chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            DeliveryTiming::AtTurnBoundary,
            None,
        );
        let mut was_terminal = false;
        let mut prev_attempts = cmd.attempts;
        for t in transitions {
            let before = cmd.state.clone();
            let result = cmd.apply(t, max_attempts);
            if was_terminal {
                // Terminal states accept nothing, ever.
                prop_assert!(result.is_err());
                prop_assert_eq!(&cmd.state, &before);
            }
            prop_assert!(cmd.attempts >= prev_attempts);
            prev_attempts = cmd.attempts;
            if matches!(cmd.state, CommandState::Confirmed | CommandState::DeadLettered { .. }) {
                was_terminal = true;
            }
        }
        // Attempts can never exceed the budget by more than the final
        // failure that dead-letters.
        prop_assert!(cmd.attempts <= max_attempts + 1);
    }

    /// Dead-lettering requires the retry budget to actually be spent: no
    /// sequence shorter than max_attempts failures can dead-letter.
    #[test]
    fn dead_letter_needs_the_budget_spent(
        max_attempts in 1u32..5,
    ) {
        let state = CommandState::Failed { reason: "x".into() };
        // With fewer recorded attempts than the budget, Retry requeues.
        let requeued = next_state(&state, &CommandTransition::Retry, max_attempts - 1, max_attempts).unwrap();
        prop_assert_eq!(requeued, CommandState::Queued);
        // At the budget, Retry dead-letters, preserving the reason.
        let dead = next_state(&state, &CommandTransition::Retry, max_attempts, max_attempts).unwrap();
        let is_dead_with_reason =
            matches!(dead, CommandState::DeadLettered { ref reason } if reason == "x");
        prop_assert!(is_dead_with_reason);
    }

    /// Layered-map resolution: the most specific layer that defines a key
    /// always wins, regardless of what the other layers contain.
    #[test]
    fn layered_map_most_specific_wins(
        org in proptest::option::of(".{1,8}"),
        global in proptest::option::of(".{1,8}"),
        group in proptest::option::of(".{1,8}"),
        worker in proptest::option::of(".{1,8}"),
    ) {
        let mut m = LayeredMap::default();
        if let Some(v) = &org { m.org.insert("K".into(), v.clone()); }
        if let Some(v) = &global { m.global.insert("K".into(), v.clone()); }
        if let Some(v) = &group { m.group.insert("K".into(), v.clone()); }
        if let Some(v) = &worker { m.worker.insert("K".into(), v.clone()); }

        let expected = worker.as_ref().map(|v| (v, ScopeLevel::Worker))
            .or(group.as_ref().map(|v| (v, ScopeLevel::Group)))
            .or(global.as_ref().map(|v| (v, ScopeLevel::Global)))
            .or(org.as_ref().map(|v| (v, ScopeLevel::Org)));

        match (m.resolve("K"), expected) {
            (None, None) => {}
            (Some(got), Some((v, lvl))) => {
                prop_assert_eq!(got.value, v.as_str());
                prop_assert_eq!(got.source, lvl);
            }
            (got, want) => prop_assert!(false, "resolve mismatch: {:?} vs {:?}", got, want),
        }
    }

    /// effective_config precedence with an option-field config: any layer
    /// combination resolves to the most specific Some.
    #[test]
    fn effective_config_precedence(
        org in proptest::option::of(0u32..100),
        global in proptest::option::of(0u32..100),
        group in proptest::option::of(0u32..100),
        worker in proptest::option::of(0u32..100),
    ) {
        #[derive(Debug, Clone, Default, PartialEq)]
        struct C(Option<u32>);
        impl Mergeable for C {
            fn merge(&mut self, other: &Self) {
                if other.0.is_some() { self.0 = other.0; }
            }
        }
        let eff = effective_config(
            Some(&C(org)), Some(&C(global)), Some(&C(group)), Some(&C(worker)),
        );
        let expected = worker.or(group).or(global).or(org);
        prop_assert_eq!(eff.0, expected);
    }

    /// ContextSnapshot: any permutation of the same fragments hashes
    /// identically (Invariant 27 — a snapshot is identified by content, not
    /// by assembly order).
    #[test]
    fn context_snapshot_hash_is_permutation_invariant(
        mut frags in proptest::collection::vec(
            (0u32..5, "[a-c]{1,4}", "[a-d]{0,6}").prop_map(|(p, s, c)| ContextFragment {
                priority: p, source: s, content: c,
            }),
            0..12,
        ),
        seed in 0u64..1000,
    ) {
        let original = ContextSnapshot::build(frags.clone());
        // Deterministic shuffle from the seed.
        let mut s = seed;
        for i in (1..frags.len()).rev() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let j = (s >> 33) as usize % (i + 1);
            frags.swap(i, j);
        }
        let shuffled = ContextSnapshot::build(frags);
        prop_assert_eq!(original.content_hash, shuffled.content_hash);
    }

    /// PagedResponse can never claim fewer total items than it returned, and
    /// `truncated` is true exactly when items exist beyond this page.
    #[test]
    fn paged_response_completeness_invariant(
        n_items in 0usize..50,
        extra in 0u64..100,
        offset in 0u64..100,
    ) {
        let items: Vec<u32> = (0..n_items as u32).collect();
        let total = items.len() as u64 + extra;
        match PagedResponse::new(items.clone(), total, offset, 50) {
            Ok(page) => {
                prop_assert!(page.total >= page.items.len() as u64);
                let expected_truncated = total > offset + items.len() as u64;
                prop_assert_eq!(page.truncated, expected_truncated);
            }
            Err(_) => prop_assert!(false, "constructor rejected a consistent page"),
        }
        // And the lying case is always rejected.
        if n_items > 0 {
            let lie = PagedResponse::new(items, n_items as u64 - 1, 0, 50);
            prop_assert!(lie.is_err());
        }
    }
}
