// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Daemon-state wire types: `drives`, `status`, and `stats` RPC responses.
//!
//! Split out of [`super::response`] so the search/payload subset of the
//! protocol surface stays under the workspace 800-LOC policy ceiling
//! without a file-size exemption.  Functionally these types form one
//! cohesive group — they describe the *daemon's own state* (loaded
//! drives, runtime telemetry, lifecycle phase, perf counters) — as
//! opposed to query results, which live alongside `SearchResponse` in
//! `response.rs`.
//!
//! The original module re-exports each type via `pub use`, so external
//! callers continue to import from `uffs_client::protocol::response::*`
//! exactly as before — the split is internal layout, not a breaking
//! change to the public protocol surface.

use serde::{Deserialize, Serialize};

/// Response for the `drives` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct DrivesResponse {
    /// Loaded drives with record counts.
    pub drives: Vec<DriveInfo>,
}

/// Response for the `warm_plan` method (v0.6.38+).
///
/// Carries the drives the daemon's dispatch would actually promote for
/// the given search params — Parked/Cold shards in scope whose bloom
/// cannot prove them irrelevant to the query's resolved extension
/// filter.
///
/// This is the daemon-authoritative answer external cold-index gates
/// must consume instead of inferring readiness from `DriveInfo::tier`
/// alone: a Parked drive whose bloom proves "no matching extensions"
/// serves the query instantly with zero rows and MUST NOT be gated or
/// force-warmed (field 2026-08-24: the MCP gate's tier-only view
/// reported such a drive "warming — retry" and paged its body back
/// into RAM for a query the bloom had already answered).
#[derive(Debug, Serialize, Deserialize)]
pub struct WarmPlanResponse {
    /// Drives the search would promote, in registry order.  Empty
    /// means the query dispatches without any re-warm — the caller's
    /// gate must pass.
    pub needs_promote: Vec<uffs_mft::platform::DriveLetter>,
}

/// Memory-tiering state of a single shard, as surfaced over the wire.
///
/// Mirrors the daemon-internal `ShardState` enum (`crates/uffs-daemon/
/// src/cache/shard.rs`) so external observers — the CLI status
/// formatter, MCP, third-party tooling — can render the tier marker
/// without depending on daemon internals.  Wire format is the
/// lowercase tier name (`"hot"`, `"warm"`, `"parked"`, `"cold"`,
/// `"unknown"`, `"evicting"`).
///
/// Added in Phase 5 (memory-tiering implementation plan §3 task
/// 5.11) to fix the dogfood-discovered display gap where the status
/// RPC enumerated only Warm/Hot shards and printed `Drives: (none
/// loaded)` when every shard was Parked.  Older daemons (pre-v0.5.82)
/// did not populate this field; the CLI treats `None` as "not
/// reported" and falls back to the legacy format.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShardTier {
    /// Just discovered; no body, no bloom, no stats. Pre-load.
    Unknown,
    /// Encrypted cache exists but nothing in RAM.
    Cold,
    /// Bloom + trie loaded; full body dropped.
    Parked,
    /// Body fully loaded and searchable.
    Warm,
    /// Body loaded + pre-faulted via `Prefetch::hint`. Recent activity.
    Hot,
    /// Demote in progress. Transient.
    Evicting,
}

