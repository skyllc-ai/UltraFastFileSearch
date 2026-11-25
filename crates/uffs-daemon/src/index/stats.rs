// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Telemetry surface for [`IndexManager`] (RPCs `stats` / `status`).
//!
//! Three closely-related accessors:
//!
//! * [`IndexManager::stats`] — query-rate / latency aggregates plus
//!   [`uffs_core::aggregate::AggregateCache::stats`] hit-rate counters.
//!   Returned by the `stats` RPC.
//! * [`IndexManager::status`] — broad daemon health snapshot (uptime,
//!   connections, PID, version) plus per-drive heap breakdown and the
//!   OS-reported RSS / mimalloc committed bytes from
//!   [`crate::telemetry::mem_snapshot`].  Returned by the `status` RPC.
//! * [`IndexManager::total_index_heap_bytes`] — the per-drive heap sum on its
//!   own, called by [`crate::telemetry::spawn_mem_snapshot_task`] for the
//!   `mem.snapshot` heartbeat trace.  Avoids the per-drive `Vec` allocation
//!   that [`IndexManager::status`] does.
//!
//! All three are read-only: they snapshot the registry once
//! and walk every loaded drive (Hot / Warm / Parked / Cold)
//! to compute the result.  No side effects, no concurrency
//! permits, no event emission — they're safe to call from
//! arbitrary RPC contexts.

use core::sync::atomic::Ordering;

use uffs_client::protocol::response::{StatsResponse, StatusResponse};

use super::IndexManager;

impl IndexManager {
    /// Get daemon performance statistics.
    #[expect(
        clippy::float_arithmetic,
        clippy::default_numeric_fallback,
        reason = "stats are approximate; f64 arithmetic needed for averages"
    )]
    pub(crate) async fn stats(&self) -> StatsResponse {
        let total_queries = self.queries_total.load(Ordering::Relaxed);
        let total_us = self.queries_total_us.load(Ordering::Relaxed);
        let startup_us = self.startup_duration_us.load(Ordering::Relaxed);
        let uptime_secs = self.start_time.elapsed().as_secs();
        let total_records = self.total_records().await;

        let avg_query_us = if total_queries > 0 {
            uffs_mft::u64_to_f64(total_us) / uffs_mft::u64_to_f64(total_queries)
        } else {
            0.0
        };
        let qps = if uptime_secs > 0 {
            uffs_mft::u64_to_f64(total_queries) / uffs_mft::u64_to_f64(uptime_secs)
        } else {
            0.0
        };

        let cache_stats = self.aggregate_cache.stats();

        StatsResponse {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            total_queries,
            total_query_time_us: total_us,
            avg_query_time_us: avg_query_us,
            startup_duration_ms: startup_us / 1000,
            uptime_secs,
            total_records,
            queries_per_second: qps,
            agg_cache_hits: cache_stats.hits,
            agg_cache_misses: cache_stats.misses,
            agg_cache_entries: u64::try_from(cache_stats.entries).unwrap_or(u64::MAX),
        }
    }

    /// Get current daemon status.
    ///
    /// Includes `has_drives` and `total_records` for completeness.
    ///
    /// # Concurrency
    ///
    /// Snapshots the `DaemonStatus` upfront via `.read().await.clone()`
    /// rather than holding the read guard across the inner awaits
    /// below.  Without the snapshot, the guard would be held across
    /// [`IndexManager::has_drives`], [`IndexManager::total_records`], and
    /// [`IndexManager::snapshot`] — three independent `self.index.read().await`
    /// acquisitions — blocking any concurrent
    /// [`IndexManager::set_ready`] / `set_loading_progress` /
    /// [`crate::index::IndexManager::refresh`] writer on
    /// `self.status` for the duration of the status RPC (which on a
    /// many-drive box with a slow snapshot path can be tens of
    /// milliseconds).  `DaemonStatus` is a small `Clone` enum
    /// (`Loading { usize, usize }` or `Refreshing { Vec<DriveLetter> }`
    /// in the worst case), so the snapshot is microseconds and never
    /// crosses an inner `.await`.  See Phase 10b audit findings.
    pub(crate) async fn status(&self, connections: usize) -> StatusResponse {
        let status_snapshot = self.status.read().await.clone();
        let loaded = self.has_drives().await;
        let records = self.total_records().await;
        tracing::trace!(
            has_drives = loaded,
            total_records = records,
            "Status queried"
        );

        // Collect per-drive memory breakdown.
        let snap = self.snapshot().await;
        let mut drive_memory = Vec::with_capacity(snap.drives.len());
        let mut total_index_heap: u64 = 0;
        for dr in &snap.drives {
            let hr = dr.heap_size_bytes();
            let heap = hr.total as u64;
            total_index_heap += heap;
            drive_memory.push(uffs_client::protocol::response::DriveMemoryInfo {
                drive: dr.letter,
                records: dr.records.len(),
                heap_bytes: heap,
                records_bytes: hr.records as u64,
                names_bytes: hr.names as u64,
                trigram_bytes: hr.trigram as u64,
                children_bytes: hr.children as u64,
                ext_index_bytes: hr.ext_index as u64,
            });
        }
        drop(snap);

        // Phase 0 of the memory-tiering work: surface the live
        // allocator-committed bytes and the OS-reported RSS alongside
        // the per-drive logical heap.  Cross-platform via mimalloc's
        // `mi_process_info`; see `crate::telemetry::mem_snapshot`.
        let (rss_bytes, mimalloc_committed_bytes) = crate::telemetry::mem_snapshot()
            .map_or((None, None), |mem| {
                (Some(mem.rss_bytes), mem.mimalloc_committed_bytes)
            });

        // Broker-adoption mode: reading via adopted broker handles (zero-UAC)
        // vs the daemon's own elevated handles. `is_elevated()` is `false` off
        // Windows; the handle count is `0` there, so both read as "direct".
        let elevated = uffs_mft::is_elevated();
        let reading_via_broker = uffs_mft::registered_broker_handle_count() > 0;

        StatusResponse {
            status: status_snapshot,
            uptime_secs: self.start_time.elapsed().as_secs(),
            connections,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            rss_bytes,
            index_heap_bytes: Some(total_index_heap),
            mimalloc_committed_bytes,
            drive_memory,
            git_sha: option_env!("UFFS_GIT_SHA").unwrap_or("unknown").to_owned(),
            elevated,
            reading_via_broker,
            live_update: crate::live_update::snapshot(),
            paths: Some(crate::live_update::daemon_paths()),
            launch_args: Some(std::env::args_os().skip(1).collect()),
        }
    }

    /// Sum of `DriveCompactIndex::heap_size_bytes()` across every
    /// loaded drive.
    ///
    /// Used by [`crate::telemetry::spawn_mem_snapshot_task`] to emit
    /// the `mem.snapshot` tracing event without going through the full
    /// [`IndexManager::status`] path (which builds a per-drive `Vec` we don't
    /// need for the heartbeat).
    pub(crate) async fn total_index_heap_bytes(&self) -> u64 {
        let snap = self.snapshot().await;
        let mut total: u64 = 0;
        for dr in &snap.drives {
            total = total.saturating_add(dr.heap_size_bytes().total as u64);
        }
        total
    }
}
