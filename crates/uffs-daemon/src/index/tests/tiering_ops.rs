// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Operator-driven memory-tiering tests for [`super::IndexManager`]
//! covering [`crate::index::tiering_ops::IndexManager::hibernate_shards`]
//! (Phase 8-B) and [`crate::index::tiering_ops::IndexManager::preload_drive`]
//! (Phase 8-C).
//!
//! Pin-contract assertions:
//!
//! * `preload C: → C is Hot ≥ pin window` (idle-demote skips pinned)
//! * `preload C: + cascade-demote → C stays Hot` (cascade-demote skips pinned)
//! * `preload C: + hibernate → C demotes to Cold` (explicit beats pin)
//!
//! All tests use the `with_body_loader_for_test` constructor so the
//! Cold/Parked → Warm/Hot body-load path is fed by a deterministic
//! [`super::FixedBodyLoader`] (or
//! [`super::body_loader_fakes::MissingBodyLoader`] for the `LoadFailed` case).

#![expect(
    clippy::std_instead_of_alloc,
    reason = "test fixtures — `std::sync::Arc` matches the rest of the daemon's \
              test fixtures, no need to switch to `alloc::sync::Arc` for tests"
)]

use std::sync::Arc;

use super::body_loader_fakes::MissingBodyLoader;
use super::{FixedBodyLoader, build_test_drive, build_test_drive_d, build_test_drive_e};
use crate::cache::policy::WARM_TO_PARKED_IDLE_SECS;
use crate::cache::{ShardState, unix_now_ms};
use crate::index::IndexManager;
use crate::index::tiering_ops::PreloadOutcome;

// ── Test-only Hot-state seed escape hatch ─────────────────────────
//
// The production `IndexManager` only reaches `Hot` via
// [`crate::index::IndexManager::preload_drive`].  The pre-existing
// `demote_letter_for_test` mutator (in `crate::index::test_helpers`)
// covers Warm→Parked→Cold seeding; the cascade-demote test below
// needs to seed a `Warm` shard with a known `last_query_at_ms` and
// then assert the cascade picks/skips it correctly.  No new escape
// hatch is required — the pre-existing `add_drive` lands shards in
// `Warm` by default.

/// Phase 8-B — `hibernate_shards(&[])` walks every loaded shard and
/// demotes each non-Cold shard to `Cold` in a single write-lock
/// batch.  Empty `drives` parameter = "every loaded drive".
#[tokio::test]
async fn hibernate_demotes_all_loaded_drives_to_cold() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    let outcome = mgr.hibernate_shards(&[]).await;

    assert_eq!(
        outcome.warm_demoted,
        vec![
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D
        ],
        "both freshly-loaded Warm drives must be reported as warm-demoted"
    );
    assert_eq!(
        outcome.hot_demoted,
        Vec::<uffs_mft::platform::DriveLetter>::new()
    );
    assert_eq!(
        outcome.parked_demoted,
        Vec::<uffs_mft::platform::DriveLetter>::new()
    );
    assert_eq!(
        outcome.already_cold,
        Vec::<uffs_mft::platform::DriveLetter>::new()
    );

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![
            (uffs_mft::platform::DriveLetter::C, ShardState::Cold),
            (uffs_mft::platform::DriveLetter::D, ShardState::Cold)
        ],
        "every shard must be Cold post-hibernate"
    );
}

/// Phase 8-B — `hibernate_shards(&[uffs_mft::platform::DriveLetter::C])` only
/// demotes the named drive; other loaded drives stay in their pre-call tier.
#[tokio::test]
async fn hibernate_subset_only_demotes_specified_drives() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    let outcome = mgr
        .hibernate_shards(&[uffs_mft::platform::DriveLetter::C])
        .await;

    assert_eq!(outcome.warm_demoted, vec![
        uffs_mft::platform::DriveLetter::C
    ]);
    assert!(
        outcome.already_cold.is_empty(),
        "non-targeted drives are filtered out before the tier check, not reported in already_cold"
    );

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![
            (uffs_mft::platform::DriveLetter::C, ShardState::Cold),
            (uffs_mft::platform::DriveLetter::D, ShardState::Warm),
            (uffs_mft::platform::DriveLetter::E, ShardState::Warm),
        ],
        "only C must be Cold; D and E must stay Warm"
    );
}

