// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 8-E — operator-facing per-drive tier + telemetry table.
//!
//! Sibling to [`super::drives`] (the legacy `drives` RPC, which
//! returns a slimmer `DriveInfo` row used by the bare `status` CLI).
//! `status_drives` widens the row to include pin state, query rate,
//! resident-bytes, and promotion counters so operators can decide
//! whether to `hibernate` / `preload` / `forget` a drive without
//! cross-referencing tracing logs.
//!
//! Three sources contribute to each row:
//!
//! * [`crate::cache::shard::ShardEntry`] — tier (`shard.state()`), in-memory
//!   body for the `resident_bytes` calculation, pin expiry
//!   (`pin_until_ms_value`).
//! * [`crate::cache::shard::DriveStats`] — query counters (`queries_total`),
//!   EMA-decayed query rate (`decay_ema_qpm`), `last_query_at_ms`.
//! * [`crate::cache::shard::ShardEntry::parked_body`] — bloom + trie
//!   `size_bytes()` for `Parked` shards (which have no `body` but still hold
//!   non-trivial RAM).
//!
//! The wire response is sorted by drive letter (case-insensitive
//! ascending) so the CLI table is stable across daemon
//! reconfigurations even when load order changes.

use uffs_client::protocol::response::{DriveTierStatus, StatusDrivesResponse};

use super::IndexManager;
use crate::cache::shard::ShardEntry;
use crate::cache::{ShardState, unix_now_ms};

impl IndexManager {
    /// Phase 8-E — build the per-drive tier + telemetry snapshot.
    ///
    /// Walks the registry under a single read-lock and builds one
    /// [`DriveTierStatus`] row per shard.  The `now_ms` clock used
    /// for the EMA decay is sampled once per call so every row in
    /// the same response shares the same wall-clock anchor.
    ///
    /// The output is sorted by drive letter (ASCII ascending) so the
    /// CLI table is stable across re-runs — load order is an
    /// implementation detail of the boot-time loader, not something
    /// operators want surfaced in their dashboards.
    pub(crate) async fn status_drives(&self) -> StatusDrivesResponse {
        let now_ms = unix_now_ms();
        // Read-lock the registry and produce the rows inside the
        // guard's lifetime, then `drop(guard)` before sorting so the
        // lock is released as quickly as possible (mirrors the
        // pattern used in [`super::drives::IndexManager::drives`]).
        // Letters with a promote in flight right now (same source as the
        // `drives` RPC's `loading` flag).
        let loading: Vec<uffs_mft::platform::DriveLetter> =
            self.in_flight_promotes.lock().map_or_else(
                |poisoned| poisoned.into_inner().keys().copied().collect(),
                |map| map.keys().copied().collect(),
            );
        let mut drives: Vec<DriveTierStatus> = {
            let guard = self.index.read().await;
            let rows: Vec<DriveTierStatus> = guard
                .iter()
                .map(|shard| build_row(shard, now_ms, loading.contains(&shard.drive)))
                .collect();
            drop(guard);
            rows
        };
        // `DriveLetter` is canonical uppercase, so sort-by-letter is
        // case-insensitive by construction.
        drives.sort_by_key(|row| row.letter);
        StatusDrivesResponse { drives }
    }
}

/// Build a single [`DriveTierStatus`] row for `shard`.
///
/// Pure read-only — never mutates the shard.  `now_ms` is the wall
/// clock anchor for the EMA decay (passed in from the caller so
/// every row in one `status_drives` response shares the same
/// timestamp).  `loading` says a promote for this drive is in flight.
fn build_row(shard: &ShardEntry, now_ms: u64, loading: bool) -> DriveTierStatus {
    let state = shard.state();
    let tier = shard_state_label(state).to_owned();
    let resident_bytes = resident_bytes_for(shard, state);
    let last_query_at_ms = shard.stats.last_query_at_ms();
    let pin_until_unix_ms = shard.pin_until_ms_value();

    // Decay the EMA against the response's anchor `now_ms`.  This
    // mutates the per-shard counter (sliding-window decay is
    // observation-driven), which is fine because we hold the
    // registry's read lock — concurrent searches go through
    // `mark_query_at` which uses the same `last_decay_ms` slot.
    let query_rate_per_min = shard.stats.decay_ema_qpm(now_ms);

    DriveTierStatus {
        letter: shard.drive,
        tier,
        resident_bytes,
        query_rate_per_min,
        last_query_at_ms: i64_from_u64_saturating(last_query_at_ms),
        // `promotions_total` reads the counter maintained by
        // [`crate::cache::shard::DriveStats::record_promote_from_demoted`]:
        // every promote back from `Parked` or `Cold`, by a search or a
        // `preload`; `0` for drives that have stayed Warm/Hot since
        // daemon start.
        promotions_total: shard.stats.promotions_total(),
        pin_until_unix_ms: i64_from_u64_saturating(pin_until_unix_ms),
        loading,
    }
}

/// Convert a `u64` Unix-millis to the wire format's `i64` shape,
/// saturating at [`i64::MAX`] for any value that would overflow.
///
/// In practice the registry's timestamps are monotonically derived
/// from [`std::time::SystemTime::duration_since`] and stay well
/// under [`i64::MAX`] (year 292 277 026 596 AD), so the saturating
/// branch is unreachable in production — but `try_from` is more
/// honest than a `as` truncation cast.
fn i64_from_u64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Compute `resident_bytes` for a shard given its tier.
///
/// * `Hot` / `Warm` → `body.heap_size_bytes().total` (the in-memory
///   `DriveCompactIndex` footprint — what an OOM trace would attribute to this
///   drive).
/// * `Parked` → `parked_body.size_bytes()` (the bloom + path trie we kept
///   across the demote so the search hot path can still answer "this drive
///   doesn't have anything matching" without a re-promote).
/// * `Cold` / `Unknown` / `Evicting` → `0` (the encrypted compact cache lives
///   on disk only).
fn resident_bytes_for(shard: &ShardEntry, state: ShardState) -> u64 {
    match state {
        ShardState::Hot | ShardState::Warm => shard.body().map_or(0, |body| {
            u64::try_from(body.heap_size_bytes().total).unwrap_or(u64::MAX)
        }),
        ShardState::Parked => shard
            .parked_body()
            .map_or(0, |pb| u64::try_from(pb.size_bytes()).unwrap_or(u64::MAX)),
        ShardState::Cold | ShardState::Unknown | ShardState::Evicting => 0,
    }
}

/// Lowercase tier label matching
/// [`uffs_client::protocol::response::ShardTier`]'s wire form.
///
/// The wire format documents lowercase strings (matching `serde`
/// rename-all = "lowercase") so the CLI can match on `"hot"` /
/// `"warm"` / etc. without colour-coding sprinkled through the
/// daemon side.
const fn shard_state_label(state: ShardState) -> &'static str {
    match state {
        ShardState::Unknown => "unknown",
        ShardState::Cold => "cold",
        ShardState::Parked => "parked",
        ShardState::Warm => "warm",
        ShardState::Hot => "hot",
        ShardState::Evicting => "evicting",
    }
}
