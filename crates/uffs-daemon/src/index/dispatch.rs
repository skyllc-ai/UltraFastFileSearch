// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search-dispatch helpers for [`IndexManager`].
//!
//! Two pre-search hooks plus the single-flight promote machinery
//! they share live here:
//!
//! * [`IndexManager::record_search_dispatch`] — stamp `last_query_at_ms` on
//!   every Warm/Hot shard so the demote controller sees a fresh idle clock.
//!   Phase 3 Commit C wired this; Phase 6 reads the same timestamp via
//!   `DriveStats::decay_ema_qpm` for the adaptive-TTL formulas.
//! * [`IndexManager::ensure_warm_for_dispatch`] — promote any Parked/Cold
//!   shards the search will touch, before [`IndexManager::snapshot`] reads the
//!   active subset. Three-phase detect → load → swap orchestration with the
//!   bloom pre-check (Phase 4 Commit F) and the per-letter single-flight dedup
//!   (PR-e).
//!
//! The single-flight machinery — [`IndexManager::load_or_join_in_flight`],
//! [`IndexManager::install_or_join_in_flight_slot`],
//! [`IndexManager::build_load_future`], and the
//! [`IndexManager::bloom_pre_check_should_promote`] decision helper —
//! lives in this module too because callers and helpers form a
//! single cohesive cluster: the dedup-map slot manager spawns
//! the body-load future, the body-load future drives the
//! `BodyLoader`/`Prefetch` lifecycle hooks, and the bloom
//! pre-check feeds the `ensure_warm_for_dispatch` filter.
//! Splitting them apart would force every entry point to import
//! every other; keeping them grouped keeps the call graph
//! readable.

use alloc::sync::Arc;

use futures::{FutureExt as _, StreamExt as _};

use super::{InFlightLoad, InFlightPromotes, IndexManager};
use crate::cache::unix_now_ms;

impl IndexManager {
    /// Phase 3 Commit C — record one search dispatch's worth of
    /// activity onto every Warm/Hot shard's
    /// [`crate::cache::shard::DriveStats`].
    ///
    /// Phase 4+ moves the increment into the per-shard
    /// search-dispatch loop so bloom-skipped shards don't bump
    /// their counters.  The Phase 3 implementation visits every
    /// active shard so the demote controller's
    /// `idle_secs = (now_ms - last_query_at_ms) / 1000`
    /// computation never observes a stale `last_query_at_ms`
    /// when the controller's first tick lands moments after a
    /// burst of search dispatches.
    ///
    /// Phase 3 routes the increment through
    /// [`crate::cache::shard::DriveStats::mark_query_at`] so the
    /// same hot-path write also stores the dispatch timestamp in
    /// `last_query_at_ms`; the demote controller in
    /// [`IndexManager::demote_idle_shards`] reads that timestamp to
    /// compute `idle_secs`.  Phase 6 additionally feeds the EMA
    /// the adaptive-TTL formulas use — see
    /// [`crate::cache::shard::DriveStats::decay_ema_qpm`].
    pub(super) async fn record_search_dispatch(&self) {
        let now_ms = unix_now_ms();
        let guard = self.index.read().await;
        for shard in guard.iter() {
            if matches!(
                shard.state(),
                crate::cache::ShardState::Warm | crate::cache::ShardState::Hot
            ) {
                shard.stats.mark_query_at(now_ms);
            }
        }
    }