/// Information about a loaded drive.
#[derive(Debug, Serialize, Deserialize)]
pub struct DriveInfo {
    /// Drive letter.
    pub letter: uffs_mft::platform::DriveLetter,
    /// Number of records in the compact index.  `0` for `Parked` /
    /// `Cold` shards whose body has been released.
    pub records: usize,
    /// Source (e.g. `"cache"`, `"live"`, `"mft_file"`, `"parked"`,
    /// `"cold"` for tier-demoted shards).
    pub source: String,
    /// Memory-tiering state of this shard, populated by the daemon
    /// from the registry's authoritative `ShardEntry::state()`.  Older
    /// daemons (pre-v0.5.82) did not set this; the CLI treats `None`
    /// as "no tier marker available" and renders accordingly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<ShardTier>,
    /// Records this drive held the last time it was warm — the
    /// denominator for re-warm progress.
    ///
    /// `records` above is `0` for a demoted shard, so on its own it
    /// cannot say how far a re-warm has to go.  Drive-count progress
    /// is not a substitute: four of seven drives warm was only 24 % of
    /// the records, because the three still cold were the large ones.
    /// `0` (or absent, from an older daemon) means this drive has not
    /// been warm in the current daemon's lifetime, so no denominator
    /// exists yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_when_warm: Option<u64>,
    /// `true` while this drive's body is actively being paged in.
    ///
    /// A re-warm is stepwise per drive, so the record count plateaus
    /// for tens of seconds while one large drive loads.  Without this,
    /// repeated polls showing identical numbers are indistinguishable
    /// from a stalled warm — and giving up is the natural response to
    /// what looks like no progress.  Absent from an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loading: Option<bool>,
}

/// Response for the `status` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Current daemon status.
    pub status: DaemonStatus,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Number of active connections.
    pub connections: usize,
    /// Daemon process ID.
    pub pid: u32,
    /// Compile-time version of the daemon binary (`env!("CARGO_PKG_VERSION")`).
    ///
    /// Surfaced to the CLI so `uffs --daemon status` / `uffs --status` can
    /// flag a daemon-vs-CLI version mismatch when the user has an old
    /// long-running daemon and an upgraded CLI binary (or vice versa).
    /// Defaults to `""` for back-compat with pre-0.5.79 daemons that
    /// did not populate this field; the CLI renders that as `<unknown>`.
    #[serde(default)]
    pub version: String,
    /// Process RSS (resident set size) in bytes, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Calculated heap footprint of all loaded indices (bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_heap_bytes: Option<u64>,
    /// Mimalloc allocator committed bytes (bytes paged in from the OS).
    ///
    /// Reported by `mi_process_info`; equals or exceeds `index_heap_bytes`
    /// because the allocator carries page-level overhead and free-but-
    /// not-yet-decommitted segments.  Comparing this to `rss_bytes` shows
    /// how much of the daemon's RSS is allocator-managed.  Phase 0 of the
    /// memory-tiering work surfaces this so subsequent phases can be
    /// measured against a stable allocator-committed baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mimalloc_committed_bytes: Option<u64>,
    /// Per-drive memory breakdown (drive letter → heap bytes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drive_memory: Vec<DriveMemoryInfo>,
    /// Short git commit the running daemon was built from (`UFFS_GIT_SHA`,
    /// `-dirty` for a modified tree). Lets an operator confirm a rebuilt binary
    /// actually took effect — the semver alone can't distinguish two builds of
    /// the same version. `""`/`"unknown"` on daemons that predate this field.
    #[serde(default)]
    pub git_sha: String,
    /// Whether the daemon process is elevated (Administrator). When `false` the
    /// daemon reads the live MFT through the Access Broker's duplicated handle;
    /// see [`Self::reading_via_broker`].
    #[serde(default)]
    pub elevated: bool,
    /// Whether the daemon is reading the live MFT via **adopted Access Broker
    /// handles** (the zero-UAC path) rather than its own elevated handles.
    /// Always `false` off Windows / for offline-MFT sources.
    #[serde(default)]
    pub reading_via_broker: bool,
    /// Live-update (USN journal) state — how many per-shard journal loops are
    /// running. Absent on daemons that predate this field or platforms without
    /// live update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_update: Option<LiveUpdateInfo>,
    /// Filesystem locations the daemon is using (data/cache dir, socket/pipe,
    /// log dir), so the operator can find the index, connect, and read logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<DaemonPaths>,
    /// The command-line arguments the daemon process was started with
    /// (after the program name), exactly as the OS handed them over.
    ///
    /// `uffs --daemon restart` starts the new daemon with these, so a
    /// restart keeps every launch flag (`--drive`, `--data-dir`,
    /// `--no-cache`, `--log-level`, `--log-file`, `--no-retire`, ...)
    /// instead of rebuilding a partial command line from the loaded drives.
    /// `OsString` so a path that is not UTF-8 survives the round trip
    /// (client and daemon run on the same machine). Absent from daemons
    /// that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_args: Option<Vec<std::ffi::OsString>>,
}