/// Phase 8-B — already-`Cold` drives land in
/// [`HibernateOutcome::already_cold`] without producing a registry
/// rebuild (no `shard.transition` event, no `index_version` bump).
#[tokio::test]
async fn hibernate_reports_already_cold_drives_separately() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    // Seed C as Cold via the pre-existing test escape hatch.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );

    let outcome = mgr.hibernate_shards(&[]).await;

    assert_eq!(outcome.already_cold, vec![
        uffs_mft::platform::DriveLetter::C
    ]);
    assert_eq!(outcome.warm_demoted, vec![
        uffs_mft::platform::DriveLetter::D
    ]);
    assert_eq!(
        outcome.hot_demoted,
        Vec::<uffs_mft::platform::DriveLetter>::new()
    );
    assert_eq!(
        outcome.parked_demoted,
        Vec::<uffs_mft::platform::DriveLetter>::new()
    );
}

/// Phase 8-C contract — `preload C:` against a `Cold` shard goes
/// `Cold → Hot` via the body loader, arms the pin, and reports the
/// pre-call tier so the operator audit trail captures the
/// transition.
#[tokio::test]
async fn preload_promotes_cold_to_hot_with_pin() {
    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;

    // Seed C as Cold so preload exercises the body-load path.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );

    let before_ms = unix_now_ms();
    let outcome = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::C, 30)
        .await;

    let PreloadOutcome::Promoted {
        from_state,
        pin_until_ms,
    } = outcome
    else {
        panic!("expected Promoted, got {outcome:?}");
    };
    assert_eq!(from_state, ShardState::Cold);
    // 30-min pin in the future, sampled inside the preload call.
    let thirty_min_ms = 30_u64 * 60 * 1000;
    assert!(
        pin_until_ms >= before_ms.saturating_add(thirty_min_ms),
        "pin_until_ms={pin_until_ms} must be at least 30 min after the pre-call wall clock ({before_ms})"
    );

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Hot
    )]);
}

/// Phase 8-C — `preload C:` against an already-`Hot` shard skips
/// the registry rebuild entirely and atomically extends the pin
/// window via [`crate::cache::shard::ShardEntry::pin_until`].
#[tokio::test]
async fn preload_already_hot_extends_pin_without_rebuild() {
    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );

    // First preload — Cold → Hot.  Use let-else so neither a
    // wildcard arm (clippy::wildcard_enum_match_arm) nor an
    // `unreachable!` (clippy::unreachable) is needed.
    let first = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::C, 5)
        .await;
    let PreloadOutcome::Promoted {
        pin_until_ms: pin1, ..
    } = first
    else {
        panic!("expected Promoted on the first preload call, got {first:?}");
    };

    // Second preload — Hot → Hot (pin extension only).
    let second = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::C, 60)
        .await;
    let PreloadOutcome::AlreadyHot { pin_until_ms: pin2 } = second else {
        panic!("expected AlreadyHot on the second preload call, got {second:?}");
    };
    assert!(
        pin2 > pin1,
        "second preload (60-min pin) must extend the pin window past the first (5-min pin): \
         pin1={pin1}, pin2={pin2}",
    );

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Hot
    )]);
}

/// Phase 8-C pin-contract — pinned shards skip the idle-demote
/// policy evaluation even when their `last_query_at_ms` is well
/// past every TTL threshold.
///
/// Mirrors the controller-test pattern from
/// [`super::idle_demote::demote_idle_shards_warm_to_parked_at_ttl`]:
/// backdate the shard's idle clock, run `demote_idle_shards`, and
/// assert the tier did **not** change.
#[tokio::test]
async fn preload_pin_blocks_idle_demote() {
    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );

    let outcome = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::C, 30)
        .await;
    assert!(matches!(outcome, PreloadOutcome::Promoted { .. }));

    // Backdate the idle clock so the policy would otherwise demote.
    let last_query_ms = 1_000_000_000_u64;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, last_query_ms)
            .await
    );

    // Run the controller with `now_ms` well past every TTL.
    let now_ms = last_query_ms + (WARM_TO_PARKED_IDLE_SECS + 1) * 1000;
    mgr.demote_idle_shards(now_ms).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Hot)],
        "pinned shard must stay Hot even when the idle clock is past the TTL"
    );
}