    /// Phase 3 Commit C — promote any Parked/Cold shards that this
    /// search will dispatch to, before
    /// [`IndexManager::snapshot`] reads the active subset.
    ///
    /// Three-phase orchestrator (read-detect → spawn-blocking
    /// load → write-swap) — see implementation comments below.
    ///
    /// `params_drives` is the search's drive-letter filter
    /// ([`uffs_client::protocol::SearchParams::drives`]).  An empty
    /// slice means "no filter" — the touched set is every loaded
    /// shard.  When non-empty, only shards whose drive letter
    /// case-insensitively matches a filter entry are considered.
    ///
    /// **Conservative on under-promote, lenient on over-promote.**
    /// If the search pattern itself implies a drive prefix
    /// (e.g. `"C:*.txt"`) but `params_drives` is empty, we'll
    /// over-promote (touching shards the search backend will then
    /// skip) — wasted I/O, no correctness issue.  The opposite
    /// (under-promote → search misses a Parked shard) is what we
    /// can't afford, hence the empty-filter == all-loaded fallback.
    ///
    /// No-op if every touched shard is already Warm/Hot — common
    /// case, single read-lock acquisition only.
    pub(super) async fn ensure_warm_for_dispatch(
        &self,
        params_drives: &[uffs_mft::platform::DriveLetter],
        ext_terms: &[String],
    ) {
        // ── Phase 1: read-lock detection (fast path) ───────────
        // See [`Self::promote_plan`] — the same computation also
        // answers the `warm_plan` RPC, so external gates (the MCP
        // cold-index gate) and this dispatch can never disagree
        // about which drives a query would actually promote.
        let needs_promote = self.promote_plan(params_drives, ext_terms).await;
        if needs_promote.is_empty() {
            return;
        }

        // ── Phase 2: per-letter parallel body load with single-flight dedup ─
        // For each Parked/Cold letter, drive one
        // [`IndexManager::load_or_join_in_flight`] call in parallel via a
        // [`futures::stream::FuturesUnordered`].  The helper
        // performs the I/O + decrypt + decompress + runtime-mmap
        // materialisation inside its inner `tokio::task::spawn_blocking`
        // and returns `None` on any non-fatal failure (missing cache
        // file, stale, decrypt error, panic, abort).  The drain
        // loop traces and skips — the shard stays in its current tier
        // and the search will dispatch against the unchanged active
        // subset.
        //
        // **Why parallel across letters** (#93): the cold-boot WARM
        // path loads N drives in parallel from the same on-disk
        // caches and completes in ~max(per-drive); the original
        // serial loop here did sum(per-drive) and was 2–3× slower on
        // real workloads (15.1 s for 6 drives in v0.5.80, vs. 5.7 s
        // for 7 drives at `daemon start`).  Each per-letter
        // write-lock swap is a sub-µs pointer-swap; even at N=7 the
        // cumulative contention is < 10 µs, well below the per-drive
        // load cost (~1 s+).
        //
        // **Why deduped per letter** (PR-e — see
        // `docs/refactor/promote-thundering-herd-fix.md`): the
        // Windows v0.5.83 MCP-validation soak triggered up to 8
        // concurrent body loads per Parked drive when 25 search
        // dispatches simultaneously hit the same Parked subset,
        // causing transient RSS spikes to ~36 GB (8 × 1.3 GB ×
        // 4 drives).  The helper coalesces concurrent callers onto
        // a single [`futures::future::Shared`] body-load future per
        // letter, capping transient allocations at one per Parked
        // drive in flight.
        // The client waits silently for this (it prints nothing but the
        // result), so the daemon log is where a long warm-up is recorded.
        tracing::info!(
            target: "shard.transition",
            drives = ?needs_promote,
            reason = "promote-on-search",
            "search needs parked drives back in memory; warming them before dispatch",
        );
        let warm_started = std::time::Instant::now();
        let mut load_set: futures::stream::FuturesUnordered<_> =
            futures::stream::FuturesUnordered::new();
        for letter in needs_promote {
            let in_flight = Arc::clone(&self.in_flight_promotes);
            let loader = Arc::clone(&self.body_loader);
            let prefetch = Arc::clone(&self.prefetch);
            load_set.push(async move {
                let body = Self::load_or_join_in_flight(in_flight, loader, prefetch, letter).await;
                (letter, body)
            });
        }

        // ── Phase 3: drain results, per-letter write-lock swap ─
        // `promote_letter` is `Option`-returning so a benign
        // race (another task promoted between our read-detect
        // and write-swap, or a demote landed on top of the
        // Parked state we observed) drops the freshly-loaded
        // body Arc and leaves the canonical registry alone.
        //
        // The `JoinError` arm of the pre-PR-e implementation moved
        // inside [`IndexManager::load_or_join_in_flight`]'s inner
        // spawn-blocking handling: aborts surface as `None` here
        // with the same `shard.transition` warning.  No outcome is
        // observably different.
        while let Some((letter, body_opt)) = load_set.next().await {
            let Some(body) = body_opt else {
                tracing::warn!(
                    target: "shard.transition",
                    drive = %letter,
                    reason = "promote-on-search",
                    "compact-cache load returned None; shard stays in current tier",
                );
                continue;
            };
            self.install_promoted(letter, body, warm_started).await;
        }
    }

