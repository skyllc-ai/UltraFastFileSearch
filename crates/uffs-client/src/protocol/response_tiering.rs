// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Memory-tiering RPC wire types: `hibernate`, `preload`, `forget`, and
//! `status_drives`.
//!
//! Phase 8-A scaffolding.  These types describe the user-visible memory
//! cutover surface introduced by the
//! [memory-tiering plan](https://github.com/skyllc-ai/UltraFastFileSearch)
//! Phase 8 commit-level decomposition (sub-phases 8-A through 8-E).
//!
//! Sub-phase 8-A wires the protocol surface only — every method returns
//! [`super::ERR_NOT_IMPLEMENTED`] until the corresponding handler is
//! filled in by 8-B (`hibernate`), 8-C (`preload`), 8-D (`forget`), and
//! 8-E (`status_drives`).  Splitting the new types into this sibling
//! file keeps [`super::response`] under the workspace 800-LOC policy
//! ceiling without a file-size exemption — same precedent as
//! [`super::response_status`] for the daemon-state RPC cluster.
//!
//! All types serialise to / deserialise from JSON with `snake_case`
//! field names so the wire format reads naturally to operators
//! inspecting an RPC capture with `jq`.

use serde::{Deserialize, Serialize};

// ────────────────────────────────────────────────────────────────────────────
// hibernate
// ────────────────────────────────────────────────────────────────────────────

/// Parameters for the `hibernate` method.
///
/// Hibernating a drive demotes its shard straight to `Cold` (encrypted
/// cache on disk, zero RAM resident).
/// An empty [`Self::drives`] vector hibernates **every** loaded drive
/// — the typical operator action when freeing memory before a long
/// idle stretch.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HibernateParams {
    /// Specific drives to hibernate.  Empty vector = every loaded drive.
    #[serde(default)]
    pub drives: Vec<uffs_mft::platform::DriveLetter>,
}

/// Response for the `hibernate` method.
///
/// Each field reports the drives whose **pre-call** tier matched the
/// field name and that are now `Cold`.  Drives that were already `Cold`
/// (or absent from the registry) are reported in
/// [`Self::already_cold`] for completeness without inflating the
/// "actually demoted" counts an operator might log to a metrics
/// pipeline.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HibernateResponse {
    /// Drives whose pre-call tier was `Hot`.
    #[serde(default)]
    pub hot_demoted: Vec<uffs_mft::platform::DriveLetter>,
    /// Drives whose pre-call tier was `Warm`.
    #[serde(default)]
    pub warm_demoted: Vec<uffs_mft::platform::DriveLetter>,
    /// Drives whose pre-call tier was `Parked`.
    #[serde(default)]
    pub parked_demoted: Vec<uffs_mft::platform::DriveLetter>,
    /// Drives that were already `Cold` (or unknown to the registry).
    #[serde(default)]
    pub already_cold: Vec<uffs_mft::platform::DriveLetter>,
}

// ────────────────────────────────────────────────────────────────────────────
// preload
// ────────────────────────────────────────────────────────────────────────────

/// Default pin duration (minutes) when [`PreloadParams::pin_minutes`]
/// is `None`.
///
/// The 30-minute default mirrors the figure quoted in the
/// memory-tiering plan §5.1 sub-phase 8-C.  Operators can override via
/// the wire field; the daemon applies the default when the field is
/// absent or `None`.
pub const DEFAULT_PRELOAD_PIN_MINUTES: u32 = 30;

/// Parameters for the `preload` method.
///
/// Preloads one or more drives into the `Hot` tier and pins them
/// against the idle TTL demote for
/// [`Self::pin_minutes`] minutes.  An empty [`Self::drives`] vector is
/// a usage error — preload requires at least one explicit drive
/// letter.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreloadParams {
    /// Drives to promote to `Hot`.  Empty = invalid (the daemon returns
    /// [`super::ERR_INVALID_PARAMS`]).
    #[serde(default)]
    pub drives: Vec<uffs_mft::platform::DriveLetter>,
    /// Pin duration in minutes.  `None` ⇒ [`DEFAULT_PRELOAD_PIN_MINUTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_minutes: Option<u32>,
}

/// Response for the `preload` method.
///
/// Reports which drives were freshly promoted vs already `Hot` (the
/// pin still extends in the latter case), plus per-drive errors and
/// the absolute pin-expiry timestamp so the CLI can render an
/// "expires at HH:MM" hint without re-parsing the input.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreloadResponse {
    /// Drives that transitioned to `Hot` as part of this call.
    #[serde(default)]
    pub promoted: Vec<uffs_mft::platform::DriveLetter>,
    /// Drives that were already `Hot`.  Pin window is still extended
    /// to the new [`Self::pin_until_unix_ms`].
    #[serde(default)]
    pub already_hot: Vec<uffs_mft::platform::DriveLetter>,
    /// Per-drive errors, prefixed with the drive letter
    /// (`"Z: cache file missing"`).
    #[serde(default)]
    pub errors: Vec<String>,
    /// Pin expiry as a Unix-millis timestamp.  `0` when no drive was
    /// successfully pinned (every drive landed in [`Self::errors`]).
    #[serde(default)]
    pub pin_until_unix_ms: i64,
}

