// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Index management: load drives, hold compact indices, refresh.
//!
//! The [`crate::index::IndexManager`] is the daemon's core data structure. It
//! holds the compact search indices for all loaded drives and delegates to
//! `uffs_core::search` for query execution.
//!
//! Each cluster of methods lives in its own sibling module — see the
//! `mod` declarations below.

mod aggregation;
mod constructors;
pub(crate) mod diff;
mod dispatch;
mod drives;
pub(crate) mod forget_drive;
mod hotload;
mod info;
mod journal;
pub(crate) mod journal_save;
mod loading;
mod physical_order;
mod predicates;
mod projection;
mod refresh;
pub(crate) mod search;
mod search_failure;
mod search_filters_build;
mod stats;
mod status_drives;
mod test_helpers;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod tiering_ops;
mod transitions;
mod wire_spec;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::time::Instant;

use futures::future::{BoxFuture, Shared};
use tokio::sync::{RwLock, Semaphore};
use uffs_client::protocol::response::DaemonStatus;
use uffs_core::aggregate::AggregateCache;
use uffs_core::search::backend::DriveIndex;

use crate::cache::ShardRegistry;
use crate::events::EventSender;

/// Type alias for the shared, awaitable in-flight body-load future.
///
/// Each entry is a [`futures::future::Shared`] over a boxed future
/// that loads + prefetches the body for a single drive letter.  N
/// concurrent callers all clone the same `Shared` and await it,
/// receiving the same `Option<Arc<DriveCompactIndex>>` outcome.
///
/// This is the core primitive for the per-letter single-flight
/// dedup that prevents the thundering-herd promote stampede observed
/// on Windows v0.5.83 (PR-e — see
/// `docs/refactor/promote-thundering-herd-fix.md`).
type InFlightLoad = Shared<BoxFuture<'static, Option<Arc<uffs_core::compact::DriveCompactIndex>>>>;

/// Slot map for in-flight body loads — one entry per drive letter.
///
/// Wrapped in a `std::sync::Mutex` (not `tokio::sync::Mutex`) because:
///
/// 1. The critical section is microscopic — a `HashMap` get / insert / remove
///    on a map bounded by the loaded-drive count (≤ 26 entries). No async work
///    is performed under the lock.
/// 2. The cleanup `Drop` path on a cancelled owner future needs to remove the
///    slot without an async runtime — `std::sync::Mutex` can be locked
///    synchronously where `tokio::sync::Mutex` would require `blocking_lock`
///    (panics inside an async runtime, see Tokio docs) or runtime-aware glue.
///
/// Poison handling matches the rest of the daemon (see
/// [`crate::lifecycle::LifecycleHandle::verify_shutdown_nonce`]): the
/// `HashMap` stores no invariants that need recovery, so we recover
/// the inner state via [`std::sync::PoisonError::into_inner`] rather
/// than panicking on poisoning.
type InFlightPromotes =
    Arc<StdMutex<std::collections::HashMap<uffs_mft::platform::DriveLetter, InFlightLoad>>>;

/// Per-drive load timing stored for profile reporting.
///
/// Field names omit the `_ms` suffix because the unit is documented
/// once here; all values are milliseconds (`u128`).
struct StoredDriveTiming {
    /// Compact-cache deserialization time (milliseconds, 0 if cache miss).
    cache: u128,
    /// MFT read time (milliseconds, 0 if cache hit).
    mft: u128,
    /// Compact index build time (milliseconds, 0 if cache hit).
    compact: u128,
    /// Trigram index build time (milliseconds, 0 if cache hit).
    trigram: u128,
}

