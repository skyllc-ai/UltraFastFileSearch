// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Guarded warm-load: serve the on-disk compact cache fast when the
//! background USN journal loop can converge the (bounded) delta, and
//! fall back to a synchronous full rebuild only when it cannot.
//!
//! ## Why this exists
//!
//! Before #94 the warm load (daemon restart from an existing cache)
//! deserialized the compact cache directly — ~20 ms — and relied on a
//! later refresh to catch up.  #94 replaced that with a **synchronous**
//! `load_drive_with_usn_refresh` on every warm load: a full `MftIndex`
//! read plus a complete `build_compact_index` (records + `path_len` +
//! trigram + children + extension indexes).  That fixed a real
//! staleness bug (5/7 drives served stale data at v0.5.80) **but** moved
//! hundreds of milliseconds of CPU onto the warm-start critical path,
//! regressing the WARM phase.
//!
//! Since #94, the Phase 7 per-shard journal loop
//! ([`crate::cache::journal_loop`]) exists: it polls the live USN
//! journal every 500 ms, seeds its cursor from the persisted
//! `<letter>_usn.cursor`, and applies deltas incrementally.  That loop
//! makes the synchronous refresh **redundant for freshness** in the
//! common case — the loop converges the cache to the live filesystem
//! within roughly one poll interval after startup.
//!
//! ## The guard
//!
//! `decide_strategy` inspects the cheap signals only — the persisted
//! cursor (an 8-byte file read) and `FSCTL_QUERY_USN_JOURNAL` (a single
//! ioctl) — and never touches the multi-hundred-MB `MftIndex`:
//!
//! * `WarmLoadStrategy::FastFromCompactCache` — the persisted cursor lies
//!   inside the live journal's valid window (`first_usn <= cursor <=
//!   next_usn`).  The compact cache is at least as fresh as that cursor (the
//!   journal loop writes body + cursor in lockstep, and full rebuilds write a
//!   body at the live head ≥ the cursor), so serving it immediately is safe:
//!   the background loop re-applies `[cursor, live)` — idempotent on any
//!   overlap — and converges within ~one poll interval.
//!
//! * `WarmLoadStrategy::FullRebuild` — the cursor is absent (`0` sentinel: cold
//!   boot / never persisted), predates the journal (`cursor < first_usn`:
//!   wrapped or long-downtime), or postdates it (`cursor > next_usn`: the
//!   journal was deleted + recreated and is younger than the cursor).  In each
//!   case the background loop cannot converge the existing cache from the
//!   persisted cursor, so we pay the synchronous rebuild — preserving #94's
//!   correctness guarantee.
//!
//! ### Residual edge case
//!
//! A journal deleted + recreated *while the daemon was down* whose new
//! `[first_usn, next_usn]` window happens to contain the stale cursor
//! would pass the bounds check yet point into an unrelated USN space.
//! This requires an admin `fsutil usn deletejournal` plus a coincidental
//! USN-range overlap and is not reachable under normal operation.
//! Closing it fully would require persisting the journal id alongside
//! the cursor; that is deliberately out of scope here to keep the change
//! surgical, and is documented rather than hidden.

#[cfg(windows)]
use uffs_core::compact::DriveCompactIndex;

/// Outcome of `decide_strategy`: how to materialise a drive's body on
/// a warm load.
///
/// Compiled on Windows (the only platform with a USN journal) and under
/// `test` (so the boundary logic is host-testable); a non-Windows
/// release build never reaches the decision, so it is not compiled there.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WarmLoadStrategy {
    /// Deserialize the on-disk compact cache directly (~20 ms) and let
    /// the background journal loop converge the bounded delta.
    FastFromCompactCache,
    /// Synchronously rebuild from a live `MftIndex` read + USN replay
    /// (the #94 path) because the background loop cannot converge the
    /// existing cache from the persisted cursor.
    FullRebuild,
}

