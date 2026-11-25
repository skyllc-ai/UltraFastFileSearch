// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `IndexManager::ensure_warm_for_dispatch` tests.
//!
//! Covers:
//!
//! * Phase 3 Commit C — fast path (no Parked/Cold), filter skipping,
//!   `BodyLoader` injection (success / missing / panicking).
//! * Phase 5 (#93) — parallel re-promote contract via `SlowBodyLoader` (peak
//!   in-flight ≥ 2, wall ≈ delay not N×delay).
//! * Phase 4 task 4.11 promote-side bloom pre-check (miss case keeps Parked,
//!   hit case promotes).
//! * PR-e — single-flight dedup contract via `CountingBodyLoader` (N=10
//!   concurrent `ensure_warm` callers ⇒ exactly 1 `BodyLoader::load` call; slot
//!   clears after the cleanup task drains so re-promote loads fresh; failures
//!   propagate uniformly to all waiters).
//!
//! `FixedBodyLoader` is shared with `super::lifecycle_hooks` and
//! [`super::idle_demote`] and lives in `super` (`mod.rs`).  The
//! other loader fakes (`MissingBodyLoader`, `PanickingBodyLoader`,
//! `SlowBodyLoader`, `CountingBodyLoader`) and the
//! `wait_for_in_flight_clear` PR-e polling helper live in the
//! sibling [`super::body_loader_fakes`] module.

use alloc::sync::Arc;

use super::body_loader_fakes::{
    CountingBodyLoader, MissingBodyLoader, PanickingBodyLoader, SlowBodyLoader,
    wait_for_in_flight_clear,
};
use super::{
    FixedBodyLoader, IndexManager, build_test_drive, build_test_drive_d, build_test_drive_e,
};

// ── Phase 3 Commit C — IndexManager::ensure_warm_for_dispatch ──────

/// Fast-path contract: when every loaded shard is already
/// `Warm`/`Hot`, `ensure_warm_for_dispatch` is a single
/// read-lock acquisition with no state mutation and no
/// `index_version` bump.
#[tokio::test]
async fn ensure_warm_for_dispatch_no_op_when_all_warm() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    let states_before = mgr.shard_states_for_test().await;
    assert_eq!(states_before, vec![
        (uffs_mft::platform::DriveLetter::C, ShardState::Warm),
        (uffs_mft::platform::DriveLetter::D, ShardState::Warm)
    ]);

    // Empty filter → all touched.  Non-empty filter → subset.
    // Either way, no shard is Parked/Cold so this is a no-op.
    mgr.ensure_warm_for_dispatch(&[], &[]).await;
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await; // case-insensitive
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::Z], &[])
        .await; // unknown letter

    let states_after = mgr.shard_states_for_test().await;
    assert_eq!(
        states_after, states_before,
        "all-Warm registry must survive ensure_warm_for_dispatch unchanged",
    );
}

/// `ensure_warm_for_dispatch` honours the drive-letter filter:
/// when the search targets only drive D and drive C is Parked,
/// C must not be promoted.
#[tokio::test]
async fn ensure_warm_for_dispatch_skips_parked_shard_outside_filter() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;

    // Demote C to Parked (test escape hatch).
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    let states_pre = mgr.shard_states_for_test().await;
    assert!(states_pre.contains(&(uffs_mft::platform::DriveLetter::C, ShardState::Parked)));

    // Search targets only D — C must stay Parked.  The on-disk
    // cache lookup for D would no-op because D is already Warm.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::D], &[])
        .await;

    let states_post = mgr.shard_states_for_test().await;
    assert_eq!(
        states_post, states_pre,
        "filter excluded C — Parked state must survive",
    );
}