    /// Install a freshly loaded `body` for `letter` (the write-lock swap
    /// half of [`Self::ensure_warm_for_dispatch`]) and log the warm-up's
    /// duration, measured from `warm_started`.
    async fn install_promoted(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        body: Arc<uffs_core::compact::DriveCompactIndex>,
        warm_started: std::time::Instant,
    ) {
        let mut guard = self.index.write().await;
        if let Some(new_registry) = guard.promote_letter(letter, body) {
            // PR-f — refresh the freshly-promoted shard's
            // `last_query_at_ms` to "now" so the demote-idle
            // controller's first 30 s tick after this promote sees
            // a fresh idle clock rather than the stale value the
            // shard carried from before parking.  Without this
            // bump, a shard whose last_query_at_ms is more than
            // `WARM_TO_PARKED_IDLE_SECS` (default 300 s) in the
            // past — common when re-promoting after a long idle
            // window — will be re-demoted on the very next tick,
            // before `record_search_dispatch` runs at the end of
            // the search to stamp the canonical timestamp.  The
            // observed promote-then-immediate-demote thrash on
            // the v0.5.85 Windows soak (G/F/M demoted within 0.5
            // –5 s of their respective promotes) was caused
            // exactly by this gap.  See the regression test
            // `promote_resets_idle_clock_against_thrash` in
            // `crate::index::tests::idle_demote`.
            //
            // Mirrors the seed in `Self::add_drive` and
            // `Self::replace_drive`; uses `mark_loaded_at`
            // (single-store, no queries_total bump) so the
            // per-drive query count stays accurate.
            let now_ms = unix_now_ms();
            if let Some(shard) = new_registry.iter().find(|shard| shard.drive == letter) {
                shard.stats.mark_loaded_at(now_ms);
            }
            *guard = Arc::new(new_registry);
            drop(guard);
            self.bump_index_version();
            tracing::info!(
                target: "shard.transition",
                drive = %letter,
                reason = "promote-on-search",
                elapsed_ms = warm_started.elapsed().as_millis(),
                "parked drive warmed for search",
            );
        }
    }