/// Phase 8-B + 8-C — explicit `hibernate` overrides a pin.  The
/// registry rebuild installs a fresh `ShardEntry` whose
/// `pin_until_ms` starts at `0`, so the pin is implicitly cleared
/// without a separate `clear_pin` call.
#[tokio::test]
async fn hibernate_overrides_preload_pin() {
    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );

    // Pin C in Hot for 30 min.
    assert!(matches!(
        mgr.preload_drive(uffs_mft::platform::DriveLetter::C, 30)
            .await,
        PreloadOutcome::Promoted { .. }
    ));

    // Hibernate must demote C to Cold despite the pin.
    let outcome = mgr.hibernate_shards(&[]).await;
    assert_eq!(
        outcome.hot_demoted,
        vec![uffs_mft::platform::DriveLetter::C],
        "hibernate must report C as hot-demoted (pre-call tier)"
    );

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Cold)],
        "hibernate must override the pin and demote to Cold"
    );
}

/// Phase 8-C — preloading an unknown drive returns
/// [`PreloadOutcome::UnknownDrive`] without mutating the registry.
#[tokio::test]
async fn preload_unknown_drive_returns_unknown() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    let outcome = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::Z, 30)
        .await;
    assert!(
        matches!(outcome, PreloadOutcome::UnknownDrive),
        "unknown drive Z must produce UnknownDrive; got {outcome:?}"
    );

    // Loaded shard untouched.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Warm
    )]);
}

/// Phase 8-C — when the body loader returns `None` for the
/// requested drive, preload reports
/// [`PreloadOutcome::LoadFailed`] and the shard stays in its
/// pre-call tier (no half-promoted state).
#[tokio::test]
async fn preload_load_failure_keeps_pre_call_state() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(MissingBodyLoader));
    mgr.add_drive(build_test_drive()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );

    let outcome = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::C, 30)
        .await;
    assert!(
        matches!(outcome, PreloadOutcome::LoadFailed),
        "MissingBodyLoader must surface as LoadFailed; got {outcome:?}"
    );

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Cold)],
        "shard must stay in pre-call Cold after load failure"
    );
}

/// Phase 8-C — preloading a `Warm` shard skips the body-load step
/// (the body is already in memory) and rebuilds the registry with
/// the existing body promoted to `Hot`.  Verifies that
/// [`crate::index::IndexManager::preload_drive`]'s Warm-source
/// branch is exercised by a fixture that does **not** drive the
/// body loader at all.
#[tokio::test]
async fn preload_warm_drive_skips_body_load() {
    use crate::cache::body_loader::BodyLoader as BodyLoaderTrait;

    /// A body loader that panics if called — proves the Warm-source
    /// branch never reaches the loader.
    struct PanicOnLoad;

    impl BodyLoaderTrait for PanicOnLoad {
        fn load(
            &self,
            letter: uffs_mft::platform::DriveLetter,
        ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
            panic!(
                "PanicOnLoad::load — unexpected call for {letter}; Warm-source preload must reuse the live body"
            )
        }
    }

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanicOnLoad));
    mgr.add_drive(build_test_drive()).await; // C lands in Warm.

    let outcome = mgr
        .preload_drive(uffs_mft::platform::DriveLetter::C, 30)
        .await;
    let PreloadOutcome::Promoted { from_state, .. } = outcome else {
        panic!("expected Promoted; got {outcome:?}");
    };
    assert_eq!(from_state, ShardState::Warm);

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Hot
    )]);
}

// ── Phase 9 — `promotions_total` Cold→Hot counter contract ──────────
//
// Mirrors the `scripts/dev/daemon-readiness.rs::scenario_q` Q3a /
// Q4a / Q5a / Q7a column-reading assertions in unit-test form, so a
// regression in the bump site (or the source-state filter) is
// caught at `cargo nextest` time without needing to run the full
// readiness script against a live daemon.