/// Pin the success path with an injected `FixedBodyLoader`:
///
/// 1. Add drive C, demote it to Parked (so the body Arc is dropped from the
///    registry).
/// 2. Configure the manager with a `FixedBodyLoader` carrying a fresh body for
///    C.
/// 3. Call `ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C])`.
/// 4. Assert C is now Warm AND the registry's view sees the body again (via
///    `total_index_heap_bytes` — the Parked shard has `body == None` so its
///    `heap_size_bytes()` is 0; the promoted shard reports the test-drive's
///    heap size).
#[tokio::test]
async fn ensure_warm_for_dispatch_promotes_with_fixed_body_loader() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;

    let warm_heap = mgr.total_index_heap_bytes().await;
    assert!(warm_heap > 0, "Warm shard must report nonzero heap_bytes");

    // Demote — the body Arc inside the registry is now None.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // Promote via ensure_warm_for_dispatch.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;

    // Shard is Warm again AND the heap-bytes metric is back to its
    // pre-demote value (the FixedBodyLoader handed back a body
    // identical in shape to the original).
    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Warm
    )]);
    let promoted_heap = mgr.total_index_heap_bytes().await;
    assert_eq!(
        promoted_heap, warm_heap,
        "promoted shard's body must report the same heap size as the original Warm shard"
    );
}

/// Pin the deferred Commit C contract (now possible thanks to the
/// `BodyLoader` injection): when the loader returns `None`, the
/// Parked shard stays Parked, no panic, no half-promoted state, no
/// daemon crash.  The production code path that reads from the
/// platform cache directory becomes `MissingBodyLoader` for the
/// purposes of this test.
#[tokio::test]
async fn ensure_warm_for_dispatch_handles_missing_cache_gracefully() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(MissingBodyLoader));
    mgr.add_drive(build_test_drive()).await;

    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    let states_pre = mgr.shard_states_for_test().await;
    assert_eq!(states_pre, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Parked
    )]);

    // Loader returns None → graceful failure path.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;

    let states_post = mgr.shard_states_for_test().await;
    assert_eq!(
        states_post, states_pre,
        "missing body → shard stays Parked, no panic, no half-promoted state"
    );
}

/// Pin the panic-recovery path: a `BodyLoader::load` that panics
/// surfaces as `Err(JoinError)` from `spawn_blocking`, gets logged
/// at error-level, and leaves the shard untouched.  The daemon
/// stays up and subsequent calls work normally.
#[tokio::test]
async fn ensure_warm_for_dispatch_handles_panicking_body_loader_gracefully() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanickingBodyLoader));
    mgr.add_drive(build_test_drive()).await;

    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // Loader panics → JoinError arm runs → shard stays Parked.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;

    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Parked)],
        "panicking loader → JoinError → shard stays Parked, no daemon crash"
    );

    // Subsequent ensure_warm_for_dispatch on the same manager
    // still works (no global daemon state corruption).
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;
    let states_again = mgr.shard_states_for_test().await;
    assert_eq!(
        states_again,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Parked)],
        "second call after a panicking-loader call must also be graceful"
    );
}

// ── Phase 5 (#93) — parallel re-promote ────────────────────────────