    /// Per-letter single-flight body-load: at most one
    /// [`crate::cache::body_loader::BodyLoader::load`] call per drive
    /// at any moment, regardless of how many concurrent search
    /// dispatches request the same Parked drive.
    ///
    /// **Contract:**
    ///
    /// * The first caller for `letter` builds and installs a
    ///   [`futures::future::Shared`] over the load + prefetch future, then
    ///   awaits it.
    /// * Concurrent callers for the same `letter` find the existing slot, clone
    ///   the same `Shared`, and await — receiving the same
    ///   `Option<Arc<DriveCompactIndex>>` outcome the original load produced.
    /// * A dedicated cleanup task (spawned at install time on the Tokio
    ///   runtime, **independent of any awaiter's lifetime**) awaits the same
    ///   `Shared` and removes the slot once it resolves.  This makes the slot
    ///   lifecycle robust against cancellation: even if every caller's outer
    ///   task is dropped mid-await, the cleanup task still runs and the slot is
    ///   removed so the next Parked → Warm cycle starts a fresh load (preserves
    ///   USN-refresh freshness).
    /// * The `BodyLoader::load` call still happens inside
    ///   [`tokio::task::spawn_blocking`] wrapped in
    ///   [`std::panic::catch_unwind`], so panics surface as `None`.
    /// * The kernel-prefetch hint runs **once** per load (inside the deduped
    ///   future) so 25 awaiters don't fire 25 `posix_madvise` /
    ///   `PrefetchVirtualMemory` calls.
    ///
    /// **Performance contract** (the PR-e headline): under N
    /// concurrent calls for the same Parked letter, exactly one
    /// `BodyLoader::load` call fires.  Pinned by the
    /// `dedupes_concurrent_promotes_for_same_letter` test in
    /// `index/tests/ensure_warm.rs`.
    ///
    /// Takes Arc fields explicitly (rather than `&self`) so the
    /// helper can be called from a `FuturesUnordered`-driven
    /// `async move` block in [`IndexManager::ensure_warm_for_dispatch`]
    /// without forcing the surrounding loop to hold a borrow of
    /// `self` across the await.
    pub(super) async fn load_or_join_in_flight(
        in_flight: InFlightPromotes,
        loader: Arc<dyn crate::cache::body_loader::BodyLoader>,
        prefetch: Arc<dyn crate::cache::prefetch::Prefetch>,
        letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        // The synchronous slot lookup + install lives in its own
        // helper so the `MutexGuard` it acquires is naturally scoped
        // to a single sync call — no guard held across any await,
        // and `clippy::significant_drop_tightening` is satisfied.
        let fut = Self::install_or_join_in_flight_slot(&in_flight, &loader, &prefetch, letter);
        fut.await
    }

    /// Synchronous slot manager for [`IndexManager::load_or_join_in_flight`].
    ///
    /// Either returns a clone of the existing per-letter
    /// [`InFlightLoad`] (the second-and-onwards concurrent caller
    /// path) or builds a fresh one, installs it, and spawns the
    /// cleanup task that removes the slot once the load resolves
    /// (the first-caller path).
    ///
    /// The lock is held only for the [`std::collections::hash_map::Entry`]
    /// lookup + install (a few ns); the cleanup task is spawned
    /// **after** the `MutexGuard` is dropped, so no async work or
    /// `tokio::spawn` overhead happens under the lock.  Callers
    /// then `.await` the returned [`InFlightLoad`] with no lock
    /// held — which keeps the dedup map non-blocking even when the
    /// underlying body load takes seconds.
    fn install_or_join_in_flight_slot(
        in_flight: &InFlightPromotes,
        loader: &Arc<dyn crate::cache::body_loader::BodyLoader>,
        prefetch: &Arc<dyn crate::cache::prefetch::Prefetch>,
        letter: uffs_mft::platform::DriveLetter,
    ) -> InFlightLoad {
        use std::collections::hash_map::Entry;

        // Acquire the lock, run the Entry lookup, drop the lock
        // (via the inner block scope) before spawning the cleanup
        // task.  Returning `(shared, was_inserted)` carries the
        // first-caller signal out across the drop boundary.
        let (shared, was_inserted) = {
            let mut map = in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match map.entry(letter) {
                Entry::Occupied(slot) => (slot.get().clone(), false),
                Entry::Vacant(slot) => {
                    let shared: InFlightLoad =
                        Self::build_load_future(Arc::clone(loader), Arc::clone(prefetch), letter)
                            .boxed()
                            .shared();
                    slot.insert(shared.clone());
                    (shared, true)
                }
            }
        };

        if was_inserted {
            // Cleanup task: awaits the same `Shared` independently
            // of any caller.  Even if every caller is cancelled
            // mid-await, the cleanup task keeps the inner future
            // polled to completion (the spawn_blocking work runs
            // on the blocking pool regardless) and removes the
            // slot.  Spawned **outside** the locked block above so
            // no lock is held across the `tokio::spawn` call.
            let cleanup_in_flight = Arc::clone(in_flight);
            let cleanup_fut = shared.clone();
            tokio::spawn(async move {
                // The `Shared` resolves to
                // `Option<Arc<DriveCompactIndex>>`; we discard it
                // (every awaiting caller already received their
                // own clone).  `drop(...await)` is the codebase's
                // idiom for this — see
                // `crates/uffs-daemon/src/index/tests/registry.rs`
                // for prior art.
                drop(cleanup_fut.await);
                cleanup_in_flight
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&letter);
            });
        }

