// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Shard-based index cache with per-drive lifecycle.
//!
//! Phase 1 of the memory-tiering work
//! (`docs/refactor/memory-tiering-implementation-plan.md`).
//!
//! The cache layer wraps each loaded `DriveCompactIndex` in a
//! [`crate::cache::shard::ShardEntry`] that carries:
//!
//! * a tier state ([`crate::cache::shard::ShardState`]) — Phase 1 pins
//!   everything to [`crate::cache::shard::ShardState::Warm`]; Phase 3 wires
//!   real transitions.
//! * per-drive query stats ([`crate::cache::shard::DriveStats`]) — atomic
//!   counters plus an exponentially-weighted moving average rate; consumed by
//!   the adaptive-TTL formulas in Phase 6.
//! * the in-memory body, an `Arc<DriveCompactIndex>` cloned cheaply into the
//!   per-search snapshot.
//!
//! [`crate::cache::registry::ShardRegistry`] is the top-level container that
//! the daemon swaps under `RwLock<Arc<...>>` in place of the old
//! `Arc<uffs_core::search::backend::DriveIndex>`.  It maintains a
//! cached `Arc<DriveIndex>` over the active (Warm/Hot) subset so the
//! search hot path stays an `Arc::clone` away from a usable backend.
//!
//! Phase 1 keeps every shard in `Warm` so the active subset always
//! matches the full registry; Phase 3 starts demoting and
//! [`crate::cache::registry::ShardRegistry::active_index`] begins to diverge
//! from the full shard list.

pub(crate) mod background_io;
pub(crate) mod body_loader;
pub(crate) mod cache_cleaner;
pub(crate) mod cursor_store;
pub(crate) mod guarded_load;
pub(crate) mod journal_loop;
pub(crate) mod journal_sink;
pub(crate) mod policy;
pub(crate) mod prefetch;
pub(crate) mod pressure;
pub(crate) mod promote_load;
pub(crate) mod registry;
pub(crate) mod shard;
pub(crate) mod working_set;

// Phase 1 surface: only `ShardRegistry` and `ShardState` are referenced
// from outside this module (in `crate::index`).  The other types
// (`ShardEntry`, `DriveStats`, `DriveStatsSnapshot`,
// `IllegalTransition`) stay accessible via the explicit `shard::` /
// `registry::` paths so the proptest harness in `shard.rs` and the
// integration tests in `crate::index::tests` can construct them
// without going through this re-export.  Phase 3+ widens the
// re-export list as more callers join.
pub(crate) use registry::ShardRegistry;
pub(crate) use shard::ShardState;

/// Current Unix-millis timestamp for the cache subsystem's clocks.
///
/// Single canonical clock function shared by:
///
/// * [`crate::index::IndexManager::record_search_dispatch`] — stamps every
///   Warm/Hot shard's `DriveStats::last_query_at_ms` on each dispatch.
/// * The Phase-3 demote controller (Commit D, in
///   [`crate::index::IndexManager::demote_idle_shards`]) — reads
///   `last_query_at_ms` to compute `idle_secs`.
///
/// Returns `0` when the system clock is set before 1970-01-01 so the
/// fallback matches the "never queried" sentinel
/// `DriveStats::last_query_at_ms == 0` used by the demote controller.
#[must_use]
pub(crate) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |dur| u64::try_from(dur.as_millis()).unwrap_or(u64::MAX))
}