/// Manages loaded drive indices and serves queries.
///
/// Concurrent queries clone the inner `Arc<DriveIndex>` under a read lock
/// (< 1 μs), then search the snapshot with no lock held.  Mutations
/// (load, refresh, remove) swap the `Arc` pointer under a write lock
/// (< 1 μs).  In-flight queries keep the old snapshot alive until they
/// finish.
pub(crate) struct IndexManager {
    /// Shared shard-registry snapshot.
    ///
    /// Read lock to clone the inner `Arc<ShardRegistry>` (< 1 μs);
    /// write lock only during load / refresh / remove (pointer swap,
    /// < 1 μs).  The registry caches an `Arc<DriveIndex>` over its
    /// active (Warm/Hot) subset so the search hot path stays one
    /// `Arc::clone` away from a usable backend — see
    /// [`IndexManager::snapshot`].
    ///
    /// Phase 1 of the memory-tiering work replaced the previous
    /// `Arc<DriveIndex>` field with `Arc<ShardRegistry>`; every shard
    /// is pinned to `Warm` so observable behavior is unchanged.  See
    /// `crate::cache` for the type layer.
    index: RwLock<Arc<ShardRegistry>>,
    /// Current daemon status.
    status: RwLock<DaemonStatus>,
    /// When the daemon started.
    start_time: Instant,
    /// Data directory for MFT files (Mac/Linux offline mode).
    data_dir: Option<PathBuf>,
    /// Event broadcaster — pushes notifications to all connected clients.
    events: EventSender,
    // ── Concurrency control ────────────────────────────────────────
    /// Limits simultaneous search operations to prevent rayon-pool
    /// oversubscription during aggregate queries.
    ///
    /// Every search fans out across the loaded drives via
    /// `drives.par_iter()` in `uffs-core`.  On a box with `C` CPU cores
    /// and `D` loaded drives, admitting `K` concurrent searches spawns
    /// `K × D` rayon tasks onto a `C`-thread pool.  Once `K × D > C`
    /// the work-stealing scheduler spends significant time on pair-
    /// merge coordination rather than compute — measured at ~9.7×
    /// per-query slowdown at `K × D / C = 7×` oversubscription (24
    /// concurrent queries × 7 drives on 24 cores, Windows validation
    /// Run 7 of the 2026-04-18 healing log).
    ///
    /// Sizing: we target `max(2, (cpus × 26) / (drives × 10))` permits
    /// by default so the product `permits × drives ≈ 2.6 × cpus`, the
    /// empirically-best oversubscription on multi-drive boxes (see
    /// [`IndexManager::auto_concurrency_target`] for the measurement that
    /// landed on the 2.6× factor).  The `UFFS_SEARCH_MAX_CONCURRENCY`
    /// env var overrides the formula for benchmark sweeps or for
    /// operators who want to clamp down on oversubscription.  The
    /// semaphore is *replaced* (not mutated) when drive count changes
    /// via [`IndexManager::tune_concurrency`]; in-flight queries hold owned
    /// permits on the pre-swap instance and finish naturally.
    search_semaphore: RwLock<Arc<Semaphore>>,
    /// Cached CPU count for the concurrency formula.  Captured once at
    /// construction so repeated tuning calls are cheap.
    cpus: usize,
    // ── Aggregate result cache ────────────────────────────────────
    /// Shared cache of recent `AggregateOutput` values.
    ///
    /// Populated on every aggregate miss, consulted on every call;
    /// invalidated wholesale whenever `index_version` is bumped.
    /// Default TTL is 60 s (see `AggregateCache::default_ttl`).
    aggregate_cache: Arc<AggregateCache>,
    /// Monotonic index-generation counter.
    ///
    /// Incremented on every drive mutation (add, replace, hot-load) so
    /// the aggregate cache can invalidate stale entries via
    /// `AggregateCache::set_index_version`.  Using `Relaxed` ordering
    /// is sufficient: the value is only read as a cache-invalidation
    /// token, never to gate memory visibility of other fields (the
    /// index `Arc` swap handles that independently).
    index_version: AtomicU64,
    // ── Performance counters ────────────────────────────────────────
    /// Total search queries served.
    queries_total: AtomicU64,
    /// Cumulative search time in microseconds.
    queries_total_us: AtomicU64,
    /// Duration from daemon start to `Ready` (microseconds, set once).
    startup_duration_us: AtomicU64,
    /// Per-drive load timing for `--profile` reporting.
    drive_timings:
        RwLock<std::collections::HashMap<uffs_mft::platform::DriveLetter, StoredDriveTiming>>,
    /// Device path (VSS snapshot, via `--device PATH=LETTER`) each loaded
    /// drive was actually read from, if any. A drive absent from this map
    /// was loaded from its live volume. Needed by
    /// [`Self::run_search_over`]'s optional physical-location-ordering
    /// step: `VolumeHandle::get_mft_extents` requires opening the *same*
    /// device the index came from, or it silently returns the wrong
    /// volume's layout (see that method's own doc comment in `uffs-mft`).
    #[cfg_attr(
        not(windows),
        expect(
            dead_code,
            reason = "only read by physical_order's Windows-only reorder_impl"
        )
    )]
    device_paths: RwLock<std::collections::HashMap<uffs_mft::platform::DriveLetter, String>>,
    /// Source for `Parked` / `Cold` shard bodies during
    /// promote-on-search.  Production paths use
    /// [`crate::cache::body_loader::DiskBodyLoader`]; the
    /// Commit-E integration tests inject fakes via
    /// `IndexManager::with_body_loader_for_test`.
    body_loader: Arc<dyn crate::cache::body_loader::BodyLoader>,
    /// Process-level working-set trim hook (Phase 5 task 5.1).
    /// Called once at the end of every demote batch in
    /// [`IndexManager::demote_idle_shards`] (task 5.4).  Production wires
    /// [`crate::cache::working_set::PlatformWorkingSetTrim`]
    /// (Mac/Linux no-op, Windows `EmptyWorkingSet`); the Phase 5
    /// tests inject
    /// `crate::cache::working_set::tests::CountingWorkingSetTrim`
    /// to assert exactly-once invocation per batch.
    working_set_trim: Arc<dyn crate::cache::working_set::WorkingSetTrim>,
    /// Region kernel-prefetch hook (Phase 5 task 5.2).  Called
    /// inside the per-letter `spawn_blocking` task in
    /// [`IndexManager::ensure_warm_for_dispatch`] right after the body
    /// loader returns the freshly-loaded body (task 5.5), so the
    /// kernel can start paging in records + names while the
    /// orchestrator acquires the registry write-lock.  Production
    /// wires [`crate::cache::prefetch::PlatformPrefetch`] (Windows
    /// `PrefetchVirtualMemory`, Mac/Linux `posix_madvise`); the
    /// Phase 5 tests inject
    /// `crate::cache::prefetch::tests::RecordingPrefetch` to assert the
    /// records + names regions reach the kernel.
    prefetch: Arc<dyn crate::cache::prefetch::Prefetch>,
    /// Memory-pressure signal source (Phase 5 task 5.3).  Held so
    /// the daemon's `spawn_pressure_subscriber` (in `lib.rs`) can
    /// call [`IndexManager::subscribe_pressure`] to obtain a
    /// [`tokio::sync::watch::Receiver`] and log every transition.
    /// Nothing is demoted on pressure (owner ruling 2026-10-03).
    /// Production wires
    /// [`crate::cache::pressure::PlatformPressureSignal`] (Mac/Linux
    /// never-fires, Windows kernel watcher thread); the lifecycle
    /// tests inject
    /// `crate::cache::pressure::tests::ControllablePressureSignal`
    /// to broadcast deterministic transitions and assert the
    /// observe-only contract.
    pressure: Arc<dyn crate::cache::pressure::PressureSignal>,
    /// Thread-level background-I/O priority hook (Phase 5 task 5.7).
    /// Held so [`IndexManager::handle_journal_refresh`] can wrap the
    /// per-letter `tokio::task::spawn_blocking` closure in a
    /// [`crate::cache::background_io::BackgroundIoScope`] so the USN
    /// catch-up + encrypted-cache write happen at Windows
    /// `THREAD_MODE_BACKGROUND_BEGIN` priority — yielding to any
    /// foreground RPC handler under disk contention.  Production
    /// wires [`crate::cache::background_io::PlatformBackgroundIoPriority`]
    /// (no-op on Mac/Linux, `SetThreadPriority` on Windows); the
    /// Phase 5 unit tests inject
    /// `crate::cache::background_io::tests::CountingBackgroundIoPriority`
    /// to assert the begin/end pair fires exactly once per refresh
    /// closure.
    ///
    /// Phase 7 activation moved the call site from the deleted
    /// `refresh_usn_for_warm_shards` global tick to
    /// [`IndexManager::handle_journal_refresh`] (per-shard, threshold-driven).
    background_io: Arc<dyn crate::cache::background_io::BackgroundIoPriority>,
    /// Per-drive cache-file cleanup hook (Phase 8-D `forget` RPC).
    ///
    /// Called from [`crate::index::IndexManager::forget_drives`] after the
    /// in-memory shard has been evicted from the registry, to delete every
    /// per-drive on-disk artefact (encrypted compact body, USN
    /// cursor, MFT index, lock file).  Production wires
    /// [`crate::cache::cache_cleaner::PlatformCacheCleaner`] which
    /// resolves the canonical cache paths via
    /// [`uffs_core::compact_cache`] / [`uffs_mft::cache`] and
    /// unlinks each via [`std::fs::remove_file`]; tests inject
    /// `crate::cache::cache_cleaner::CountingCacheCleaner` so
    /// registry-eviction behaviour can be verified deterministically
    /// without ever touching the host's cache directory.
    cache_cleaner: Arc<dyn crate::cache::cache_cleaner::CacheCleaner>,
    /// Per-letter single-flight dedup for body loads — PR-e fix
    /// for the thundering-herd promote stampede observed on
    /// Windows v0.5.83 MCP-validation soak (see
    /// `docs/refactor/promote-thundering-herd-fix.md`).
    ///
    /// When N concurrent search dispatches all observe the same
    /// drive as Parked, the first one installs a
    /// [`futures::future::Shared`] over the body-load future; the
    /// rest find the existing entry and clone-and-await the same
    /// future, receiving the same outcome.  After the load
    /// completes, a dedicated cleanup task — spawned at install
    /// time and independent of any awaiter's lifetime — removes
    /// the slot so the next Parked → Warm cycle starts a fresh
    /// load (preserves USN-refresh freshness).
    ///
    /// Pre-fix RAM math (Windows v0.5.83 storm window): 8 × 1.3 GB
    /// transient × 4 drives ≈ 32 GB peak.  Post-fix: 1 × 1.3 GB
    /// per Parked drive in flight at any moment, ≈ 2 GB peak for
    /// the same workload.  See [`IndexManager::load_or_join_in_flight`].
    in_flight_promotes: InFlightPromotes,
    /// Phase 7 activation: per-letter [`JournalLoopHandle`] map.
    ///
    /// Populated by [`IndexManager::attach_journal_handle`] from the
    /// per-shard journal-loop spawn site
    /// (`lib.rs::spawn_journal_loops_for_warm_shards`) after each
    /// [`crate::cache::journal_loop::spawn_journal_loop`] call.
    ///
    /// On daemon shutdown the field's `Drop` cancels nothing
    /// explicitly — the per-loop [`watch::Sender`] inside each
    /// handle is dropped when the map is dropped, which signals
    /// every loop's `cancel_rx.changed()` arm and the loop exits
    /// on its next iteration (within one
    /// [`crate::cache::journal_loop::JournalLoopConfig::poll_interval`]).
    /// A future graceful-shutdown commit may add an explicit
    /// drain-and-parallel-cancel surface here once the daemon's
    /// other fire-and-forget background tasks (idle-demote,
    /// pressure subscriber, mem-snapshot) get a coordinated
    /// teardown path.
    ///
    /// Wrapped in [`std::sync::Mutex`] (not [`tokio::sync::Mutex`])
    /// because the critical section is microscopic — a
    /// [`std::collections::HashMap::insert`] on a map bounded by
    /// the loaded-drive count (≤ 26 entries).  Mirrors the
    /// [`IndexManager::in_flight_promotes`] field's lock-choice rationale
    /// immediately above.
    ///
    /// Poison handling matches [`IndexManager::in_flight_promotes`] (the
    /// `HashMap` stores no invariants that need recovery, so we
    /// recover the inner state via [`std::sync::PoisonError::into_inner`]
    /// rather than panicking on poisoning).
    ///
    /// [`JournalLoopHandle`]: crate::cache::journal_loop::JournalLoopHandle
    /// [`watch::Sender`]: tokio::sync::watch::Sender
    journal_handles: Arc<
        StdMutex<
            std::collections::HashMap<
                uffs_mft::platform::DriveLetter,
                crate::cache::journal_loop::JournalLoopHandle,
            >,
        >,
    >,
    /// Parsed `daemon.toml` (Phase 6).  Loaded once at
    /// [`crate::run_daemon`] startup via
    /// [`crate::config::Config::load_default`] and shared across
    /// every controller — read by [`IndexManager::demote_idle_shards`] for
    /// the per-drive [`TierThresholds`] sizing + per-drive
    /// `min_tier` clamp (plan tasks 6.1, 6.3, 6.6, 6.7).
    ///
    /// `Arc` for cheap clone into the demote / pressure / USN
    /// controllers; the config is immutable post-load so no
    /// interior-mutability cell is needed.  Callers that don't
    /// need the live `daemon.toml` (test paths) compose
    /// `Arc::new(crate::config::Config::default())` which makes
    /// the controller behave identically to the Phase-3 static
    /// ladder (plan task 6.8 contract — pinned by the unit
    /// tests in `crate::config::tests`).
    ///
    /// [`TierThresholds`]: crate::cache::policy::TierThresholds
    config: Arc<crate::config::Config>,
}