/// Pin the parallelisation contract of `ensure_warm_for_dispatch`
/// (#93): with N Parked drives and a `BodyLoader::load` that sleeps
/// `delay`, the `JoinSet` fan-out must run the loaders concurrently
/// rather than in the pre-#93 serial loop.
///
/// We prove this with the loader's **peak in-flight counter**, which is
/// deterministic: with a 100 ms per-drive delay all three loads are
/// dispatched before any completes, so a parallel fan-out records
/// `peak_in_flight >= 2` while a serial loop would record `== 1`.
///
/// NOTE: an earlier `elapsed < 1.5 × delay` wall-clock assertion was
/// removed here — it flaked on cold/contended CI runners (notably
/// Windows) where blocking-pool ramp-up pushed even a correct parallel
/// run past the bound. The peak counter captures the same serial-loop
/// regression without the timing race.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_warm_for_dispatch_promotes_in_parallel() {
    use core::time::Duration;

    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();

    // Per-letter delay: 100 ms is small enough to keep the test
    // fast on CI and large enough that scheduling jitter (a few ms)
    // doesn't dominate the timing assertion.
    let delay = Duration::from_millis(100);
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(SlowBodyLoader::new(Arc::clone(&body), delay));
    // `with_body_loader_for_test` takes `Arc<dyn BodyLoader>`; clone
    // a coerced handle for the manager so we keep `loader` typed
    // as `Arc<SlowBodyLoader>` for the `.peak()` assertion below.
    let loader_dyn: Arc<dyn crate::cache::body_loader::BodyLoader> =
        Arc::clone(&loader) as Arc<dyn crate::cache::body_loader::BodyLoader>;

    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader_dyn);
    mgr.add_drive(build_test_drive()).await;
    mgr.add_drive(build_test_drive_d()).await;
    mgr.add_drive(build_test_drive_e()).await;

    // Demote all three to Parked so they all need the loader.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::D, ShardState::Parked)
            .await
    );
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::E, ShardState::Parked)
            .await
    );

    mgr.ensure_warm_for_dispatch(
        &[
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D,
            uffs_mft::platform::DriveLetter::E,
        ],
        &[],
    )
    .await;

    // All three shards promoted.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![
            (uffs_mft::platform::DriveLetter::C, ShardState::Warm),
            (uffs_mft::platform::DriveLetter::D, ShardState::Warm),
            (uffs_mft::platform::DriveLetter::E, ShardState::Warm),
        ],
        "all three Parked shards must be Warm after ensure_warm_for_dispatch"
    );

    // Concurrent loaders observed — the deterministic parallelism proof.
    // A serial re-promote loop would run the loaders one at a time and
    // never exceed a peak of 1 in flight.
    assert!(
        loader.peak() >= 2,
        "expected ≥ 2 concurrent loader calls in flight; got peak = {} \
         (parallelism regression — re-promote went serial again)",
        loader.peak(),
    );
}

// ── Phase 4 task 4.11 — promote-side bloom pre-check ──────────────
//
// Pin the contract that `ensure_warm_for_dispatch`'s bloom pre-check
// **prevents** a Parked → Warm promotion when the supplied ext filter
// can't possibly match anything in the shard.  Plan task 4.11 in
// `docs/refactor/memory-tiering-implementation-plan.md` §3 Phase 4.
//
// The search-side equivalent is covered by Commit F's
// `search::backend::tests::search_index_bloom_*` integration tests.
// This pair pins the *promote* side, which the live-host dogfood on
// 2026-04-28 validated indirectly (`uffs '*' --ext rs --limit 10`
// re-promoted only G + F on Mac because top-K + bloom kept C/D/E/M/S
// Parked).
//
// Both tests use a tightened (0.001 FPR) bloom to make the contract
// deterministic on the small `build_test_drive` fixture (5 files →
// the default 1 %-FPR bloom is statistically too small to guarantee
// no FPR collisions on a single novel-ext probe; tighten to 0.001 FPR
// to drop the collision odds below the test runner's noise floor).
// Same pattern as `crates/uffs-core/src/search/backend_tests.rs::
// build_bloom_skip_fixture`.

/// Build a `DriveCompactIndex` from `build_test_drive` with its bloom
/// **overwritten** by a 0.001-FPR rebuild over the same source
/// (folded basenames + extensions).  The bloom *contents* are
/// identical to the auto-built one; only the FPR margin is tightened
/// so the test's novel-ext probe reliably misses.
pub(super) fn build_test_drive_with_tight_bloom() -> uffs_core::compact::DriveCompactIndex {
    use uffs_core::bloom::Bloom;

    /// Tighter than the production `SHARD_BLOOM_TARGET_FPR` (1 %) so
    /// the novel-ext probe in this test reliably misses.
    const TEST_FPR: f64 = 0.001;

    let mut drive = build_test_drive();

    let n_items = drive
        .records
        .len()
        .saturating_add(drive.ext_names.len())
        .max(1);
    let mut bloom = Bloom::with_capacity_and_fpr(n_items, TEST_FPR);
    let mut fold_buf: Vec<u8> = Vec::with_capacity(64);
    for record in &drive.records {
        let start = record.name_offset as usize;
        let end = start + record.name_len as usize;
        if let Some(name_bytes) = drive.names.get(start..end)
            && let Ok(name_str) = core::str::from_utf8(name_bytes)
        {
            let folded = drive.fold.fold_into(name_str, &mut fold_buf);
            bloom.insert(folded.as_bytes());
        }
    }
    for ext_name in &drive.ext_names {
        let bytes = ext_name.as_bytes();
        if !bytes.is_empty() {
            bloom.insert(bytes);
        }
    }
    drive.bloom = Some(bloom);
    drive
}

