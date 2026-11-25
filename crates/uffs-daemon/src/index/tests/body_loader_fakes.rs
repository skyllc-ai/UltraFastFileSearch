// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `BodyLoader` test fakes used by [`super::ensure_warm`].
//!
//! Each fake exercises a distinct contract of
//! [`crate::index::IndexManager::ensure_warm_for_dispatch`]:
//!
//! * [`MissingBodyLoader`] — `load` returns `None` (Phase 3 Commit C
//!   "missing-cache" graceful-failure path).
//! * [`PanickingBodyLoader`] — `load` panics, exercising the `Err(JoinError)`
//!   arm of the `spawn_blocking` match.
//! * [`SlowBodyLoader`] — sleeps before returning, with a `peak_in_flight`
//!   counter so tests can assert real parallelism (Phase 5 #93 — re-promote
//!   fan-out across the blocking pool).
//! * [`CountingBodyLoader`] — counts total `load` calls and optionally returns
//!   failure (PR-e — single-flight promote dedup).
//!
//! Plus the [`wait_for_in_flight_clear`] polling helper used by the
//! PR-e slot-lifecycle test.
//!
//! The success-path [`super::FixedBodyLoader`] lives in
//! [`super`][super] (`mod.rs`) because it's also shared with
//! [`super::lifecycle_hooks`] and [`super::idle_demote`].
//! Keeping it there avoids cyclic visibility and matches the
//! "fixtures shared by ≥ 2 sibling test modules live in `mod.rs`"
//! convention.

use alloc::sync::Arc;

use super::IndexManager;

/// A `BodyLoader` that always returns `None` — simulates a missing
/// or stale cache file between demote and promote.
pub(super) struct MissingBodyLoader;

impl crate::cache::body_loader::BodyLoader for MissingBodyLoader {
    fn load(
        &self,
        _letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        None
    }
}

/// A `BodyLoader` whose `load` method panics — exercises the
/// `Err(JoinError)` arm of the spawn-blocking match in
/// `ensure_warm_for_dispatch`.  The panic is contained inside
/// `tokio::task::spawn_blocking`'s thread; the daemon stays up and
/// the shard stays in its current tier.
pub(super) struct PanickingBodyLoader;

impl crate::cache::body_loader::BodyLoader for PanickingBodyLoader {
    fn load(
        &self,
        _letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        panic!("PanickingBodyLoader::load — synthetic panic for the JoinError arm");
    }
}

/// A `BodyLoader` that sleeps for `delay` before returning a clone
/// of `body`, and records the peak number of concurrent calls
/// in flight.  Used to verify that
/// [`IndexManager::ensure_warm_for_dispatch`] fans out per-letter
/// loads across the blocking pool instead of serialising them.
pub(super) struct SlowBodyLoader {
    body: Arc<uffs_core::compact::DriveCompactIndex>,
    delay: core::time::Duration,
    in_flight: core::sync::atomic::AtomicUsize,
    peak_in_flight: core::sync::atomic::AtomicUsize,
}

impl SlowBodyLoader {
    pub(super) fn new(
        body: Arc<uffs_core::compact::DriveCompactIndex>,
        delay: core::time::Duration,
    ) -> Self {
        Self {
            body,
            delay,
            in_flight: core::sync::atomic::AtomicUsize::new(0),
            peak_in_flight: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(super) fn peak(&self) -> usize {
        self.peak_in_flight
            .load(core::sync::atomic::Ordering::Acquire)
    }
}

impl crate::cache::body_loader::BodyLoader for SlowBodyLoader {
    fn load(
        &self,
        _letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        use core::sync::atomic::Ordering;

        let now = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        // Bump peak via a CAS loop: read the current peak, write
        // back `now` only if it's strictly larger.  Pure `fetch_max`
        // would be one call but isn't stable on all targets we
        // build; the loop is portable and the contention window is
        // microscopic (only the first few in-flight loaders ever
        // raise the peak).
        let mut prev = self.peak_in_flight.load(Ordering::Acquire);
        while now > prev {
            match self.peak_in_flight.compare_exchange_weak(
                prev,
                now,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
        std::thread::sleep(self.delay);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        Some(Arc::clone(&self.body))
    }
}

/// `BodyLoader` that records the total number of `load` calls
/// across its lifetime and optionally sleeps `delay` per call.
///
/// `body == Some` returns a clone (success path); `body == None`
/// simulates a missing-cache failure.  The `delay` is what makes
/// the dedup race observable: with N concurrent callers and a
/// delay long enough to outlast the slot-install scheduler turn,
/// every caller after the first finds the existing
/// `Shared` slot and joins the in-flight load instead of
/// installing its own.
pub(super) struct CountingBodyLoader {
    body: Option<Arc<uffs_core::compact::DriveCompactIndex>>,
    calls: core::sync::atomic::AtomicUsize,
    delay: core::time::Duration,
}

impl CountingBodyLoader {
    pub(super) fn succeeding(
        body: Arc<uffs_core::compact::DriveCompactIndex>,
        delay: core::time::Duration,
    ) -> Self {
        Self {
            body: Some(body),
            calls: core::sync::atomic::AtomicUsize::new(0),
            delay,
        }
    }

    pub(super) fn failing(delay: core::time::Duration) -> Self {
        Self {
            body: None,
            calls: core::sync::atomic::AtomicUsize::new(0),
            delay,
        }
    }

    pub(super) fn call_count(&self) -> usize {
        self.calls.load(core::sync::atomic::Ordering::Acquire)
    }
}

impl crate::cache::body_loader::BodyLoader for CountingBodyLoader {
    fn load(
        &self,
        _letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        self.calls
            .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        std::thread::sleep(self.delay);
        self.body.clone()
    }
}

/// Spin-wait for the `in_flight_promotes` map to drain — the
/// cleanup task spawned by `load_or_join_in_flight` runs
/// asynchronously, and the `slot_clears_after_completion` test
/// needs to assert the slot is gone before it triggers the
/// re-promote.  Polling via the test accessor (rather than a
/// real-time sleep) keeps the test deterministic on slow CI
/// runners.
///
/// Times out after ~100 ms; if the cleanup task hasn't run by
/// then, the test fails loudly with a clear message.  100 ms is
/// 1000× the expected cleanup latency (a single mutex acquire +
/// a `HashMap` remove, ~µs), so the bound only fires on a real
/// regression.
pub(super) async fn wait_for_in_flight_clear(mgr: &IndexManager) {
    // Suffix the loop bound to dodge `clippy::default_numeric_fallback`
    // (the codebase forbids implicit `i32` integer types).
    for _ in 0_u32..100_u32 {
        if mgr.in_flight_promotes_len_for_test() == 0 {
            return;
        }
        tokio::time::sleep(core::time::Duration::from_millis(1)).await;
    }
    panic!(
        "in-flight promote slot did not clear within ~100 ms — \
         the cleanup task spawned by `load_or_join_in_flight` did \
         not run (regression: `Shared` slot leak)"
    );
}
