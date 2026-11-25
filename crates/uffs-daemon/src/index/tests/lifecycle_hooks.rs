// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 5 lifecycle-hook injection tests + `drives` RPC tier-marker
//! contract.
//!
//! Covers:
//!
//! * Plan task 5.11 — `IndexManager::drives()` enumerates Warm / Parked / Cold
//!   shards with tier markers (the 2026-04-28 dogfood regression).
//! * Plan task 5.8 — `WorkingSetTrim::trim()` invocation contract (once per
//!   batch in `demote_idle_shards`).
//! * Plan task 5.9 — `Prefetch::hint()` invocation with the freshly-loaded
//!   body's records + names regions.
//! * Owner ruling 2026-10-03 — the pressure subscriber only observes: a `Low`
//!   transition demotes nothing and trims nothing.

use alloc::sync::Arc;

use super::{
    FixedBodyLoader, IndexManager, build_test_drive, build_test_drive_d, build_test_drive_e,
};

/// Plan task **5.11**: `IndexManager::drives()` must enumerate every
/// shard in the registry — Warm, Parked, *and* Cold — tagged with its
/// `ShardTier` so the CLI status formatter can render the tier marker
/// instead of printing `Drives: (none loaded)` when the registry holds
/// only demoted shards.
///
/// Surfaced by the 2026-04-28 dogfood: at t=44m the daemon correctly
/// had all 7 drives Parked (their bloom + path-trie still resident,
/// ready for re-promote on bloom hit), but `daemon status` rendered
/// the empty-registry path because the old `drives()` filtered
/// through `active_index()` (Warm/Hot only).  The fix walks the
/// registry directly; this test pins the contract.
///
/// Topology: 3 drives.  C stays Warm.  D demotes to Parked.  E
/// demotes to Cold.  Assertions cover:
/// * every shard is in the response (no filtering),
/// * tiers map 1:1 from `ShardState` → `ShardTier`,
/// * Warm shards carry the body's `records.len()`,
/// * Parked / Cold shards report `records: 0` and a synthetic `source` label,
/// * load-order is preserved (C, D, E).
#[tokio::test]
async fn drives_rpc_enumerates_warm_parked_and_cold_shards_with_tier_markers() {
    use uffs_client::protocol::response::ShardTier;

    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    // Demote D → Parked (body released; bloom + trie resident).
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::D, ShardState::Parked)
            .await
    );
    // Demote E → Cold (no body, no filters).
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::E, ShardState::Cold)
            .await
    );

    let response = mgr.drives().await;
    assert_eq!(
        response.drives.len(),
        3,
        "every loaded shard must appear, including Parked and Cold"
    );

    // Load-order preserved (matches ShardRegistry::iter()).
    let letters: Vec<uffs_mft::platform::DriveLetter> =
        response.drives.iter().map(|dr| dr.letter).collect();
    assert_eq!(
        letters,
        vec![
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D,
            uffs_mft::platform::DriveLetter::E
        ],
        "load order preserved"
    );

    // C — Warm: body present, records nonzero, tier=Warm,
    // source from the body's IndexSource (live MFT path "C:").
    let drive_c = response.drives.first().expect("drives has an element 0");
    assert_eq!(drive_c.letter, uffs_mft::platform::DriveLetter::C);
    assert_eq!(drive_c.tier, Some(ShardTier::Warm), "C remains Warm");
    assert!(
        drive_c.records > 0,
        "Warm shard reports its body's records.len()"
    );
    assert_eq!(
        drive_c.source, "live",
        "Warm shard's body source flows through"
    );

    // D — Parked: no body, records=0, tier=Parked,
    // source synthesized as "parked".
    let drive_d = response.drives.get(1).expect("drives has an element 1");
    assert_eq!(drive_d.letter, uffs_mft::platform::DriveLetter::D);
    assert_eq!(drive_d.tier, Some(ShardTier::Parked), "D demoted to Parked");
    assert_eq!(drive_d.records, 0, "Parked shard has no body in RAM");
    assert_eq!(
        drive_d.source, "parked",
        "Parked shard surfaces a synthetic source label"
    );

    // E — Cold: no body, no filters, records=0, tier=Cold,
    // source synthesized as "cold".
    let drive_e = response.drives.get(2).expect("drives has an element 2");
    assert_eq!(drive_e.letter, uffs_mft::platform::DriveLetter::E);
    assert_eq!(drive_e.tier, Some(ShardTier::Cold), "E demoted to Cold");
    assert_eq!(drive_e.records, 0, "Cold shard has nothing in RAM");
    assert_eq!(
        drive_e.source, "cold",
        "Cold shard surfaces a synthetic source label"
    );
}

