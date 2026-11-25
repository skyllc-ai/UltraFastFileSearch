// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 3 Commit D — `IndexManager::demote_idle_shards` state-ladder tests.
//!
//! Covers:
//!
//! * `mark_loaded_at` seeding the freshly-mounted shard's idle clock.
//! * `demote_idle_shards` no-op / Warm→Parked / Parked→Cold / batch-multiple /
//!   round-trip query stats.
//! * Plan tasks 3.7 / 3.8 — virtual-time multi-drive demote tests.
//!
//! `shard.transition` tracing-event contract tests (Phase 3 Commit E +
//! Phase 5 G4 + PR-f promote-thrash regression) live in the sibling
//! [`super::idle_demote_tracing`] module; the shared `EventLog` /
//! `CapturedEvent` capture scaffold is in [`super::tracing_capture`].

use alloc::sync::Arc;

use super::{IndexManager, build_test_drive, build_test_drive_d, build_test_drive_e};

// ── Phase 3 Commit D — IndexManager::demote_idle_shards ────────────

/// `add_drive` calls `DriveStats::mark_loaded_at(now_ms)` on the
/// freshly mounted shard so the demote-controller's idle clock
/// starts ticking from load time, not from epoch zero.  Without
/// this seed, every freshly loaded shard would demote on the
/// first idle tick because `last_query_at_ms == 0` would compute
/// `idle_secs ≈ now_ms / 1000` (≈ billions of seconds since
/// 1970-01-01).
#[tokio::test]
async fn mark_loaded_at_seeds_freshly_added_drive() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    // Read the shard's last_query_at_ms via a search-ish path; we
    // only have access from inside the manager, so drive a
    // demote-controller call with `now_ms = load_ts + 0` and assert
    // the shard didn't demote — that proves last_query_at_ms is
    // recent (within `WARM_TO_PARKED_IDLE_SECS` of now).
    //
    // More directly: the timestamp must be non-zero.  We test that
    // by attempting to demote at a now that's *exactly* the load
    // time + 1 ms; an unseeded shard would have `idle_ms = now`
    // (huge) and demote, but a seeded shard sees `idle_ms = 1` ms
    // (basically 0 s) and stays Warm.
    let states_before = mgr.shard_states_for_test().await;
    assert_eq!(states_before, vec![(
        uffs_mft::platform::DriveLetter::C,
        crate::cache::ShardState::Warm
    )]);

    // Synthetic now_ms a billion ms in the future would catch
    // unseeded `last_query_at_ms == 0`.  But a seeded shard has
    // last_query_at_ms ≈ unix_now_ms() (when add_drive was just
    // called), so calling demote_idle_shards with the same now
    // gives idle_secs ≈ 0.
    let now_ms = crate::cache::unix_now_ms();
    mgr.demote_idle_shards(now_ms).await;

    let states_after = mgr.shard_states_for_test().await;
    assert_eq!(
        states_after, states_before,
        "freshly loaded shard must NOT demote on the first tick — \
         mark_loaded_at must have seeded last_query_at_ms",
    );
}

/// Fast-path contract: `demote_idle_shards` on a registry where
/// every shard has been queried recently must complete with no
/// state mutation and no `index_version` bump.
#[tokio::test]
async fn demote_idle_shards_no_op_when_all_fresh() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    // Pretend every shard was queried at t=10_000_000_000 ms.
    let load_ts = 10_000_000_000_u64;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, load_ts)
            .await
    );
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::D, load_ts)
            .await
    );

    // `now_ms` only 1 ms after load → idle_secs = 0 → no demote.
    mgr.demote_idle_shards(load_ts + 1).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![
        (uffs_mft::platform::DriveLetter::C, ShardState::Warm),
        (uffs_mft::platform::DriveLetter::D, ShardState::Warm)
    ]);
}

/// Warm shard idle past `WARM_TO_PARKED_IDLE_SECS` demotes to
/// Parked on the next `demote_idle_shards` call.
#[tokio::test]
async fn demote_idle_shards_warm_to_parked_at_ttl() {
    use crate::cache::policy::WARM_TO_PARKED_IDLE_SECS;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    // Backdate C's last_query_at_ms to t=1_000_000_000 ms.
    let last_query_ms = 1_000_000_000_u64;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, last_query_ms)
            .await
    );

    // now_ms = last_query + WARM_TO_PARKED_IDLE_SECS * 1000 (exact
    // boundary; `next_state_for_idle` uses `>=`).
    let now_ms = last_query_ms + WARM_TO_PARKED_IDLE_SECS * 1000;
    mgr.demote_idle_shards(now_ms).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        crate::cache::ShardState::Parked
    )]);
}

