// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `ShardRegistry` + `ShardEntry` integration tests.
//!
//! Covers:
//!
//! * Phase 1 add/replace/remove round-trip.
//! * `ShardEntry::try_transition` legal/illegal graph (Task 1.7).
//! * Two-drive search dispatch under the registry (Task 1.9).
//! * `search_records_query_on_every_active_shard` (Task 1.5).
//! * Phase 3 Commit B `demote_letter` / `promote_letter` unit contracts.

#![expect(
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    clippy::std_instead_of_alloc,
    reason = "test code — assertions index into known-shape vectors, use short \
              drive-letter idents, and pull `Arc` from `std` to match the rest \
              of the daemon's test fixtures"
)]

use std::sync::Arc;

use super::{IndexManager, build_test_drive, build_test_drive_d};

// ── Phase 1 — ShardRegistry / ShardEntry integration ────────────────

/// `ShardRegistry::{add, replace, remove}` round-trip with real
/// `DriveCompactIndex` bodies.  Pins the case-insensitive contract on
/// `replace` / `remove` that mirrors the pre-Phase-1
/// `IndexManager::replace_drive` filter.
#[test]
fn shard_registry_add_replace_remove_round_trip() {
    use crate::cache::ShardRegistry;

    let body_c = Arc::new(build_test_drive());
    let body_d = Arc::new(build_test_drive_d());
    let body_c_v2 = Arc::new(build_test_drive());

    // Empty start → add C → add D → replace 'c' (case-insensitive) →
    // remove 'd' (case-insensitive).  Single mutable binding so we
    // don't trip clippy::shadow_reuse on each rebuild.
    let mut reg = ShardRegistry::new();
    assert!(reg.is_empty());
    assert_eq!(reg.active_index().drives.len(), 0);

    reg = reg.add(Arc::clone(&body_c));
    reg = reg.add(Arc::clone(&body_d));
    assert_eq!(reg.active_index().drives.len(), 2);
    assert!(reg.contains(uffs_mft::platform::DriveLetter::C));
    assert!(reg.contains(uffs_mft::platform::DriveLetter::D));

    reg = reg.replace(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c_v2));
    assert_eq!(
        reg.active_index().drives.len(),
        2,
        "replace must not duplicate",
    );

    reg = reg.remove(uffs_mft::platform::DriveLetter::D);
    assert!(!reg.contains(uffs_mft::platform::DriveLetter::D));
    assert_eq!(reg.active_index().drives.len(), 1);
    assert_eq!(reg.loaded_letters(), vec![
        uffs_mft::platform::DriveLetter::C
    ]);
}

/// `ShardEntry::try_transition` enforces the legal-transition graph
/// from [`ShardState::can_transition_to`] using a CAS loop.
///
/// Task 1.7 — covers both legal and illegal moves on a real shard
/// with a `DriveCompactIndex` body, complementing the proptest in
/// `crate::cache::shard::tests` which exercises the pure state graph.
#[test]
fn shard_entry_try_transition_legal_and_illegal() {
    use crate::cache::ShardState;
    use crate::cache::shard::ShardEntry;

    let body = Arc::new(build_test_drive());
    let shard = ShardEntry::new_warm(uffs_mft::platform::DriveLetter::C, Arc::clone(&body));
    assert_eq!(shard.state(), ShardState::Warm);

    // Legal: Warm → Hot.
    let prev = shard
        .try_transition(ShardState::Hot)
        .expect("warm->hot is legal");
    assert_eq!(prev, ShardState::Warm);
    assert_eq!(shard.state(), ShardState::Hot);

    // Illegal: Hot → Cold (must go via Evicting → Cold/Parked).
    let err = shard
        .try_transition(ShardState::Cold)
        .expect_err("hot->cold is illegal");
    assert_eq!(err.from, ShardState::Hot);
    assert_eq!(err.to, ShardState::Cold);
    assert_eq!(
        shard.state(),
        ShardState::Hot,
        "state must be unchanged on illegal transition"
    );

    // Recovery path: Hot → Warm → Evicting → Cold all legal.
    shard
        .try_transition(ShardState::Warm)
        .expect("hot->warm legal");
    shard
        .try_transition(ShardState::Evicting)
        .expect("warm->evicting legal");
    shard
        .try_transition(ShardState::Cold)
        .expect("evicting->cold legal");
    assert_eq!(shard.state(), ShardState::Cold);
}

