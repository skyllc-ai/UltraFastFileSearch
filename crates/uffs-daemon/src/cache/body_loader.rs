// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Body source for the Phase 3 demote / promote orchestrator.
//!
//! Phase 3 of the memory-tiering work
//! (`docs/refactor/memory-tiering-implementation-plan.md`).
//!
//! [`IndexManager::ensure_warm_for_dispatch`] needs to materialise a
//! `DriveCompactIndex` for any `Parked` / `Cold` shard the search
//! hot path touches.  The production source is the encrypted compact
//! cache on disk
//! (`<cache_dir>/<letter>_compact.uffs`); tests inject deterministic
//! fakes (fixed body, missing body, panicking loader) without
//! touching the platform cache directory.
//!
//! Both flavours implement [`BodyLoader`].  The trait is **sync** —
//! the caller in `IndexManager` wraps every call in
//! `tokio::task::spawn_blocking` since the production loader does
//! file I/O + decryption.  Test fakes are cheap and could be sync,
//! but the same `spawn_blocking` wrapper applies uniformly to keep
//! the call site simple.
//!
//! [`IndexManager::ensure_warm_for_dispatch`]:
//!     crate::index::IndexManager::ensure_warm_for_dispatch

use alloc::sync::Arc;

use uffs_core::compact::DriveCompactIndex;

/// Source for `Parked` / `Cold` shard bodies.
///
/// Implementations are held by [`crate::index::IndexManager`] as an
/// `Arc<dyn BodyLoader>` so the manager can be constructed with the
/// production [`DiskBodyLoader`] for normal daemon operation, or
/// with a test fake for the Commit-E integration tests.
///
/// The single method is **synchronous** — the demote / promote
/// orchestrator wraps every call in
/// `tokio::task::spawn_blocking`, so implementations can do file
/// I/O, decryption, allocation, or whatever they need without
/// blocking the async runtime.  Async work inside the loader is
/// not supported and would deadlock the spawn-blocking thread
/// pool.
///
/// Returning `None` is a graceful failure: the orchestrator emits
/// a `target: "shard.transition"` warning and leaves the shard in
/// its current tier.  A panic is also recovered by the
/// `JoinError` arm in the orchestrator and emits an error-level
/// `shard.transition` event; the shard stays put.
pub(crate) trait BodyLoader: Send + Sync + 'static {
    /// Materialise the body for `letter`, or return `None` if the
    /// underlying source is missing / stale / corrupted.
    fn load(&self, letter: uffs_mft::platform::DriveLetter) -> Option<Arc<DriveCompactIndex>>;
}

/// Production loader: reads
/// `<cache_dir>/<letter>_compact.uffs`, validates the header,
/// decrypts the encrypted body, and assembles a fresh
/// `DriveCompactIndex` (heap or runtime-mmap variant per
/// `compact_cache`'s own selection logic).
///
/// `ttl_seconds = u64::MAX` skips the freshness check — the
/// demote / promote path doesn't care if the cache is "old", only
/// that it is a valid serialisation of the drive that *was* loaded.
///
/// Phase 5 (#94) wires this loader to `load_drive_with_usn_refresh`
/// so re-promote on Windows applies USN deltas before the body
/// reaches the search hot path.  This struct stays as the
/// "trust-the-on-disk-cache" fallback: if MFT cache is missing or
/// the USN journal is unavailable (e.g. drive G `error 1179`), the
/// daemon logs the failure and serves the un-refreshed body so the
/// shard still becomes usable.
///
/// Phase 5 (#96) updates the failure path to log the structured
/// [`uffs_core::compact_cache::LoadCacheError`] variant so the
/// daemon's tracing output distinguishes "cache file missing"
/// (cold-boot first-touch) from "decryption failed" (key rotated;
/// alert) from "stale by TTL" (idle-timer rebuild trigger).
pub(crate) struct DiskBodyLoader;

impl BodyLoader for DiskBodyLoader {
    fn load(&self, letter: uffs_mft::platform::DriveLetter) -> Option<Arc<DriveCompactIndex>> {
        // Fast path: the compact cache plus the journal changes since its
        // last save, replayed synchronously before the body is served.
        // Seconds and one index's worth of memory instead of a full
        // rebuild; `None` whenever the replay could not be exact.
        #[cfg(windows)]
        if let Some(body) = super::promote_load::load_parked_drive(letter) {
            return Some(Arc::new(body));
        }

        // Phase 5 (#94): full rebuild — USN-refreshed re-promote.
        // On Windows this applies USN deltas to the cached MftIndex
        // and rebuilds the compact index, so the body served to the
        // search hot path reflects the live filesystem state since
        // the daemon's last MFT refresh.  On non-Windows the helper
        // errors out by design and we fall through to the bare
        // compact-cache load below.
        //
        // NB: the startup warm-load guard (`cache::guarded_load`) is
        // deliberately NOT used here.  Re-promote runs while the
        // daemon is live, and the per-shard journal loop's in-memory
        // cursor moved on while the shard was demoted.  Serving the
        // on-disk compact cache then deferring to that loop would
        // strand the `[demote, now]` delta — so the delta is replayed
        // synchronously, either by the fast path above or by this
        // full rebuild.
        match uffs_core::compact_loader::load_drive_with_usn_refresh(letter) {
            Ok((body, _timing)) => return Some(Arc::new(body)),
            Err(err) => {
                tracing::warn!(
                    target: "shard.transition",
                    drive = %letter,
                    error = %err,
                    reason = "promote-on-search",
                    "USN-refresh failed; falling back to bare compact-cache load",
                );
            }
        }

        // Phase 5 (#94) fallback: the USN-refresh path failed (cache
        // missing, journal unavailable, drive G `error 1179`, etc.).
        // Serve the un-refreshed compact cache so the shard is still
        // usable, with a structured `LoadCacheError` (#96) surfaced
        // on failure.
        match uffs_core::compact_cache::load_compact_cache(letter, u64::MAX, 0, true) {
            Ok(body) => Some(Arc::new(body)),
            Err(err) => {
                tracing::warn!(
                    target: "shard.transition",
                    drive = %letter,
                    error = %err,
                    reason = "promote-on-search",
                    "compact-cache fallback also failed; shard stays in current tier",
                );
                None
            }
        }
    }
}