/// Warm shard idle just below `WARM_TO_PARKED_IDLE_SECS` stays
/// Warm — pin the off-by-one that `>=` vs `>` would expose.
#[tokio::test]
async fn demote_idle_shards_below_ttl_keeps_warm() {
    use crate::cache::policy::WARM_TO_PARKED_IDLE_SECS;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    let last_query_ms = 1_000_000_000_u64;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, last_query_ms)
            .await
    );

    // 1 ms before the boundary — idle_secs computed by
    // `(now - last) / 1000` is `WARM_TO_PARKED_IDLE_SECS - 1`,
    // strictly below the threshold.
    let now_ms = last_query_ms + WARM_TO_PARKED_IDLE_SECS * 1000 - 1;
    mgr.demote_idle_shards(now_ms).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        crate::cache::ShardState::Warm
    )]);
}

/// Parked shard idle past `PARKED_TO_COLD_IDLE_SECS` demotes to
/// Cold on the next `demote_idle_shards` call.
///
/// Pins the multi-step ladder: a single tick can see both
/// `Warm → Parked` and `Parked → Cold` transitions if a Parked
/// shard's `last_query_at_ms` is old enough, but the policy only
/// returns one demote target per call so each tick advances each
/// shard at most one tier.  This test seeds a Parked shard
/// directly to keep the assertion focused.
#[tokio::test]
async fn demote_idle_shards_parked_to_cold_at_ttl() {
    use crate::cache::ShardState;
    use crate::cache::policy::PARKED_TO_COLD_IDLE_SECS;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    // Seed C as Parked via the test escape hatch.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // Backdate so the Parked shard has been idle past its TTL.
    let last_query_ms = 1_000_000_000_u64;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, last_query_ms)
            .await
    );

    let now_ms = last_query_ms + PARKED_TO_COLD_IDLE_SECS * 1000;
    mgr.demote_idle_shards(now_ms).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Cold
    )]);
}

/// `demote_idle_shards` batches multiple demotes inside a single
/// write-lock window.  Pin the contract by demoting three shards
/// in one call.
#[tokio::test]
async fn demote_idle_shards_batches_multiple_demotes() {
    use crate::cache::ShardState;
    use crate::cache::policy::WARM_TO_PARKED_IDLE_SECS;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    let last_query_ms = 1_000_000_000_u64;
    for letter in [
        uffs_mft::platform::DriveLetter::C,
        uffs_mft::platform::DriveLetter::D,
    ] {
        assert!(
            mgr.backdate_last_query_at_ms_for_test(letter, last_query_ms)
                .await
        );
    }

    let now_ms = last_query_ms + WARM_TO_PARKED_IDLE_SECS * 1000;
    let version_before = mgr
        .index_version
        .load(core::sync::atomic::Ordering::Relaxed);
    mgr.demote_idle_shards(now_ms).await;
    let version_after = mgr
        .index_version
        .load(core::sync::atomic::Ordering::Relaxed);

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![
            (uffs_mft::platform::DriveLetter::C, ShardState::Parked),
            (uffs_mft::platform::DriveLetter::D, ShardState::Parked)
        ],
        "all backdated Warm shards must demote in a single batch call"
    );
    assert_eq!(
        version_after - version_before,
        1,
        "batch must bump index_version exactly once for the whole batch, \
         not once per demoted shard"
    );
}