/// Two-drive integration: searches dispatch correctly across both
/// shards under the new `ShardRegistry` indirection.
///
/// Task 1.9 — `build_test_drive` + `build_test_drive_d` with
/// `IndexManager::search`, asserting the results carry rows from both
/// drives.  Pins the "zero observable change" contract for Phase 1.
#[tokio::test]
async fn shard_registry_search_two_drives_returns_rows_from_each() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    let params = uffs_client::protocol::SearchParams {
        pattern: "*".to_owned(),
        limit: Some(50),
        ..Default::default()
    };
    let resp = mgr.search(&params).await.expect("search completes");
    assert!(
        resp.total_count >= 2,
        "two-drive '*' search must return at least 2 rows; got {}",
        resp.total_count,
    );

    // Both drives must contribute records to the snapshot.
    let snap = mgr.snapshot().await;
    assert_eq!(snap.drives.len(), 2);
    let letters: std::collections::HashSet<uffs_mft::platform::DriveLetter> =
        snap.drives.iter().map(|d| d.letter).collect();
    assert!(
        letters.contains(&uffs_mft::platform::DriveLetter::C),
        "C drive must be in the snapshot"
    );
    assert!(
        letters.contains(&uffs_mft::platform::DriveLetter::D),
        "D drive must be in the snapshot"
    );
}

/// `IndexManager::search` records one query per dispatch on every
/// active shard, via `record_search_dispatch` + `DriveStats::record_query`.
///
/// Task 1.5 — pins the wiring between the search hot path and the
/// per-shard counter that Phase 6 reads for adaptive-TTL.
#[tokio::test]
async fn search_records_query_on_every_active_shard() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    // Baseline: no searches yet.
    let before = mgr.shard_query_totals_for_test().await;
    assert_eq!(before.len(), 2);
    for (letter, count) in &before {
        assert_eq!(*count, 0, "drive {letter} must start at 0 queries");
    }

    let params = uffs_client::protocol::SearchParams {
        pattern: "*".to_owned(),
        limit: Some(10),
        ..Default::default()
    };

    // Three searches.  Suffix the loop bound to avoid the implicit i32
    // fallback flagged by clippy::default_numeric_fallback.
    for _ in 0_u32..3_u32 {
        drop(mgr.search(&params).await.expect("search completes"));
    }

    let after = mgr.shard_query_totals_for_test().await;
    assert_eq!(after.len(), 2);
    for (letter, count) in after {
        assert_eq!(
            count, 3,
            "drive {letter} must have recorded 3 queries; got {count}",
        );
    }
}

// ── Phase 3 Commit B — ShardRegistry demote_letter / promote_letter ────

/// Demote a `Warm` shard to `Parked`: the new shard has no body,
/// the active index drops the drive, and the per-drive
/// `Arc<DriveStats>` is shared so query counters survive the
/// rebuild.
#[test]
fn demote_letter_warm_to_parked_drops_body_and_preserves_stats() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let body_d = Arc::new(build_test_drive_d());
    // Single mutable binding so we don't trip clippy::shadow_reuse
    // on each rebuild — same pattern as
    // `shard_registry_add_replace_remove_round_trip`.
    let mut reg = ShardRegistry::new()
        .add(Arc::clone(&body_c))
        .add(Arc::clone(&body_d));
    assert_eq!(reg.active_index().drives.len(), 2);

    // Mark some queries on C so we can verify they survive the
    // rebuild.
    let c_shard_pre = reg
        .iter()
        .find(|s| s.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C present pre-demote");
    for _ in 0_u32..5_u32 {
        c_shard_pre.stats.record_query();
    }
    assert_eq!(c_shard_pre.stats.queries_total(), 5);

    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
        .expect("warm → parked is legal");

    // Active index now only contains D.
    assert_eq!(reg.active_index().drives.len(), 1);
    assert_eq!(
        reg.active_index().drives[0].letter,
        uffs_mft::platform::DriveLetter::D
    );

    // Both shards are still loaded; C is Parked, body lifted.
    let c_shard = reg
        .iter()
        .find(|s| s.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C still loaded post-demote");
    assert_eq!(c_shard.state(), ShardState::Parked);
    assert!(c_shard.body().is_none());

    // Query counter survives via the shared Arc<DriveStats>.
    assert_eq!(
        c_shard.stats.queries_total(),
        5,
        "demote rebuild must preserve query stats via shared Arc<DriveStats>",
    );
}

/// Demote a `Warm` shard directly to `Cold` (skipping `Parked`).
#[test]
fn demote_letter_warm_to_cold_drops_body() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(body_c);
    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
        .expect("warm → cold is legal");

    assert_eq!(reg.active_index().drives.len(), 0);
    let c_shard = reg
        .iter()
        .find(|s| s.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C still loaded");
    assert_eq!(c_shard.state(), ShardState::Cold);
    assert!(c_shard.body().is_none());
}

/// Demoting an unknown letter is a `None` no-op.
#[test]
fn demote_letter_unknown_letter_returns_none() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let reg = ShardRegistry::new().add(body_c);
    assert!(
        reg.demote_letter(uffs_mft::platform::DriveLetter::Z, ShardState::Parked)
            .is_none(),
        "demote on unknown letter must return None"
    );
}