/// Phase 9 — the counter goes 0 → 1 → 2 across two `Cold → Hot`
/// cycles, and the `AlreadyHot` path between them does **not** bump.
///
/// This is the canonical contract documented on
/// [`crate::cache::shard::DriveStats::promotions_total`] and
/// surfaced via the wire response's `promotions_total` field /
/// the CLI's `PROMOTIONS` column.
#[tokio::test]
async fn preload_cold_to_hot_bumps_promotions_total_per_cycle() {
    // Helper hoisted above the let-bindings to satisfy
    // `clippy::items_after_statements` (items must precede
    // statements within a function body).  Reads the counter via
    // the public `status_drives` RPC surface so the test pins the
    // operator-visible contract, not a private accessor.
    async fn read_counter(mgr: &IndexManager) -> u64 {
        let response = mgr.status_drives().await;
        let [row] = response.drives.as_slice() else {
            panic!("expected exactly 1 drive; got {}", response.drives.len());
        };
        row.promotions_total
    }

    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;

    assert_eq!(
        read_counter(&mgr).await,
        0,
        "fresh `add_drive` must leave promotions_total at 0"
    );

    // ── Cycle 1: Cold → Hot ─────────────────────────────────────
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );
    assert!(matches!(
        mgr.preload_drive(uffs_mft::platform::DriveLetter::C, 5)
            .await,
        PreloadOutcome::Promoted { .. }
    ));
    assert_eq!(
        read_counter(&mgr).await,
        1,
        "Cold→Hot preload must bump promotions_total exactly once"
    );

    // ── AlreadyHot — must NOT bump ──────────────────────────────
    assert!(matches!(
        mgr.preload_drive(uffs_mft::platform::DriveLetter::C, 60)
            .await,
        PreloadOutcome::AlreadyHot { .. }
    ));
    assert_eq!(
        read_counter(&mgr).await,
        1,
        "AlreadyHot preload skips the rebuild and must not bump promotions_total"
    );

    // ── Cycle 2: Hot → Cold (via test escape) → Hot ─────────────
    // `demote_letter_for_test` overrides the pin; same effect as
    // an operator `hibernate` for counter-bump purposes.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .await
    );
    assert!(matches!(
        mgr.preload_drive(uffs_mft::platform::DriveLetter::C, 5)
            .await,
        PreloadOutcome::Promoted { .. }
    ));
    assert_eq!(
        read_counter(&mgr).await,
        2,
        "second Cold→Hot cycle must bump promotions_total to 2 \
         (1 + 1, AlreadyHot in between is a no-op)"
    );
}

/// Phase 9 — the `Warm → Hot` source arm of `promote_letter_to_hot`
/// is a tier-marker flip with no body load and no decrypt cost; it
/// must **not** bump `promotions_total`.
///
/// Reuses the `PanicOnLoad` body loader from
/// `preload_warm_drive_skips_body_load` so any accidental body
/// load (which would also be the wrong implementation) panics
/// before the assertion runs.
#[tokio::test]
async fn preload_warm_to_hot_does_not_bump_promotions_total() {
    use crate::cache::body_loader::BodyLoader as BodyLoaderTrait;

    struct PanicOnLoad;
    impl BodyLoaderTrait for PanicOnLoad {
        fn load(
            &self,
            letter: uffs_mft::platform::DriveLetter,
        ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
            panic!(
                "PanicOnLoad::load — Warm→Hot source arm must not call the loader (letter={letter})"
            )
        }
    }

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanicOnLoad));
    mgr.add_drive(build_test_drive()).await; // C lands in Warm.

    assert!(matches!(
        mgr.preload_drive(uffs_mft::platform::DriveLetter::C, 5)
            .await,
        PreloadOutcome::Promoted {
            from_state: ShardState::Warm,
            ..
        }
    ));

    let response = mgr.status_drives().await;
    let [row] = response.drives.as_slice() else {
        panic!("expected exactly 1 drive; got {}", response.drives.len());
    };
    assert_eq!(
        row.promotions_total, 0,
        "Warm→Hot is a tier-marker flip; promotions_total counts \
         only promotions back from Parked or Cold"
    );
}

/// The `Parked → Hot` source arm of `promote_letter_to_hot` pays the
/// same body re-load from the on-disk cache as `Cold → Hot` (it drops
/// the parked bloom + trie and re-runs the body loader; see
/// `crate::index::tiering_ops::IndexManager::preload_drive` source
/// arms), so it counts in `promotions_total` exactly once.  Until
/// v0.6.46 it did not, and a drive re-warmed from Parked read 0.
#[tokio::test]
async fn preload_parked_to_hot_bumps_promotions_total_once() {
    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;

    // Seed C as Parked (legal demote target from Warm).
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    assert!(matches!(
        mgr.preload_drive(uffs_mft::platform::DriveLetter::C, 5)
            .await,
        PreloadOutcome::Promoted {
            from_state: ShardState::Parked,
            ..
        }
    ));

    let response = mgr.status_drives().await;
    let [row] = response.drives.as_slice() else {
        panic!("expected exactly 1 drive; got {}", response.drives.len());
    };
    assert_eq!(
        row.promotions_total, 1,
        "Parked→Hot re-loads the body from disk; promotions_total must be 1"
    );
}