/// Pure warm-load decision from the cheap signals.
///
/// `cursor` is the persisted `<letter>_usn.cursor` value (`0` when no
/// cursor has been persisted yet).  `first_usn` / `next_usn` are the
/// live journal bounds from `FSCTL_QUERY_USN_JOURNAL`.
///
/// Kept platform-agnostic and side-effect-free so the boundary logic is
/// pinned by host unit tests rather than only exercised on Windows.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn decide_strategy(cursor: u64, first_usn: i64, next_usn: i64) -> WarmLoadStrategy {
    // No persisted cursor → cold boot / first touch: the background loop
    // would have to replay from the journal head, which cannot
    // reconstruct an arbitrary on-disk snapshot.  Build synchronously.
    if cursor == 0 {
        return WarmLoadStrategy::FullRebuild;
    }
    // Narrow the unsigned persisted cursor to the kernel-facing signed
    // USN space exactly as the journal source does
    // (`WindowsJournalSource::poll`), so the comparison matches the value
    // the background loop will actually feed to `FSCTL_READ_USN_JOURNAL`.
    let cursor_usn = i64::try_from(cursor).unwrap_or(i64::MAX);
    // Cursor predates the oldest readable record (journal wrapped or the
    // daemon was down long enough for the gap to be truncated), or
    // postdates the live head (journal recreated younger than the
    // cursor).  Either way the loop can't converge the existing cache.
    if cursor_usn < first_usn || cursor_usn > next_usn {
        return WarmLoadStrategy::FullRebuild;
    }
    WarmLoadStrategy::FastFromCompactCache
}

/// Windows warm-load entry point used by the startup live-drive loader
/// and the re-promote body loader.
///
/// Returns the same `(DriveCompactIndex, LoadTiming)` shape as
/// [`uffs_core::compact::load_drive`] so call sites are unchanged.
///
/// # Errors
///
/// Propagates the underlying loader error when both the fast compact
/// cache path and the synchronous rebuild fail.
#[cfg(windows)]
pub(crate) fn load_live_drive(
    letter: uffs_mft::platform::DriveLetter,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, uffs_core::compact::LoadTiming)> {
    use std::time::Instant;

    use crate::cache::cursor_store::DiskCursorStore;
    use crate::cache::journal_loop::CursorStore as _;

    // `--no-cache` forces a clean rebuild and must not consult the cache.
    if no_cache {
        return full_rebuild(letter, no_cache, None);
    }

    // Cheap signal #1: live journal bounds (single ioctl, no MFT read).
    let info = match uffs_mft::usn::query_usn_journal(letter) {
        Ok(info) => info,
        Err(err) => {
            // No journal (e.g. os error 1179) → no incremental refresh
            // mechanism, so the background loop can't converge a cached
            // body.  Rebuild synchronously, matching the pre-guard path.
            tracing::debug!(
                target: "shard.warm_load",
                drive = %letter,
                error = %err,
                "USN journal unavailable; full rebuild",
            );
            return full_rebuild(letter, no_cache, None);
        }
    };

    // Cheap signal #2: persisted cursor (8-byte file read).
    let cursor_store = DiskCursorStore::new(uffs_mft::cache::cache_dir());
    let cursor = cursor_store.load(letter);

    match decide_strategy(cursor, info.first_usn.raw(), info.next_usn.raw()) {
        WarmLoadStrategy::FastFromCompactCache => {
            let t0 = Instant::now();
            match uffs_core::compact_cache::load_compact_cache(letter, u64::MAX, 0, true) {
                Ok(body) => {
                    let cache_ms = t0.elapsed().as_millis();
                    tracing::info!(
                        target: "shard.warm_load",
                        drive = %letter,
                        cursor,
                        first_usn = %info.first_usn,
                        next_usn = %info.next_usn,
                        cache_ms,
                        "Warm load: fast compact-cache path (background loop converges delta)",
                    );
                    Ok((body, uffs_core::compact::LoadTiming {
                        cache: cache_ms,
                        mft: 0,
                        compact: 0,
                        trigram: 0,
                    }))
                }
                Err(err) => {
                    // Cache missing / corrupt / key rotated: the fast
                    // path is unavailable, so rebuild synchronously.
                    tracing::warn!(
                        target: "shard.warm_load",
                        drive = %letter,
                        error = %err,
                        "Warm load: compact cache unusable; falling back to full rebuild",
                    );
                    full_rebuild(letter, no_cache, Some(&info))
                }
            }
        }
        WarmLoadStrategy::FullRebuild => full_rebuild(letter, no_cache, Some(&info)),
    }
}