/// Demote target outside the legal demote set (e.g. `Warm`,
/// `Hot`, `Unknown`) returns `None`.
#[test]
fn demote_letter_illegal_target_returns_none() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let reg = ShardRegistry::new().add(body_c);
    for bad_target in [
        ShardState::Warm,
        ShardState::Hot,
        ShardState::Unknown,
        ShardState::Evicting,
    ] {
        assert!(
            reg.demote_letter(uffs_mft::platform::DriveLetter::C, bad_target)
                .is_none(),
            "demote target {bad_target} must be rejected"
        );
    }
}

/// Self-demote (`Parked → Parked`, `Cold → Cold`) is rejected so a
/// buggy controller can't rebuild the registry on every idle tick
/// for an already-demoted shard.
#[test]
fn demote_letter_self_demote_returns_none() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(body_c);
    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
        .expect("first demote");
    assert!(
        reg.demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .is_none(),
        "Parked → Parked must be rejected"
    );

    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
        .expect("parked → cold");
    assert!(
        reg.demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .is_none(),
        "Cold → Cold must be rejected"
    );
}

/// Promote a `Parked` shard back to `Warm`: body restored, active
/// index re-includes the letter, query stats preserved.
#[test]
fn promote_letter_parked_to_warm_restores_body_and_preserves_stats() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));
    // Bump a few queries before demote so we have something to
    // verify across the round trip.
    let pre = reg
        .iter()
        .find(|s| s.drive == uffs_mft::platform::DriveLetter::C)
        .unwrap();
    for _ in 0_u32..3_u32 {
        pre.stats.record_query();
    }
    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
        .expect("demote");

    // Promote with a fresh body (Phase 4+ will fault the original
    // back from disk; for this test we just hand it the same Arc).
    reg = reg
        .promote_letter(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
        .expect("promote");

    assert_eq!(reg.active_index().drives.len(), 1);
    let c = reg
        .iter()
        .find(|s| s.drive == uffs_mft::platform::DriveLetter::C)
        .unwrap();
    assert_eq!(c.state(), ShardState::Warm);
    assert!(c.body().is_some());
    assert_eq!(
        c.stats.queries_total(),
        3,
        "round-trip demote+promote must preserve query stats",
    );
}

/// Promoting an unknown letter is a `None` no-op.
#[test]
fn promote_letter_unknown_letter_returns_none() {
    use crate::cache::ShardRegistry;

    let body_c = Arc::new(build_test_drive());
    let body_d = Arc::new(build_test_drive_d());
    let reg = ShardRegistry::new().add(body_c);
    assert!(
        reg.promote_letter(uffs_mft::platform::DriveLetter::Z, body_d)
            .is_none(),
        "promote on unknown letter must return None"
    );
}

/// Promoting an already-`Warm` shard is a caller bug — `None`.
#[test]
fn promote_letter_already_warm_returns_none() {
    use crate::cache::ShardRegistry;

    let body_c = Arc::new(build_test_drive());
    let reg = ShardRegistry::new().add(Arc::clone(&body_c));
    assert!(
        reg.promote_letter(uffs_mft::platform::DriveLetter::C, body_c)
            .is_none(),
        "promote on already-Warm shard must return None"
    );
}

// ── Promotion counter: every promote back from Parked or Cold counts ──