// ────────────────────────────────────────────────────────────────────────────
// forget
// ────────────────────────────────────────────────────────────────────────────

/// Parameters for the `forget` method.
///
/// Forgetting a drive removes its encrypted compact cache, bloom,
/// trie, and parked-cache files from disk and evicts the shard from
/// the registry.  The operation is idempotent on already-forgotten
/// drives (no error, freed-bytes contribution is `0`).
///
/// By default the handler refuses to forget a drive whose shard is
/// non-`Cold` (returns [`super::ERR_DRIVE_BUSY`]) — the operator must
/// `hibernate` the drive first.  Setting [`Self::force`] auto-runs the
/// hibernate step before deleting cache files.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForgetParams {
    /// Drives to forget.  Empty = invalid (the daemon returns
    /// [`super::ERR_INVALID_PARAMS`]).
    #[serde(default)]
    pub drives: Vec<uffs_mft::platform::DriveLetter>,
    /// Force-forget non-`Cold` drives by auto-hibernating first.
    /// Default `false` ⇒ refuse with [`super::ERR_DRIVE_BUSY`] so the
    /// side effect (memory drop) is explicit.
    #[serde(default)]
    pub force: bool,
}

/// Response for the `forget` method.
///
/// Reports which drives had their cache files deleted, which were
/// already absent (idempotent no-op), the cumulative freed-bytes
/// total, and any per-drive errors (e.g. permission denied on a cache
/// file unlink).
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForgetResponse {
    /// Drives whose cache files were deleted in this call.
    #[serde(default)]
    pub forgotten: Vec<uffs_mft::platform::DriveLetter>,
    /// Drives that had no cache files on disk (idempotent no-op).
    #[serde(default)]
    pub already_absent: Vec<uffs_mft::platform::DriveLetter>,
    /// Cumulative bytes freed across every successfully-forgotten drive.
    #[serde(default)]
    pub freed_bytes: u64,
    /// Per-drive errors, prefixed with the drive letter
    /// (`"Z: cache directory permission denied"`).
    #[serde(default)]
    pub errors: Vec<String>,
}

// ────────────────────────────────────────────────────────────────────────────
// status_drives
// ────────────────────────────────────────────────────────────────────────────

/// Parameters for the `status_drives` method.
///
/// No fields today — preserved as a unit struct so future filters
/// (e.g. `tier_filter: Option<ShardTier>`) can be added without a
/// wire-format break: serde serialises a fieldless struct as `{}`,
/// matching the JSON the daemon already accepts when callers omit
/// the `params` envelope entirely.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusDrivesParams;

/// Response for the `status_drives` method.
///
/// Returns one [`DriveTierStatus`] row per shard known to the
/// registry — including `Cold` shards (encrypted cache on disk, zero
/// RAM) which the legacy `status` RPC's `drives` field omits.
/// Sub-phase 8-E renders this into a CLI table; the bare `status`
/// single-line summary is preserved verbatim.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StatusDrivesResponse {
    /// Per-drive tier + telemetry rows, ordered by drive letter
    /// (ascending).
    #[serde(default)]
    pub drives: Vec<DriveTierStatus>,
}

/// Per-drive tier + telemetry snapshot, as surfaced by `status_drives`.
///
/// Fields are populated from the registry's authoritative
/// `ShardEntry` state plus the per-shard usage counters maintained by
/// the journal-loop telemetry.  See `crates/uffs-daemon/src/cache/`
/// for the daemon-side source of truth.
///
/// `Default` is intentionally not derived: `letter` is a typed
/// `DriveLetter` which has no canonical zero value, and no caller
/// currently constructs `DriveTierStatus` via `::default()`.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct DriveTierStatus {
    /// Drive letter.
    pub letter: uffs_mft::platform::DriveLetter,
    /// Current tier name (lowercase, matching
    /// [`super::response_status::ShardTier`]).
    pub tier: String,
    /// Resident bytes in memory (heap + mmap-resident if applicable).
    /// `0` for `Cold` and `Unknown` shards.
    #[serde(default)]
    pub resident_bytes: u64,
    /// Query rate (queries / minute, EWMA over the last 5 min window).
    /// `0.0` when no queries have hit this drive since daemon start.
    #[serde(default)]
    pub query_rate_per_min: f64,
    /// Unix-millis timestamp of the most recent query (`0` if never
    /// queried since daemon start).
    #[serde(default)]
    pub last_query_at_ms: i64,
    /// Cumulative count of promotions back from `Parked` or `Cold`
    /// since daemon start, by a search or a `preload` (each re-reads
    /// the drive's index from disk).  Daemons before v0.6.46 counted
    /// only `Cold → Hot`.
    #[serde(default)]
    pub promotions_total: u64,
    /// Pin expiry as Unix-millis.  `0` when the shard is not pinned
    /// (no `preload` in flight, or pin already elapsed).
    #[serde(default)]
    pub pin_until_unix_ms: i64,
    /// `true` while this drive's index is being paged back in from a
    /// demoted tier (a search or `preload` is waiting on it).  `tier`
    /// still names the pre-promote tier until the swap lands, so
    /// without this a drive mid-load reads as idle `parked`.  `false`
    /// from daemons before v0.6.46.
    #[serde(default)]
    pub loading: bool,
}