/// Counter-test to the enumeration above: empty registry must still
/// render the legacy `(none loaded)` path so cold-boot detection in
/// external scripts (`scripts/windows/api-validation.rs`,
/// `cli-validation.rs`, `mcp-validation.rs`) continues to fire on a
/// truly empty daemon.  Pins that the formatter doesn't accidentally
/// emit a tier-marker line for a registry that holds zero shards.
#[tokio::test]
async fn drives_rpc_returns_empty_vec_when_registry_is_empty() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));

    let response = mgr.drives().await;
    assert!(
        response.drives.is_empty(),
        "no shards loaded → empty drives vec — CLI renders `(none loaded)`"
    );
}

/// Phase 5 task **5.8** — `demote_idle_shards` invokes the
/// `WorkingSetTrim::trim()` hook **exactly once** per applied
/// batch, not once per shard.  Pins the contract documented on
/// the trait: process-level call, coalesced across the batch
/// (Windows `EmptyWorkingSet` is process-wide so per-shard calls
/// would be wasted syscalls).
///
/// Topology: 3 drives all backdated past `WARM_TO_PARKED_IDLE_SECS`
/// so the controller demotes them in a single batch.  Inject a
/// `CountingWorkingSetTrim` fake; assert `calls() == 1` after the
/// tick.
#[tokio::test]
async fn demote_idle_shards_invokes_working_set_trim_once_per_batch() {
    use crate::cache::policy::WARM_TO_PARKED_IDLE_SECS;
    use crate::cache::working_set::tests::CountingWorkingSetTrim;

    let (tx, _rx) = crate::events::event_channel();
    let counting_trim = Arc::new(CountingWorkingSetTrim::new());
    let hooks = crate::index::constructors::LifecycleHooks {
        working_set_trim: Arc::clone(&counting_trim)
            as Arc<dyn crate::cache::working_set::WorkingSetTrim>,
        ..crate::index::constructors::LifecycleHooks::production()
    };
    let mgr = IndexManager::with_lifecycle_hooks_for_test(
        None,
        tx,
        hooks,
        Arc::new(crate::config::Config::default()),
    );
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    // Backdate every shard's last_query_at_ms past the Warm→Parked
    // threshold so the controller picks up all three in one batch.
    let last_query_ms = 1_000_000_000_u64;
    for letter in [
        uffs_mft::platform::DriveLetter::C,
        uffs_mft::platform::DriveLetter::D,
        uffs_mft::platform::DriveLetter::E,
    ] {
        assert!(
            mgr.backdate_last_query_at_ms_for_test(letter, last_query_ms)
                .await
        );
    }

    // Pre-batch: hook never fired.
    assert_eq!(counting_trim.calls(), 0, "no demote yet → no trim");

    let now_ms = last_query_ms + WARM_TO_PARKED_IDLE_SECS * 1000;
    mgr.demote_idle_shards(now_ms).await;

    // Post-batch: every shard demoted, hook fired exactly once.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![
        (
            uffs_mft::platform::DriveLetter::C,
            crate::cache::ShardState::Parked
        ),
        (
            uffs_mft::platform::DriveLetter::D,
            crate::cache::ShardState::Parked
        ),
        (
            uffs_mft::platform::DriveLetter::E,
            crate::cache::ShardState::Parked
        ),
    ]);
    assert_eq!(
        counting_trim.calls(),
        1,
        "WorkingSetTrim::trim() fires once per batch, not per shard"
    );

    // Idempotent on a second tick: nothing to demote → no trim.
    mgr.demote_idle_shards(now_ms).await;
    assert_eq!(
        counting_trim.calls(),
        1,
        "no-op tick must not re-trim — coalescing depends on `applied > 0`",
    );
}