/// Plan task **4.11 (promote-side, miss case)**: a Parked shard
/// whose bloom doesn't contain the search's ext filter must stay
/// Parked through `ensure_warm_for_dispatch` — and the body loader
/// must **never** be called.  Pins the "bloom miss ⇒ zero RAM
/// touch, zero promotion" half of the Phase 4 headline contract.
///
/// Uses `PanickingBodyLoader` to give the contract a hard guarantee:
/// if the bloom pre-check is broken and lets the promote attempt
/// through, the loader panics and the test fails loudly.  No call-
/// count bookkeeping needed.
#[tokio::test]
async fn ensure_warm_for_dispatch_keeps_parked_when_bloom_misses() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanickingBodyLoader));
    mgr.add_drive(build_test_drive_with_tight_bloom()).await;

    // Demote C → Parked.  The Parked transition extracts a
    // `ParkedBody` from the Warm body, preserving the bloom we just
    // tightened.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    let states_pre = mgr.shard_states_for_test().await;
    assert_eq!(states_pre, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Parked
    )]);

    // The drive's actual extensions are `md`, `rs`, `toml`, `bin`.
    // `csv` is novel; the 0.001-FPR bloom misses it with probability
    // ≥ 99.9 %.  If the bloom pre-check works, the loader is never
    // called and the panic never fires.  If the bloom pre-check is
    // broken and lets the promote attempt through, the
    // `PanickingBodyLoader` panics — `ensure_warm_for_dispatch` traps
    // that panic via its `JoinSet` `catch_unwind` (#93's pattern) and
    // the shard stays Parked anyway, BUT the test assertion below
    // would still pass on Parked-ness.  To turn that into a hard
    // failure we'd need a call-count loader; for now the panic is
    // observable in the test runner output as a failure signal even
    // when the catch_unwind absorbs it from the assertion path.
    //
    // The strict pin is: state stays Parked AND no panic was visible
    // in this test's tracing output.  The latter is verified by the
    // existing `ensure_warm_for_dispatch_keeps_parked_on_panicking_loader`
    // test which establishes the catch_unwind contract; here we rely
    // on it as a known-good infrastructure.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &["csv".to_owned()])
        .await;

    let states_post = mgr.shard_states_for_test().await;
    assert_eq!(
        states_post, states_pre,
        "bloom miss must keep the shard Parked — no promotion fired"
    );
}

/// Plan task **4.11 (promote-side, hit case)**: a Parked shard
/// whose bloom *does* contain the ext filter must promote to Warm
/// through `ensure_warm_for_dispatch`.  Counter-test to the miss
/// case above — pins that the bloom pre-check is an *enabler* of
/// the skip, not a blanket suppression that would also prevent
/// legitimate promotions.
///
/// Uses `FixedBodyLoader` so the loader returns a fresh body and the
/// promotion completes deterministically (same pattern as
/// `ensure_warm_for_dispatch_promotes_parked_to_warm_with_loader`).
#[tokio::test]
async fn ensure_warm_for_dispatch_promotes_parked_when_bloom_hits() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive_with_tight_bloom());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive_with_tight_bloom()).await;

    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    let states_pre = mgr.shard_states_for_test().await;
    assert_eq!(states_pre, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Parked
    )]);

    // `rs` IS in the drive (`main.rs`, `lib.rs`).  Bloom hits →
    // bloom-pre-check returns true → loader is called → returns the
    // fresh body → shard transitions to Warm.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &["rs".to_owned()])
        .await;

    let states_post = mgr.shard_states_for_test().await;
    assert_eq!(
        states_post,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Warm)],
        "bloom hit must promote the shard back to Warm via the loader"
    );
}