        shared
    }

    /// Build the single-flight body-load future for `letter`.
    ///
    /// This is the future that gets wrapped in
    /// [`futures::future::Shared`] and stored in the in-flight
    /// slot map by [`IndexManager::load_or_join_in_flight`].  Factored out
    /// for readability — the inner async block carries non-trivial
    /// panic-recovery + prefetch-hint logic that would otherwise
    /// drown the dedup machinery in noise.
    ///
    /// Returns `None` on any of:
    ///
    /// * The synchronous `BodyLoader::load` panics (caught by `catch_unwind`).
    /// * The blocking task itself is aborted (caught by the outer
    ///   `unwrap_or_else` on the `JoinHandle::await`).
    /// * The loader returns `None` (missing / stale / corrupted cache file).
    ///
    /// On `Some(body)` the kernel-prefetch hint fires **once**
    /// before the future resolves; failures to hint are logged at
    /// `warn` and don't block the promotion.
    async fn build_load_future(
        loader: Arc<dyn crate::cache::body_loader::BodyLoader>,
        prefetch: Arc<dyn crate::cache::prefetch::Prefetch>,
        letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        // `catch_unwind` lives in `std` (needs the unwinding
        // runtime); `AssertUnwindSafe` lives in `core` (the
        // production lint enforces `core` imports for items
        // that are available there).
        let body = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| loader.load(letter)))
                .unwrap_or_else(|_payload| {
                    tracing::error!(
                        target: "shard.transition",
                        drive = %letter,
                        reason = "promote-on-search",
                        "blocking-task panic during cache load; shard stays in current tier",
                    );
                    None
                })
        })
        .await
        .unwrap_or_else(|join_err| {
            tracing::warn!(
                target: "shard.transition",
                drive = %letter,
                error = %join_err,
                reason = "promote-on-search",
                "blocking-task aborted before completion; shard stays in current tier",
            );
            None
        });

        // ── Phase 5 task 5.5 — prefetch hint ─────────────
        // Best-effort kernel-prefetch on the records + names
        // regions before the future resolves and the
        // orchestrator acquires the registry write-lock for
        // the swap.  Mac/Linux: `posix_madvise(MADV_WILLNEED)`
        // per region; Windows: single `PrefetchVirtualMemory`
        // call.  The body `Arc` keeps the underlying
        // allocation / mmap alive across this call so the raw
        // pointers stay valid (Send/Sync wrapper:
        // `PrefetchRegion`).  Errors are logged and ignored —
        // the shard still promotes.
        //
        // Single-flight property: this fires **once** per
        // load even when N awaiters share the future, since
        // the hint runs inside the future before it resolves
        // — once, in the cleanup task's scheduler turn.
        if let Some(body_arc) = body.as_ref() {
            let regions = [
                crate::cache::prefetch::PrefetchRegion {
                    ptr: body_arc.records.as_slice().as_ptr().cast::<u8>(),
                    len: size_of_val(body_arc.records.as_slice()),
                },
                crate::cache::prefetch::PrefetchRegion {
                    ptr: body_arc.names.as_slice().as_ptr(),
                    len: body_arc.names.as_slice().len(),
                },
            ];
            if let Err(err) = prefetch.hint(&regions) {
                tracing::warn!(
                    target: "shard.transition",
                    drive = %letter,
                    error = %err,
                    reason = "promote-on-search",
                    "Prefetch::hint failed; shard still promotes",
                );
            }
        }
        body
    }

    /// The drives a dispatch with this scope + resolved ext-term set
    /// would need to promote: every Parked/Cold shard in the touched
    /// set whose bloom cannot prove it irrelevant.
    ///
    /// Single read-lock acquisition; the registry's `iter()` is a Vec
    /// walk, no allocation beyond the returned Vec.
    ///
    /// Phase 4 Commit F — for Parked shards we additionally probe the
    /// bloom against `ext_terms`: a miss means the shard provably has
    /// no records matching the ext filter, so the promote is skipped
    /// entirely (zero-RAM-touch contract).  Cold shards drop their
    /// bloom on demote, so they always promote.  Empty `ext_terms`
    /// short-circuits to the Phase-3 always-promote behaviour.
    ///
    /// Shared verbatim by [`Self::ensure_warm_for_dispatch`] (which
    /// then executes the promotes) and the `warm_plan` RPC (which
    /// reports the set to external gates) — one computation, so the
    /// MCP cold-index gate can never contradict what dispatch will
    /// actually do (field 2026-08-24: the gate's tier-only view
    /// declared a bloom-skippable Parked drive "not ready" and
    /// force-promoted its body for a query the bloom had already
    /// answered).
    pub(crate) async fn promote_plan(
        &self,
        params_drives: &[uffs_mft::platform::DriveLetter],
        ext_terms: &[String],
    ) -> Vec<uffs_mft::platform::DriveLetter> {
        let guard = self.index.read().await;
        guard
            .iter()
            .filter(|shard| params_drives.is_empty() || params_drives.contains(&shard.drive))
            .filter(|shard| {
                matches!(
                    shard.state(),
                    crate::cache::ShardState::Parked | crate::cache::ShardState::Cold
                )
            })
            .filter(|shard| Self::bloom_pre_check_should_promote(shard, ext_terms))
            .map(|shard| shard.drive)
            .collect()
    }

    /// The `warm_plan` RPC body: the promote set the SAME search would
    /// trigger, derived through the same canonicalisation + filter
    /// resolution `run_search_over` performs before its ensure-warm
    /// call — bit-exact with dispatch by construction, because both
    /// funnel into [`Self::promote_plan`].
    pub(crate) async fn warm_plan(
        &self,
        params: &uffs_client::protocol::SearchParams,
    ) -> Vec<uffs_mft::platform::DriveLetter> {
        let mut effective = params.clone();
        effective.populate_canonical_fields();
        let filters = super::search_filters_build::build_search_filters(&effective);
        self.promote_plan(&effective.drives, &filters.extensions)
            .await
    }

    /// Phase 4 Commit F — bloom pre-check for Parked shards.
    ///
    /// Returns `true` when the shard must be promoted (the search
    /// might match a record there); `false` when the shard is
    /// provably empty for the supplied ext filter and can stay
    /// Parked.
    ///
    /// Decision matrix:
    ///
    /// * `ext_terms` empty → always promote (the bloom-skip pre-check only
    ///   applies to ext-filtered queries; substring queries never bloom-skip —
    ///   see `crate::search::bloom_skip` for the correctness contract).
    /// * Shard is Cold → always promote (bloom dropped on Parked → Cold
    ///   transition; the only way to recover is the full body load).
    /// * Shard is Parked + has `parked_body` → probe the bloom; emit
    ///   `shard.bloom.decision` with the outcome and source `"ensure_warm"`.
    /// * Shard is Parked + has no `parked_body` (defensive: torn tier
    ///   transition) → promote (preserves correctness; the subsequent full-body
    ///   load surfaces any real corruption).
    fn bloom_pre_check_should_promote(
        shard: &crate::cache::shard::ShardEntry,
        ext_terms: &[String],
    ) -> bool {
        if ext_terms.is_empty() {
            return true;
        }
        let Some(parked) = shard.parked_body() else {
            // Cold shard, or Parked with no parked_body (legacy /
            // defensive).  No bloom available to query → must
            // promote so the full search runs against fresh data.
            return true;
        };
        let decision =
            uffs_core::search::bloom_skip::decide_for_ext_filter(Some(&parked.bloom), ext_terms);
        let matched = decision.keep();
        tracing::debug!(
            target: "shard.bloom.decision",
            drive = %shard.drive,
            r#match = matched,
            terms = ?ext_terms,
            source = "ensure_warm",
            "bloom pre-check"
        );
        matched
    }
}