/// Live-update (USN journal) liveness, surfaced in [`StatusResponse`].
///
/// Deliberately just the loop count: it is set **once when the loops spawn**,
/// so reporting it costs nothing on the live-indexing hot path (unlike a
/// per-patch "last applied" timestamp, which would add a store to every USN
/// apply). "N loops running" answers the operative question — *is live update
/// actually active?* — which is what an operator needs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct LiveUpdateInfo {
    /// Number of per-shard journal loops currently running (0 = live update
    /// not active, e.g. offline MFT sources or a non-Windows host).
    pub active_loops: usize,
}

/// Filesystem locations the daemon is operating from, surfaced in
/// [`StatusResponse`] so an operator can locate the index, socket, and logs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonPaths {
    /// Index data / cache directory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data_dir: String,
    /// IPC socket (Unix) or named pipe (Windows) clients connect on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub socket: String,
    /// Directory the daemon writes its logs to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub log_dir: String,
}

/// Per-drive memory breakdown for status reporting.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DriveMemoryInfo {
    /// Drive letter.
    pub drive: uffs_mft::platform::DriveLetter,
    /// Number of records in this drive's index.
    pub records: usize,
    /// Calculated heap footprint in bytes.
    pub heap_bytes: u64,
    /// Breakdown: records Vec.
    pub records_bytes: u64,
    /// Breakdown: names Vec.
    pub names_bytes: u64,
    /// Breakdown: trigram index.
    pub trigram_bytes: u64,
    /// Breakdown: children index.
    pub children_bytes: u64,
    /// Breakdown: extension index.
    pub ext_index_bytes: u64,
}

/// Response for the `stats` method — daemon performance metrics.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResponse {
    /// Compile-time version of the daemon binary (`env!("CARGO_PKG_VERSION")`).
    ///
    /// Mirrors [`StatusResponse::version`].  Both responses include it
    /// so neither RPC has to chain to the other purely to obtain the
    /// daemon's version string.  Defaults to `""` for pre-0.5.79
    /// daemons (back-compat).
    #[serde(default)]
    pub version: String,
    /// Total search queries served since startup.
    pub total_queries: u64,
    /// Cumulative search time in microseconds.
    pub total_query_time_us: u64,
    /// Average query time in microseconds.
    pub avg_query_time_us: f64,
    /// Time from daemon start to `Ready` in milliseconds.
    pub startup_duration_ms: u64,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Total records across all loaded drives.
    pub total_records: usize,
    /// Queries per second (over daemon lifetime).
    pub queries_per_second: f64,
    /// Aggregate-cache hit count (lifetime, since daemon start).
    ///
    /// Defaults to `0` when the daemon is older than this field
    /// (forward compatibility with pre-0.5.44 daemons).
    #[serde(default)]
    pub agg_cache_hits: u64,
    /// Aggregate-cache miss count (lifetime, includes stale/expired).
    #[serde(default)]
    pub agg_cache_misses: u64,
    /// Number of entries currently in the aggregate cache.
    #[serde(default)]
    pub agg_cache_entries: u64,
}

/// Daemon operational status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state")]
pub enum DaemonStatus {
    /// Daemon is loading indices.
    #[serde(rename = "loading")]
    Loading {
        /// Drives loaded so far.
        drives_loaded: usize,
        /// Total drives to load.
        drives_total: usize,
    },
    /// Daemon is ready to serve queries.
    #[serde(rename = "ready")]
    Ready,
    /// Daemon is refreshing one or more drives.
    #[serde(rename = "refreshing")]
    Refreshing {
        /// Drives being refreshed.
        drives: Vec<uffs_mft::platform::DriveLetter>,
    },
}