/// `promote_letter_to_hot` from a `Cold` source bumps the per-shard
/// `promotions_total` counter by 1 — the canonical
/// `uffs --daemon preload <drive>`-against-evicted-drive path that
/// Phase 9 wires through.
#[test]
fn promote_letter_to_hot_bumps_promotions_total_when_source_is_cold() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));
    // Demote C to Cold (the typical state pre-`preload`).
    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
        .expect("demote");
    assert_eq!(
        reg.iter()
            .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
            .expect("C present after Cold demote")
            .stats
            .promotions_total(),
        0,
        "freshly-demoted Cold shard must have promotions_total = 0",
    );

    // Promote Cold → Hot (the actual preload-from-Cold path).
    reg = reg
        .promote_letter_to_hot(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
        .expect("promote_letter_to_hot from Cold");

    let c = reg
        .iter()
        .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C present post-promote");
    assert_eq!(c.state(), ShardState::Hot, "post-promote tier must be Hot");
    assert_eq!(
        c.stats.promotions_total(),
        1,
        "Cold → Hot promote must bump promotions_total by exactly one",
    );
}

/// `promote_letter_to_hot` from a `Warm` source does NOT bump the
/// counter — that's an "already in RAM, just flip the tier marker"
/// path, not an expensive re-read from the on-disk cache.
#[test]
fn promote_letter_to_hot_does_not_bump_promotions_total_when_source_is_warm() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));
    // C lands in Warm (the default after add).
    let pre_state = reg
        .iter()
        .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C present after add")
        .state();
    assert_eq!(pre_state, ShardState::Warm);

    // Promote Warm → Hot (the operator preloads an already-Warm
    // drive — a no-cost tier-marker flip).
    reg = reg
        .promote_letter_to_hot(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
        .expect("promote_letter_to_hot from Warm");

    let c = reg
        .iter()
        .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C present post-promote");
    assert_eq!(c.state(), ShardState::Hot);
    assert_eq!(
        c.stats.promotions_total(),
        0,
        "Warm → Hot promote must NOT bump promotions_total \
         (the body was already in RAM)",
    );
}

/// `promote_letter_to_hot` from a `Parked` source bumps the counter:
/// the body the caller passes in was re-read from the on-disk cache
/// (`load_or_join_in_flight` → `DiskBodyLoader`), the same expensive
/// re-promote a `Cold` source costs.
#[test]
fn promote_letter_to_hot_bumps_promotions_total_when_source_is_parked() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));
    reg = reg
        .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
        .expect("demote to Parked");

    // Promote Parked → Hot.
    reg = reg
        .promote_letter_to_hot(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
        .expect("promote_letter_to_hot from Parked");

    let c = reg
        .iter()
        .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C present post-promote");
    assert_eq!(c.state(), ShardState::Hot);
    assert_eq!(
        c.stats.promotions_total(),
        1,
        "Parked → Hot promote must bump promotions_total by exactly one",
    );
}

/// Field 2026-10-14: a search that re-warmed parked D: left the
/// PROMOTIONS column at 0.  The search path (`promote_letter`,
/// Parked/Cold → Warm) must count like the `preload` path does.
#[test]
fn promote_letter_bumps_promotions_total_from_parked_and_cold() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));
    for (round, source) in [(1_u64, ShardState::Parked), (2_u64, ShardState::Cold)] {
        reg = reg
            .demote_letter(uffs_mft::platform::DriveLetter::C, source)
            .expect("demote");
        reg = reg
            .promote_letter(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
            .expect("promote_letter from a demoted tier");
        let c = reg
            .iter()
            .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
            .expect("C present post-promote");
        assert_eq!(c.state(), ShardState::Warm);
        assert_eq!(
            c.stats.promotions_total(),
            round,
            "after promote from {source}"
        );
    }
}

/// Two consecutive Cold → Hot promotes (e.g. operator runs
/// `preload C` → `hibernate C` → `preload C` again) bump the
/// counter to 2 — the per-drive `Arc<DriveStats>` survives the
/// registry rebuild, so the count accumulates across the
/// shard-rebuild churn.
#[test]
fn promote_letter_to_hot_accumulates_across_repeated_cold_to_hot_cycles() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));

    for _ in 0_u32..2_u32 {
        // Demote to Cold.
        reg = reg
            .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Cold)
            .expect("demote");
        // Promote Cold → Hot.
        reg = reg
            .promote_letter_to_hot(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
            .expect("promote_letter_to_hot");
    }

    let c = reg
        .iter()
        .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
        .expect("C present post-cycle");
    assert_eq!(c.state(), ShardState::Hot);
    assert_eq!(
        c.stats.promotions_total(),
        2,
        "two Cold → Hot cycles must accumulate to promotions_total = 2",
    );
}