impl IndexManager {
    // Constructors live in `super::constructors` — see that module
    // for `new`, `new_with_config`, `new_with_lifecycle_hooks`, and
    // the three `_for_test` variants.

    /// Acquire an owned search-concurrency permit.
    ///
    /// Returns `None` if the semaphore was closed (daemon shutting
    /// down).  The permit is tied to the semaphore instance that was
    /// current at acquisition time — if [`IndexManager::tune_concurrency`]
    /// swaps the semaphore while this permit is outstanding, the old
    /// instance stays alive until the permit is dropped, so in-flight
    /// queries always see a consistent admission slot.
    pub(crate) async fn acquire_search_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let sem = Arc::clone(&*self.search_semaphore.read().await);
        sem.acquire_owned().await.ok()
    }

    /// Environment variable that overrides the auto-tuned search-permit
    /// count.
    ///
    /// Accepts any positive `usize`.  Invalid or empty values are
    /// ignored and the auto-tuned default is used instead.  Applied
    /// every time [`IndexManager::tune_concurrency`] runs, so the daemon can
    /// be re-tuned at runtime by setting the env var and invoking an
    /// operation that re-tunes (e.g. a refresh).  Typical use is to
    /// set it before `uffs --daemon start` for benchmark sweeps:
    ///
    /// ```text
    /// UFFS_SEARCH_MAX_CONCURRENCY=12 uffs --daemon start
    /// ```
    const SEARCH_CONCURRENCY_ENV: &'static str = "UFFS_SEARCH_MAX_CONCURRENCY";

    /// Compute the auto-tuned search-permit target for a given `(cpus,
    /// drives)` topology.
    ///
    /// **Formula**: `max(2, (cpus × 26) / (drives × 10))`.
    ///
    /// That is roughly `2.6 × cpus / drives` in closed form, i.e. **30 %
    /// more permits** than the simpler `2 × cpus / drives` heuristic used
    /// through v0.5.45.  The extra 30 % was calibrated empirically against
    /// the api-validation harness on a 24 CPU × 7 drive Windows box
    /// (2026-04-18 sweep in `LOG/Output`):
    ///
    /// | permits | wall  | avg per-test | slowest |
    /// |--------:|------:|-------------:|--------:|
    /// |   6 (`2×cpus/drives`)  | 21.7 s |  1318 ms | 2792 ms |
    /// | **8 (`2.6×cpus/drives`)** | **12.0 s** | **560 ms** | **1395 ms** |
    /// |  12 | 12.0 s |  629 ms | 1430 ms |
    /// |  16 | 12.6 s |  688 ms | 1491 ms |
    /// |  24 | 11.2 s |  689 ms | 1427 ms |
    ///
    /// The `2 × cpus` heuristic left ~45 % of throughput on the table; at
    /// `2.6 × cpus` wall time collapsed without meaningfully growing per-
    /// query latency (avg-query went from 280 ms to 326 ms — ~16 %).
    /// Beyond that, returns diminished sharply and rayon oversubscription
    /// began showing through as per-query slowdown.
    ///
    /// The integer expression `(cpus × 26) / (drives × 10)` is used to
    /// keep the formula deterministic, auditable, and free of floating-
    /// point rounding surprises — easy to reason about in tests and in
    /// the retune log line.
    ///
    /// **Floor of 2** keeps the daemon responsive on single-drive boxes
    /// and on machines with an unusually large number of drives (where
    /// the raw ratio can round down to 1 or 0).
    ///
    /// The `UFFS_SEARCH_MAX_CONCURRENCY` env var still overrides this
    /// computation directly — see [`IndexManager::tune_concurrency`].
    #[must_use]
    pub(crate) const fn auto_concurrency_target(cpus: usize, drives: usize) -> usize {
        // Clamp drives=0 → 1 so the pre-load admission window (before any
        // drive has registered) still returns a usable target instead of
        // dividing by zero.  Rename vs. the parameter so we don't trip
        // `clippy::shadow_reuse`.
        let effective_drives = if drives == 0 { 1 } else { drives };
        let numerator = cpus.saturating_mul(26);
        let denominator = effective_drives.saturating_mul(10);
        // `max(2, numerator / denominator)` written out because
        // `Ord::max` is not `const` on stable.
        let raw = numerator / denominator;
        if raw < 2 { 2 } else { raw }
    }

    /// Re-size the search semaphore to match the currently loaded
    /// drive count.
    ///
    /// **Default formula**: see [`IndexManager::auto_concurrency_target`] —
    /// roughly `max(2, 2.6 × cpus / drives)`.  The 30 % oversubscription
    /// vs. the simpler `2 × cpus / drives` lets the work-stealing
    /// scheduler chew through concurrent queries without serialising on
    /// the semaphore — measured 45 % wall-time improvement on 24×7
    /// Windows with only 16 % per-query latency cost.
    ///
    /// **Override**: the `UFFS_SEARCH_MAX_CONCURRENCY` environment
    /// variable, when set to a positive integer, short-circuits the
    /// formula and uses that value directly.  This is the intended
    /// knob for benchmark sweeps — no rebuild required.
    ///
    /// **Implementation**: the current `Arc<Semaphore>` is swapped
    /// out for a fresh one with the new permit count.  In-flight
    /// queries keep the pre-swap `Arc` alive via their owned permits
    /// and finish on the old instance; new queries acquire on the
    /// new one.  One allocation and one pointer swap per drive-count
    /// change; avoids the "forget-debt" bookkeeping that
    /// [`Semaphore::forget_permits`] would require when in-flight
    /// queries outnumber the target permit count.
    pub(crate) async fn tune_concurrency(&self) {
        let drive_count = self.snapshot().await.drives.len().max(1);
        let auto_target = Self::auto_concurrency_target(self.cpus, drive_count);

        let (target, source) = std::env::var(Self::SEARCH_CONCURRENCY_ENV)
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .map_or((auto_target, "auto"), |n| (n, "env"));

        let mut slot = self.search_semaphore.write().await;
        let previous_permits = slot.available_permits();
        *slot = Arc::new(Semaphore::new(target));
        drop(slot);
        tracing::info!(
            cpus = self.cpus,
            drives = drive_count,
            auto_target,
            target,
            source,
            previous_permits,
            env_var = Self::SEARCH_CONCURRENCY_ENV,
            "search concurrency retuned"
        );
    }

    /// Get a reference to the event sender (for IPC and lifecycle integration).
    pub(crate) const fn event_sender(&self) -> &EventSender {
        &self.events
    }

    // Drive loading + atomic registry mutations (`load_from_data_dir`,
    // `load_live_drives` Windows-only, `set_ready`, `add_drive`,
    // `replace_drive`, and the helper functions they delegate to)
    // live in `super::loading` — see that module.

    /// Shared reference to the aggregate cache.
    pub(crate) fn aggregate_cache(&self) -> &AggregateCache {
        &self.aggregate_cache
    }

    /// Increment `index_version` and notify the aggregate cache so it
    /// drops entries computed against the previous generation.
    ///
    /// Called from every drive-mutating path ([`IndexManager::add_drive`] and
    /// [`IndexManager::replace_drive`]).  Cheap: one atomic fetch-add plus a
    /// single `Mutex::lock` inside the cache.
    fn bump_index_version(&self) {
        let new_version = self.index_version.fetch_add(1, Ordering::Relaxed) + 1;
        self.aggregate_cache.set_index_version(new_version);
    }

    /// Snapshot the current `DriveIndex` over the active (Warm/Hot)
    /// shard subset.
    ///
    /// Two `Arc::clone`s under the read lock (registry, then its
    /// pre-cached active index) — no per-search rebuild.  Callers run
    /// the search against the returned `Arc` with no lock held.  Phase
    /// 1 keeps every shard `Warm` so the active subset always equals
    /// the full loaded set; Phase 3+ this return value shrinks below
    /// the registry as shards demote.
    async fn snapshot(&self) -> Arc<DriveIndex> {
        let guard = self.index.read().await;
        guard.active_index()
    }

    /// Get the configured data directory, if any.
    #[must_use]
    pub(crate) fn data_dir(&self) -> Option<&std::path::Path> {
        self.data_dir.as_deref()
    }

    /// Check if the daemon has any loaded drives.
    pub(crate) async fn has_drives(&self) -> bool {
        let guard = self.index.read().await;
        !guard.is_empty()
    }

    /// Total records across all active (Warm/Hot) drives.
    ///
    /// Phase 1 keeps every shard `Warm`, so this is identical to the
    /// pre-Phase-1 "records across every loaded drive" count.  Phase
    /// 3+ when shards demote, this returns only the records that are
    /// actually searchable right now.
    pub(crate) async fn total_records(&self) -> usize {
        let guard = self.index.read().await;
        guard.active_index().total_records()
    }

    /// Return the set of currently loaded drive letters.
    ///
    /// Includes every shard regardless of tier (Warm, Hot, Parked,
    /// Cold).  Pre-Phase-3 this matched the active-index drive list
    /// exactly; post-Phase-3 it can include shards whose body has been
    /// dropped.
    pub(crate) async fn loaded_drive_letters(&self) -> Vec<uffs_mft::platform::DriveLetter> {
        let guard = self.index.read().await;
        guard.loaded_letters()
    }
}

// ── Allocator page release ──────────────────────────────────────────────

/// Ask the system allocator to return freed pages to the OS.
///
/// After a large allocation+free cycle (e.g. `MftIndex` → drop), the
/// system allocator retains committed virtual memory.  This function
/// issues a best-effort request to reclaim those pages so the process
/// RSS reflects actual usage.
///
/// Uses `mi_collect(true)` (mimalloc) which aggressively decommits freed
/// pages.  This replaces the previous `HeapCompact` / `malloc_trim` calls
/// which only work with the system allocator — since we now use mimalloc as
/// `#[global_allocator]`, those had no effect.
fn release_allocator_pages() {
    mi_collect_force();
    tracing::debug!("mi_collect(true) completed");
}

/// Call `mi_collect(true)` to aggressively decommit freed mimalloc segments.
#[expect(unsafe_code, reason = "FFI call to mimalloc's mi_collect")]
fn mi_collect_force() {
    // SAFETY: `mi_collect(true)` only decommits unreferenced mimalloc
    // segments.  No allocated data is affected.
    unsafe {
        libmimalloc_sys::mi_collect(true);
    }
}