/// End-to-end: demote → promote → demote → promote preserves
/// query stats across every rebuild.  Pins the
/// `Arc<DriveStats>`-sharing contract under repeated transitions.
#[test]
fn demote_then_promote_round_trips_query_stats() {
    use crate::cache::{ShardRegistry, ShardState};

    let body_c = Arc::new(build_test_drive());
    let mut reg = ShardRegistry::new().add(Arc::clone(&body_c));

    // Each transition adds queries to verify the canonical stats
    // Arc is what the new shard's `.stats` points at.
    for round in 0_u64..3_u64 {
        reg.iter()
            .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
            .unwrap()
            .stats
            .mark_query_at(1_000 + round);
        reg = reg
            .demote_letter(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .expect("demote");
        reg.iter()
            .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
            .unwrap()
            .stats
            .mark_query_at(2_000 + round);
        reg = reg
            .promote_letter(uffs_mft::platform::DriveLetter::C, Arc::clone(&body_c))
            .expect("promote");
    }

    let final_c = reg
        .iter()
        .find(|shard| shard.drive == uffs_mft::platform::DriveLetter::C)
        .unwrap();
    // 6 mark_query_at calls total across 3 rounds (3 pre-demote +
    // 3 post-demote-pre-promote).
    assert_eq!(final_c.stats.queries_total(), 6);
    // Last mark_query_at was during round 2 with `now_ms = 2_002`.
    assert_eq!(final_c.stats.last_query_at_ms(), 2_002);
}

// ── Phase 3 Commit E — virtual-time multi-drive demote tests ───────

/// Plan task 3.7 — three drives loaded; only C is queried; advance
/// past `WARM_TO_PARKED_IDLE_SECS` and verify D + E demote to Parked
/// while C stays Warm.
///
/// Models the steady-state pattern of a developer using their
/// project drive (C) actively while archive drives (D, E) sit idle.
/// Pins the per-shard idle-clock contract: each shard's
/// `last_query_at_ms` is independent, so the demote controller
/// only acts on the ones that have actually been idle.
///
/// `now_ms` threading lets the test simulate "31 minutes later"
/// deterministically — no `tokio::time::pause` needed because
/// `demote_idle_shards(now_ms)` reads the timestamp from its
/// argument, not from a clock.
#[tokio::test]
async fn demote_idle_shards_warm_only_for_unqueried_drives() {
    use crate::cache::ShardState;
    use crate::cache::policy::WARM_TO_PARKED_IDLE_SECS;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    let load_ts = 1_000_000_000_u64;
    // Seed all three to the load timestamp.
    for letter in [
        uffs_mft::platform::DriveLetter::C,
        uffs_mft::platform::DriveLetter::D,
        uffs_mft::platform::DriveLetter::E,
    ] {
        assert!(
            mgr.backdate_last_query_at_ms_for_test(letter, load_ts)
                .await
        );
    }

    // C is queried 30 minutes after load (last query at
    // load_ts + 30min).  D and E remain at load_ts.
    let c_last_query_ms = load_ts + 30 * 60 * 1000;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, c_last_query_ms)
            .await
    );

    // now_ms = load_ts + 31 minutes.
    let now_ms = load_ts + 31 * 60 * 1000;

    // Sanity: 31 min ≥ WARM_TO_PARKED_IDLE_SECS for D, E.
    let d_e_idle_secs = (now_ms - load_ts) / 1000;
    assert!(d_e_idle_secs >= WARM_TO_PARKED_IDLE_SECS);
    // Sanity: 1 min < WARM_TO_PARKED_IDLE_SECS for C.
    let c_idle_secs = (now_ms - c_last_query_ms) / 1000;
    assert!(c_idle_secs < WARM_TO_PARKED_IDLE_SECS);

    mgr.demote_idle_shards(now_ms).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![
            (uffs_mft::platform::DriveLetter::C, ShardState::Warm),
            (uffs_mft::platform::DriveLetter::D, ShardState::Parked),
            (uffs_mft::platform::DriveLetter::E, ShardState::Parked),
        ],
        "C must stay Warm (recently queried); D and E must demote to Parked"
    );
}

/// Plan task 3.8 — three Parked drives, advance past
/// `PARKED_TO_COLD_IDLE_SECS`, verify all three demote to Cold.
///
/// Pins the bottom rung of the static-TTL ladder.  The Parked tier
/// is the first that drops bloom + trie (Phase 4+); for Phase 3
/// "Parked" already means "no body", so the only difference is the
/// state label.  Cold means "needs a full re-decrypt to re-promote",
/// captured by the policy via the longer 24 h threshold.
#[tokio::test]
async fn demote_idle_shards_parked_drives_demote_to_cold_past_threshold() {
    use crate::cache::ShardState;
    use crate::cache::policy::PARKED_TO_COLD_IDLE_SECS;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    // Seed every drive's last_query to load_ts and demote each to
    // Parked via the test escape hatch.  Order: backdate first so
    // the demote controller doesn't trip on the seeding tick.
    let load_ts = 1_000_000_000_u64;
    for letter in [
        uffs_mft::platform::DriveLetter::C,
        uffs_mft::platform::DriveLetter::D,
        uffs_mft::platform::DriveLetter::E,
    ] {
        assert!(
            mgr.backdate_last_query_at_ms_for_test(letter, load_ts)
                .await
        );
        assert!(mgr.demote_letter_for_test(letter, ShardState::Parked).await);
    }

    let pre_states = mgr.shard_states_for_test().await;
    assert_eq!(pre_states, vec![
        (uffs_mft::platform::DriveLetter::C, ShardState::Parked),
        (uffs_mft::platform::DriveLetter::D, ShardState::Parked),
        (uffs_mft::platform::DriveLetter::E, ShardState::Parked),
    ],);

    // now_ms = load_ts + 25 hours (≥ PARKED_TO_COLD_IDLE_SECS = 24h).
    let now_ms = load_ts + 25 * 60 * 60 * 1000;
    let idle_secs = (now_ms - load_ts) / 1000;
    assert!(idle_secs >= PARKED_TO_COLD_IDLE_SECS);

    mgr.demote_idle_shards(now_ms).await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![
            (uffs_mft::platform::DriveLetter::C, ShardState::Cold),
            (uffs_mft::platform::DriveLetter::D, ShardState::Cold),
            (uffs_mft::platform::DriveLetter::E, ShardState::Cold),
        ],
        "all three Parked shards past the cold-tier TTL must demote to Cold"
    );
}