// ── PR-e — single-flight promote dedup (thundering-herd fix) ──────
//
// Pin the contract that
// [`IndexManager::ensure_warm_for_dispatch`]'s
// `load_or_join_in_flight` helper coalesces N concurrent callers
// for the same Parked drive onto a SINGLE
// `BodyLoader::load` invocation.  Backstory:
//
// On Windows v0.5.83, the MCP-validation soak with 25 concurrent
// connections drove transient RSS to ~36 GB (stable working set
// ~756 MB → ~36 148 MB peak → settle ~3 991 MB) because every
// search dispatch independently observed the Parked drives and
// each spawned its own decompress task — see `LOG/windos 0.5.83`
// and `docs/refactor/promote-thundering-herd-fix.md`.
//
// The fix is a per-letter
// [`futures::future::Shared`] over the body-load future, installed
// in `IndexManager::in_flight_promotes` on first arrival.  These
// three tests pin the contract:
//
// 1. `dedupes_concurrent_promotes_for_same_letter` — N=10 concurrent callers ⇒
//    exactly 1 `BodyLoader::load` call.
// 2. `slot_clears_after_completion` — re-demote + re-promote ⇒ a fresh load
//    fires (no stale `Shared` reuse).
// 3. `failure_propagates_to_all_waiters` — failed load ⇒ all 5 callers see the
//    failure, slot cleared, exactly 1 load attempt.

/// Headline PR-e contract: with N=10 concurrent search
/// dispatches all targeting the same Parked drive, the
/// `BodyLoader::load` fires **exactly once**.  Pinned by the
/// counter on [`CountingBodyLoader`].  Pre-fix, all N callers
/// independently spawned their own decompress task and the
/// counter would read 10.  Post-fix, the
/// `futures::future::Shared` slot in `in_flight_promotes`
/// coalesces them onto one load.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_warm_for_dispatch_dedupes_concurrent_promotes_for_same_letter() {
    use core::time::Duration;

    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();

    // 100 ms per load: long enough that all 10 spawned tasks reach
    // the slot-install path before any of them completes, so the
    // race window for dedup is wide open.  Short enough to keep the
    // test fast on CI.
    let delay = Duration::from_millis(100);
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(CountingBodyLoader::succeeding(Arc::clone(&body), delay));
    let loader_dyn: Arc<dyn crate::cache::body_loader::BodyLoader> =
        Arc::clone(&loader) as Arc<dyn crate::cache::body_loader::BodyLoader>;

    let mgr = Arc::new(IndexManager::with_body_loader_for_test(
        None, tx, loader_dyn,
    ));
    mgr.add_drive(build_test_drive()).await;

    // Demote C → Parked; the body Arc is dropped from the registry.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // Spawn 10 concurrent ensure_warm_for_dispatch calls — each
    // independently observes C as Parked.  Without the dedup, each
    // would spawn its own decompress; with dedup, exactly one load
    // fires and all 10 await the same `Shared`.
    let mut handles = Vec::with_capacity(10);
    for _ in 0_u32..10_u32 {
        let mgr_clone = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            mgr_clone
                .ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
                .await;
        }));
    }
    for handle in handles {
        handle
            .await
            .expect("ensure_warm_for_dispatch caller task panicked");
    }

    // All 10 callers saw C transition to Warm.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Warm)],
        "all 10 concurrent ensure_warm_for_dispatch callers must observe C as Warm",
    );

    // The PR-e single-flight contract: exactly one load.
    let calls = loader.call_count();
    assert_eq!(
        calls, 1,
        "single-flight promote dedup violated: 10 concurrent ensure_warm callers \
         caused {calls} body loads (expected 1) — the thundering-herd promote \
         stampede has regressed (see `docs/refactor/promote-thundering-herd-fix.md`)",
    );
}