/// Phase 5 task **5.9** — `ensure_warm_for_dispatch` invokes the
/// `Prefetch::hint()` hook with the freshly-loaded body's
/// records + names regions, in that order, before the registry
/// write-lock swap.  Pins the contract that the kernel-prefetch
/// runs while the orchestrator is still in the blocking task so
/// the syscall overlaps with the lock acquisition.
///
/// Topology: 1 drive (C), demoted to Parked.  Inject a
/// `FixedBodyLoader` so the body Arc handed to `Prefetch::hint`
/// is byte-identical to the one we constructed pre-test;
/// `RecordingPrefetch` captures every region as `(ptr-as-usize,
/// len)` so the assertion can match on the body's
/// `records.as_ptr()` and `names.as_ptr()` directly.
#[tokio::test]
async fn ensure_warm_for_dispatch_invokes_prefetch_with_records_and_names_regions() {
    use crate::cache::ShardState;
    use crate::cache::prefetch::tests::RecordingPrefetch;

    let (tx, _rx) = crate::events::event_channel();

    // Build the fixed body up front so we can compare regions
    // against it after promote.
    let body = Arc::new(build_test_drive());
    let recording_prefetch = Arc::new(RecordingPrefetch::new());
    let hooks = crate::index::constructors::LifecycleHooks {
        body_loader: Arc::new(FixedBodyLoader {
            body: Arc::clone(&body),
        }),
        prefetch: Arc::clone(&recording_prefetch) as Arc<dyn crate::cache::prefetch::Prefetch>,
        ..crate::index::constructors::LifecycleHooks::production()
    };
    let mgr = IndexManager::with_lifecycle_hooks_for_test(
        None,
        tx,
        hooks,
        Arc::new(crate::config::Config::default()),
    );
    mgr.add_drive(build_test_drive()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // Pre-promote: no prefetch calls.
    assert_eq!(
        recording_prefetch.calls(),
        Vec::<Vec<(usize, usize)>>::new()
    );

    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;

    // Shard promoted (the Phase-3 contract this test depends on).
    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Warm
    )]);

    // Prefetch invoked exactly once, with two regions in a fixed
    // order: records first (typed slice → byte length), names
    // second (raw `u8` slice → length is element count == bytes).
    let calls = recording_prefetch.calls();
    assert_eq!(
        calls.len(),
        1,
        "exactly one Prefetch::hint() call per promoted shard"
    );
    let regions = calls.first().expect("calls has an element 0");
    assert_eq!(
        regions.len(),
        2,
        "regions: [records, names] — fixed order, no extras"
    );

    let expected_records_ptr = body.records.as_slice().as_ptr() as usize;
    let expected_records_len = size_of_val(body.records.as_slice());
    let expected_names_ptr = body.names.as_slice().as_ptr() as usize;
    let expected_names_len = body.names.as_slice().len();

    assert_eq!(
        *regions.first().expect("regions has an element 0"),
        (expected_records_ptr, expected_records_len),
        "records region matches the body's records.as_slice()",
    );
    assert_eq!(
        *regions.get(1).expect("regions has an element 1"),
        (expected_names_ptr, expected_names_len),
        "names region matches the body's names.as_slice()",
    );
}

/// Owner ruling 2026-10-03 — the pressure subscriber observes, it does
/// not act.  A kernel `Low` transition must leave every shard where it
/// is and never call [`WorkingSetTrim::trim`]: a shard goes Parked →
/// Hot when a search needs it and only the idle TTL ladder retires it.
/// Pins the contract so the forced cascade cannot creep back.
///
/// Drives the timeline through the watch channel against the real
/// [`crate::spawn_pressure_subscriber`] with a `ControllablePressureSignal`
/// fake; a 50 ms quiescent window after each transition is far longer
/// than the subscriber's single `borrow_and_update` + log.
///
/// [`WorkingSetTrim::trim`]: crate::cache::working_set::WorkingSetTrim::trim
#[tokio::test]
async fn pressure_subscriber_observes_transitions_without_demoting() {
    use core::time::Duration;

    use crate::cache::ShardState;
    use crate::cache::pressure::PressureLevel;
    use crate::cache::pressure::tests::ControllablePressureSignal;
    use crate::cache::working_set::tests::CountingWorkingSetTrim;
    use crate::index::constructors::LifecycleHooks;

    let (tx, _rx) = crate::events::event_channel();
    let counting_trim = Arc::new(CountingWorkingSetTrim::new());
    let pressure_fake = Arc::new(ControllablePressureSignal::new());
    let hooks = LifecycleHooks {
        working_set_trim: Arc::clone(&counting_trim)
            as Arc<dyn crate::cache::working_set::WorkingSetTrim>,
        pressure: Arc::clone(&pressure_fake) as Arc<dyn crate::cache::pressure::PressureSignal>,
        ..LifecycleHooks::production()
    };
    let mgr = Arc::new(IndexManager::with_lifecycle_hooks_for_test(
        None,
        tx,
        hooks,
        Arc::new(crate::config::Config::default()),
    ));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    let subscriber = crate::spawn_pressure_subscriber(&mgr);
    let attach_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while pressure_fake.receiver_count() == 0 {
        assert!(
            std::time::Instant::now() < attach_deadline,
            "subscriber did not attach to the watch channel within 2 s",
        );
        tokio::task::yield_now().await;
    }

    for level in [PressureLevel::Low, PressureLevel::High, PressureLevel::Low] {
        assert!(
            pressure_fake.set(level),
            "broadcast {level:?} must reach the attached subscriber",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let states = mgr.shard_states_for_test().await;
        assert!(
            states.iter().all(|(_, state)| *state == ShardState::Warm),
            "{level:?} must not move any shard; got {states:?}",
        );
        assert_eq!(
            counting_trim.calls(),
            0,
            "{level:?} must not trim the working set",
        );
    }

    subscriber.abort();
}