/// Run the synchronous full rebuild ([`uffs_core::compact::load_drive_then`])
/// and, once its cache file is written, persist a cursor so a subsequent
/// restart can take the fast path.
///
/// The persisted cursor is the journal's `next_usn` **captured before**
/// the rebuild's MFT read.  The freshly built body reflects USN state up
/// to the live head at read time, which is `>=` that pre-read value, so
/// the persisted cursor is a safe lower bound: the background loop
/// re-applies `[cursor, live)` idempotently with no gap.
#[cfg(windows)]
fn full_rebuild(
    letter: uffs_mft::platform::DriveLetter,
    no_cache: bool,
    info: Option<&uffs_mft::usn::UsnJournalInfo>,
) -> anyhow::Result<(DriveCompactIndex, uffs_core::compact::LoadTiming)> {
    use crate::cache::cursor_store::DiskCursorStore;
    use crate::cache::journal_loop::CursorStore as _;

    // Best-effort cursor seed so the next warm load is fast.  Only when
    // caching is enabled and we have a live journal reading; never
    // regress a newer persisted cursor (the journal loop may have
    // advanced past this pre-read lower bound already).  Stored only once
    // the rebuilt body's cache file is on disk: seeding when `load_drive`
    // returns (its save still running) let a crash in between leave the
    // cursor ahead of the file, and the next warm load skip the changes
    // in between for good.
    let pre_read = info.filter(|_journal| !no_cache).map_or(0, |journal| {
        u64::try_from(journal.next_usn.raw()).unwrap_or(0)
    });
    uffs_core::compact::load_drive_then(
        &uffs_core::compact::MftSource::Live(letter),
        no_cache,
        move || {
            if pre_read == 0 {
                return;
            }
            let store = DiskCursorStore::new(uffs_mft::cache::cache_dir());
            let seed = store.load(letter).max(pre_read);
            store.store(letter, seed);
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{WarmLoadStrategy, decide_strategy};

    #[test]
    fn zero_cursor_forces_full_rebuild() {
        // No persisted cursor → cold boot; the loop can't reconstruct an
        // arbitrary snapshot from the journal head.
        assert_eq!(decide_strategy(0, 0, 1_000), WarmLoadStrategy::FullRebuild);
    }

    #[test]
    fn cursor_inside_window_takes_fast_path() {
        assert_eq!(
            decide_strategy(500, 100, 1_000),
            WarmLoadStrategy::FastFromCompactCache
        );
    }

    #[test]
    fn cursor_at_window_edges_is_fast() {
        // Inclusive bounds: a cursor exactly at first_usn or next_usn is
        // still convergeable by the background loop.
        assert_eq!(
            decide_strategy(100, 100, 1_000),
            WarmLoadStrategy::FastFromCompactCache
        );
        assert_eq!(
            decide_strategy(1_000, 100, 1_000),
            WarmLoadStrategy::FastFromCompactCache
        );
    }

    #[test]
    fn cursor_before_first_usn_rebuilds() {
        // Journal wrapped / long downtime: the gap was truncated.
        assert_eq!(
            decide_strategy(50, 100, 1_000),
            WarmLoadStrategy::FullRebuild
        );
    }

    #[test]
    fn cursor_after_next_usn_rebuilds() {
        // Journal recreated younger than the persisted cursor.
        assert_eq!(
            decide_strategy(2_000, 100, 1_000),
            WarmLoadStrategy::FullRebuild
        );
    }

    #[test]
    fn cursor_overflowing_i64_is_clamped_and_rebuilds() {
        // A cursor past i64::MAX narrows to i64::MAX, which is > any real
        // next_usn, so it conservatively rebuilds rather than trusting a
        // nonsensical value.
        assert_eq!(
            decide_strategy(u64::MAX, 100, 1_000),
            WarmLoadStrategy::FullRebuild
        );
    }
}