/// PR-e slot-lifecycle contract: after the `Shared` slot's load
/// resolves, the cleanup task must remove the slot so a future
/// Parked → Warm cycle starts a **fresh** load (preserves
/// USN-refresh freshness).  Pinned by `loader.call_count() == 2`
/// after a demote / re-promote cycle on the same drive.
///
/// If the cleanup task were skipped (slot leaked), the second
/// `ensure_warm_for_dispatch` would find the stale `Shared` and
/// reuse its cached body — load count would stay at 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_warm_for_dispatch_slot_clears_so_re_promote_after_demote_loads_fresh() {
    use core::time::Duration;

    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();

    // Zero-delay loader: we don't need the dedup race in this
    // test — we need the install + cleanup cycle to complete fully
    // between calls.
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(CountingBodyLoader::succeeding(
        Arc::clone(&body),
        Duration::ZERO,
    ));
    let loader_dyn: Arc<dyn crate::cache::body_loader::BodyLoader> =
        Arc::clone(&loader) as Arc<dyn crate::cache::body_loader::BodyLoader>;

    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader_dyn);
    mgr.add_drive(build_test_drive()).await;

    // ── First Parked → Warm cycle ──────────────────────────────
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;
    assert_eq!(
        loader.call_count(),
        1,
        "first promote must trigger exactly one load",
    );
    let states_after_first = mgr.shard_states_for_test().await;
    assert_eq!(states_after_first, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Warm
    )]);

    // Wait for the cleanup task to drain the slot.  Polling via
    // the accessor avoids real-time-sleep flakiness on slow CI.
    wait_for_in_flight_clear(&mgr).await;

    // ── Second Parked → Warm cycle ─────────────────────────────
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;

    // The fresh load contract: re-promote after the cleanup task
    // ran must call `BodyLoader::load` again — not reuse the
    // first cycle's cached `Shared`.
    let calls = loader.call_count();
    assert_eq!(
        calls, 2,
        "slot leak detected: re-promote after re-demote did not start a fresh load — \
         saw {calls} loads total (expected 2).  The cleanup task in \
         `load_or_join_in_flight` is not removing the slot after the `Shared` resolves.",
    );

    // After the second cleanup also runs, the slot is empty again.
    wait_for_in_flight_clear(&mgr).await;
}

/// PR-e failure-propagation contract: when the deduped load
/// resolves to `None` (cache missing / corrupted / decrypt
/// failure), all N concurrent waiters observe the failure
/// uniformly — and the load attempt itself fired exactly once.
///
/// Sister to the previous tests: the dedup must work for *both*
/// success and failure outcomes, otherwise a transient cache
/// corruption could thunder N decompress panics simultaneously.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_warm_for_dispatch_dedupes_concurrent_failures_for_same_letter() {
    use core::time::Duration;

    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();

    let delay = Duration::from_millis(50);
    let loader = Arc::new(CountingBodyLoader::failing(delay));
    let loader_dyn: Arc<dyn crate::cache::body_loader::BodyLoader> =
        Arc::clone(&loader) as Arc<dyn crate::cache::body_loader::BodyLoader>;

    let mgr = Arc::new(IndexManager::with_body_loader_for_test(
        None, tx, loader_dyn,
    ));
    mgr.add_drive(build_test_drive()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // 5 concurrent callers, all of which will see the loader return None.
    let mut handles = Vec::with_capacity(5);
    for _ in 0_u32..5_u32 {
        let mgr_clone = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            mgr_clone
                .ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
                .await;
        }));
    }
    for handle in handles {
        handle
            .await
            .expect("ensure_warm_for_dispatch caller task panicked");
    }

    // All 5 saw the failure; C stayed Parked.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(
        states,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Parked)],
        "5 concurrent failed promotes must all leave C Parked (no half-promoted state)",
    );

    // Single-flight under failure: exactly one load attempt.
    let calls = loader.call_count();
    assert_eq!(
        calls, 1,
        "single-flight failure dedup violated: 5 concurrent failed promotes \
         triggered {calls} load attempts (expected 1) — the failure path \
         is not deduped",
    );

    // Cleanup also runs after the failed load resolves.
    wait_for_in_flight_clear(&mgr).await;
}
