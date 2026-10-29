// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! MFT data loading, caching, and refresh logic for compact indices.
//!
//! Handles cold reads (file/live), IOCP detection, `.uffs` cache integration,
//! compact-cache integration, and USN-based incremental patching.

use std::path::PathBuf;
use std::time::Instant;

use uffs_mft::index::MftIndex;

use crate::compact::{DriveCompactIndex, INDEX_TTL_SECONDS, build_compact_index};

mod apply;
mod rebuild;

/// What produced a given `DriveCompactIndex`.
#[derive(Clone)]
pub enum IndexSource {
    /// Raw/IOCP/compressed MFT file.
    MftFile(PathBuf),
}

/// Timing breakdown for the compact index build.
pub struct LoadTiming {
    /// Time to deserialize the compact cache (milliseconds, 0 if cache miss).
    pub cache: u128,
    /// Time to load/read the MFT (milliseconds, 0 if cache hit).
    pub mft: u128,
    /// Time to build compact records from `MftIndex` (milliseconds, 0 if cache
    /// hit).
    pub compact: u128,
    /// Time to build trigram index (milliseconds, 0 if cache hit).
    pub trigram: u128,
}

/// Where to read MFT data from.
#[derive(Debug, Clone)]
pub enum MftSource {
    /// Offline MFT file (`.uffs`, `.raw`, `.iocp` capture).
    /// Second field is an optional drive-letter override.
    File(PathBuf, Option<uffs_mft::platform::DriveLetter>),
    /// Live Windows NTFS volume (e.g., `'C'`).
    #[cfg(windows)]
    Live(uffs_mft::platform::DriveLetter),
    /// A VSS snapshot device path (e.g.
    /// `\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopyN`), read via
    /// [`uffs_mft::MftReader::open_device_path`]. The `DriveLetter` is
    /// the drive the snapshot was taken from — used only to build the
    /// compact index and for diagnostics, never as a cache key (see
    /// [`Self::is_ephemeral_device`]).
    #[cfg(windows)]
    Device(String, uffs_mft::platform::DriveLetter),
}

impl MftSource {
    /// Returns the file path if this is a `File` source.
    #[must_use]
    #[cfg_attr(
        not(windows),
        expect(
            clippy::unnecessary_wraps,
            reason = "returns None for MftSource::Live on Windows"
        )
    )]
    pub fn file_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::File(path, _) => Some(path),
            #[cfg(windows)]
            Self::Live(_) | Self::Device(..) => None,
        }
    }

    /// Whether this source's data must never be persisted to (or served
    /// from) the drive-letter-keyed on-disk caches — `uffs-mft`'s
    /// `.uffs` index cache and `uffs-core`'s compact cache.
    ///
    /// A VSS snapshot device is an ephemeral, point-in-time capture that
    /// happens to report the same drive letter as the live volume it was
    /// taken from. Both caches key purely by drive letter, so sharing
    /// that key would let the resident daemon serve snapshot data as if
    /// it were live, or (worse) let a snapshot read silently overwrite
    /// the live drive's cache. `Device` sources are always read fresh
    /// and never cached — see `load_mft_index_from_device`.
    #[must_use]
    pub const fn is_ephemeral_device(&self) -> bool {
        match self {
            Self::File(..) => false,
            #[cfg(windows)]
            Self::Live(_) => false,
            #[cfg(windows)]
            Self::Device(..) => true,
        }
    }
}

/// Unified entry point: load MFT data from any source and build a compact
/// index.
///
/// Handles compact cache → MFT cache → cold read → save caches,
/// with `[CACHE_PROFILE]` profiling when `UFFS_CACHE_PROFILE=1`.
///
/// # Errors
///
/// Returns an error if the MFT data cannot be read or parsed.
pub fn load_drive(
    source: &MftSource,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    load_drive_then(source, no_cache, || {})
}

/// [`load_drive`], running `on_cache_written` once the background compact
/// cache save has written the file (never when no save runs or it fails).
///
/// A caller that persists anything describing the saved body — the daemon's
/// USN cursor — must do it here, not when this function returns: the save
/// is still running then, and a crash before it finishes would leave the
/// description ahead of the file on disk.
///
/// # Errors
///
/// Returns an error if the MFT data cannot be read or parsed.
pub fn load_drive_then<F>(
    source: &MftSource,
    no_cache: bool,
    on_cache_written: F,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)>
where
    F: FnOnce() + Send + 'static,
{
    let drive_letter = match source {
        MftSource::File(path, drive_override) => drive_override.unwrap_or_else(|| {
            let stem = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("X");
            stem.chars()
                .next()
                .and_then(|ch| uffs_mft::platform::DriveLetter::parse(ch).ok())
                .unwrap_or(uffs_mft::platform::DriveLetter::X)
        }),
        #[cfg(windows)]
        MftSource::Live(ch) | MftSource::Device(_, ch) => *ch,
    };

    // ── Load MftIndex (cache + USN replay, or cold) ────────────────
    //
    // Phase 5 (#94): the previous `try_compact_cache_hit` fast path
    // returned the on-disk compact cache directly without applying
    // USN deltas, so cold-boot WARM load served stale data for any
    // drive whose compact cache was older than the live filesystem
    // (5/7 drives at the v0.5.80 reference run).  We now always go
    // through the MftIndex path: on Windows this triggers
    // `read_index_cached` → `apply_usn_updates_to_fresh_index`, so
    // the rebuilt compact reflects the live USN journal state.  On
    // non-Windows / offline-file sources the MFT itself is the
    // source of truth and USN replay is a no-op.
    //
    // Cost: ~1.5 s per drive for the compact rebuild + trigram
    // step; at N=7 drives in parallel the cold-boot WARM total
    // moves from ~5.7 s to ~7–10 s (one-time per daemon start).
    // The Phase 4 re-promote path uses
    // [`load_drive_with_usn_refresh`] for the same correctness
    // guarantee.
    let mft_start = Instant::now();
    let mft_index = match source {
        MftSource::File(path, _) => load_mft_index_from_file(path, drive_letter, no_cache)?,
        #[cfg(windows)]
        MftSource::Live(ch) => load_mft_index_live(*ch, no_cache)?,
        #[cfg(windows)]
        MftSource::Device(device_path, ch) => load_mft_index_from_device(device_path, *ch)?,
    };
    let mft_elapsed = mft_start.elapsed().as_millis();

    // ── Build compact index ────────────────────────────────────────
    let (mut compact, compact_elapsed, tri_elapsed) = build_compact_index(drive_letter, &mft_index);

    // Log per-component heap footprint.
    compact.log_heap_report();

    // Free MftIndex (~1.6 GB for 7M records) now that compact is built.
    drop(mft_index);

    if let Some(path) = source.file_path() {
        compact.source = IndexSource::MftFile(path.to_path_buf());
    }

    // ── Save compact cache (background, best-effort) ────────────────
    //
    // Never persist an ephemeral VSS-snapshot-device read: it shares its
    // drive letter's cache key with the live drive but is a distinct,
    // point-in-time capture (see `MftSource::is_ephemeral_device`).
    if !no_cache && !source.is_ephemeral_device() {
        let t_compact_save = Instant::now();
        if let Err(err) =
            crate::compact_cache::save_compact_cache_background_then(&compact, on_cache_written)
        {
            tracing::warn!(drive = %drive_letter, error = %err, "Failed to start compact cache save");
        }
        let compact_save_ms = t_compact_save.elapsed().as_millis();
        tracing::debug!(
            target: "cache_profile",
            compact_save_submit_ms = %compact_save_ms,
            "compact_save_submit (serialized, bg thread spawned)"
        );
    }

    Ok((compact, LoadTiming {
        cache: 0,
        mft: mft_elapsed,
        compact: compact_elapsed,
        trigram: tri_elapsed,
    }))
}

/// Timing breakdown for [`load_drive_with_usn_refresh`].
///
/// Mirrors [`LoadTiming`] but with names that match the USN-refresh
/// flow (no "cache hit" arm — that path was removed in #94).  Field
/// names follow [`LoadTiming`]'s `_ms`-free convention.
#[derive(Debug, Default, Clone, Copy)]
pub struct RefreshTiming {
    /// `MftIndex` load wall-clock in milliseconds (cache hit + USN
    /// replay).
    pub mft: u128,
    /// Compact rebuild from the refreshed `MftIndex` in
    /// milliseconds.
    pub compact: u128,
    /// Trigram CSR rebuild in milliseconds.
    pub trigram: u128,
    /// Total wall-clock in milliseconds, including the
    /// background-save submission.
    pub total: u128,
}

/// Phase 5 (#94) re-promote helper.
///
/// Loads the per-drive `MftIndex` from cache, applies USN journal
/// deltas, rebuilds the `DriveCompactIndex` from the refreshed MFT,
/// and submits a background compact-cache save so the next call is
/// faster.
///
/// Used by the daemon's `DiskBodyLoader::load` and (eventually) the
/// background USN refresh timer (#95).  On Windows the call goes
/// through `MftReader::read_index_cached` →
/// `apply_usn_updates_to_fresh_index` which is the canonical USN
/// replay path the cold-boot fall-through has been using all along.
/// On non-Windows the live-volume reader is unavailable and the
/// function returns an error so the caller can fall back to a bare
/// [`crate::compact_cache::load_compact_cache`].
///
/// # Errors
///
/// Returns an error if the live MFT cannot be opened, the cached
/// `MftIndex` cannot be loaded, or the USN journal apply step fails.
/// The compact-cache save is best-effort (warn-logged on failure)
/// and never propagates an error to the caller.
#[cfg(windows)]
pub fn load_drive_with_usn_refresh(
    drive_letter: uffs_mft::platform::DriveLetter,
) -> anyhow::Result<(DriveCompactIndex, RefreshTiming)> {
    let total_start = Instant::now();
    let mft_start = Instant::now();
    let mft_index = load_mft_index_live(drive_letter, /* no_cache = */ false)?;
    let mft = mft_start.elapsed().as_millis();

    let (mut compact, compact_ms_inner, trigram_ms_inner) =
        build_compact_index(drive_letter, &mft_index);
    drop(mft_index);

    // Persist the USN-refreshed compact so the next promote on this
    // letter (or the next cold-boot) starts from a fresher snapshot.
    // Best-effort — warn on failure but don't fail the caller; the
    // returned in-memory body is still correct.
    if let Err(err) = crate::compact_cache::save_compact_cache_background(&compact) {
        tracing::warn!(
            drive = %drive_letter,
            error = %err,
            "Failed to start USN-refreshed compact cache save (best-effort)",
        );
    }

    let total = total_start.elapsed().as_millis();
    tracing::info!(
        target: "shard.transition",
        drive = %drive_letter,
        mft_ms = %mft,
        compact_ms = %compact_ms_inner,
        trigram_ms = %trigram_ms_inner,
        total_ms = %total,
        records = compact.records.len(),
        "♻️ USN-refreshed body ready (load_drive_with_usn_refresh)",
    );
    compact.source = IndexSource::MftFile(PathBuf::from(format!("{drive_letter}:")));
    Ok((compact, RefreshTiming {
        mft,
        compact: compact_ms_inner,
        trigram: trigram_ms_inner,
        total,
    }))
}

/// Non-Windows stub for [`load_drive_with_usn_refresh`].
///
/// USN journals are NTFS-only.  On macOS / Linux the daemon's only
/// cache source is the offline-file path (`MftSource::File`), which
/// does not need USN replay because the underlying `.iocp` /
/// `.uffs` snapshot is the source of truth.  This stub returns an
/// error so callers (notably `DiskBodyLoader::load`) fall back to
/// [`crate::compact_cache::load_compact_cache`] without a Windows
/// `cfg` gate at the call site.
///
/// # Errors
///
/// Always returns an `anyhow` error noting USN replay is unsupported
/// on this platform.  The caller is expected to handle this by
/// falling back to the bare compact-cache load.
#[cfg(not(windows))]
pub fn load_drive_with_usn_refresh(
    drive_letter: uffs_mft::platform::DriveLetter,
) -> anyhow::Result<(DriveCompactIndex, RefreshTiming)> {
    anyhow::bail!(
        "USN refresh not supported on this platform (drive {drive_letter}); caller should fall back to load_compact_cache"
    )
}

/// Parse a `.uffs` MFT file into `MftIndex`, choosing the parser that
/// matches the file format.
///
/// IOCP captures must use `load_iocp_to_index` (unified `process_record`
/// path) which mirrors the Windows LIVE inline parser exactly.  The
/// generic `load_raw_to_index_with_options` dispatches IOCP to
/// `load_iocp_capture_to_index` (`MftRecordMerger` multi-pass path) which
/// produces different `total_stream_count` values and therefore
/// different tree metrics (descendants, treesize) — a known parity
/// divergence that we avoid by picking the right parser up front.
fn parse_mft_file_to_index(
    mft_path: &std::path::Path,
    drive_letter: uffs_mft::platform::DriveLetter,
) -> anyhow::Result<MftIndex> {
    // `?` performs the `MftError → anyhow::Error` conversion via
    // `From<MftError> for anyhow::Error`, matching the original
    // inline call site's behaviour.
    let is_iocp = uffs_mft::is_iocp_capture(mft_path).unwrap_or(false);
    if is_iocp {
        tracing::info!(
            drive = %drive_letter,
            "📼 IOCP capture detected — using unified process_record parser for parity"
        );
        return Ok(uffs_mft::load_iocp_to_index(mft_path)?);
    }
    let options = uffs_mft::raw::LoadRawOptions {
        header_only: false,
        volume_letter: Some(drive_letter),
        forensic: false,
    };
    Ok(uffs_mft::MftReader::load_raw_to_index_direct(
        mft_path, &options,
    )?)
}

/// Kick off the post-parse background cache save and emit a matching
/// tracing line for success or failure.
fn spawn_mft_cache_save(index: &MftIndex, drive_letter: uffs_mft::platform::DriveLetter) {
    match uffs_mft::cache::save_to_cache_background(
        index,
        drive_letter,
        0,
        0,
        uffs_mft::usn::Usn::ZERO,
    ) {
        Ok(()) => {
            tracing::info!(drive = %drive_letter, "💾 MFT cache save started (background)");
        }
        Err(err) => {
            tracing::warn!(
                drive = %drive_letter,
                error = %err,
                "Failed to start .uffs cache save"
            );
        }
    }
}

/// Load `MftIndex` from an offline file (cache → cold parse).
fn load_mft_index_from_file(
    mft_path: &std::path::Path,
    drive_letter: uffs_mft::platform::DriveLetter,
    no_cache: bool,
) -> anyhow::Result<MftIndex> {
    let cached = if no_cache {
        None
    } else {
        uffs_mft::cache::load_cached_index(drive_letter, INDEX_TTL_SECONDS)
    };
    if let Some((cached_index, _header)) = cached {
        tracing::info!(
            drive = %drive_letter,
            records = cached_index.records.len(),
            "📦 Cache hit — loaded .uffs cache"
        );
        return Ok(cached_index);
    }

    tracing::info!(
        drive = %drive_letter,
        path = %mft_path.display(),
        "📖 Parsing MFT file (delegating to uffs-mft)"
    );
    let parsed = parse_mft_file_to_index(mft_path, drive_letter)?;
    spawn_mft_cache_save(&parsed, drive_letter);
    Ok(parsed)
}

/// Load `MftIndex` from a live Windows volume (cache → cold read via IOCP).
///
/// Extracted from `load_drive` for readability; the workspace allows
/// `clippy::single_call_fn` so this remains a one-call helper rather
/// than being inlined.
#[cfg(windows)]
fn load_mft_index_live(
    drive_letter: uffs_mft::platform::DriveLetter,
    no_cache: bool,
) -> anyhow::Result<MftIndex> {
    use anyhow::Context as _;

    let read_index = async {
        let reader = uffs_mft::MftReader::open(drive_letter)
            .with_context(|| format!("Failed to open drive {drive_letter}:"))?;
        if no_cache {
            reader
                .read_all_index()
                .await
                .with_context(|| format!("Failed to read MFT fresh for drive {drive_letter}:"))
        } else {
            reader
                .read_index_cached(INDEX_TTL_SECONDS)
                .await
                .with_context(|| format!("Failed to read MFT for drive {drive_letter}:"))
        }
    };

    // Run the async MFT read synchronously.
    //
    // This function is called from multiple contexts:
    //   - Tokio worker threads (CLI `#[tokio::main]`)
    //   - Tokio blocking threads (daemon `JoinSet::spawn_blocking`)
    //   - No runtime at all (standalone tests)
    //
    // A dedicated current-thread runtime is always safe regardless of the
    // calling context.  The ~50µs overhead is negligible against seconds of
    // MFT I/O.  This avoids `block_in_place` (panics on blocking threads)
    // and `handle.block_on` (panics inside a runtime context).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(read_index)
}

/// Load `MftIndex` fresh from a VSS snapshot device path.
///
/// Always a fresh read — deliberately never consults or populates the
/// drive-letter-keyed `.uffs` index cache (unlike
/// [`load_mft_index_live`]/[`load_mft_index_from_file`]). See
/// [`MftSource::is_ephemeral_device`] for why sharing that cache key
/// would be a correctness bug, not just a missed optimization.
#[cfg(windows)]
fn load_mft_index_from_device(
    device_path: &str,
    drive_letter: uffs_mft::platform::DriveLetter,
) -> anyhow::Result<MftIndex> {
    use anyhow::Context as _;

    let read_index = async {
        let reader = uffs_mft::MftReader::open_device_path(device_path, drive_letter)
            .with_context(|| format!("Failed to open snapshot device {device_path}"))?;
        reader
            .read_all_index()
            .await
            .with_context(|| format!("Failed to read MFT from snapshot device {device_path}"))
    };

    // See `load_mft_index_live`'s matching comment: a dedicated
    // current-thread runtime is always safe regardless of the calling
    // context (Tokio worker thread, blocking thread, or no runtime).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(read_index)
}

/// Statistics from in-place USN patching.
#[derive(Debug, Clone, Default)]
pub struct PatchStats {
    /// Records marked as deleted (`name_len` zeroed).
    pub deleted: usize,
    /// New records appended.
    pub created: usize,
    /// Records with updated name/parent.
    pub renamed: usize,
    /// Changes skipped (FRS not in index, or no actionable change).
    pub skipped: usize,
}

/// Refresh a drive by reloading from its original source.
///
/// # Errors
///
/// Returns an error if the drive source cannot be reloaded.
pub fn refresh_drive(drive: &DriveCompactIndex) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    match &drive.source {
        IndexSource::MftFile(path) => {
            let source = if path.to_string_lossy().len() <= 2 {
                #[cfg(windows)]
                {
                    MftSource::Live(drive.letter)
                }
                #[cfg(not(windows))]
                {
                    anyhow::bail!("Cannot refresh live drive {}: on non-Windows", drive.letter);
                }
            } else {
                MftSource::File(path.clone(), Some(drive.letter))
            };
            load_drive(&source, false)
        }
    }
}

/// Convenience wrapper for loading an MFT file.
///
/// **Deprecated:** Use [`load_drive`] with [`MftSource::File`] instead.
///
/// # Errors
///
/// Returns an error if the MFT file cannot be read or parsed.
#[deprecated(
    since = "0.4.31",
    note = "Use load_drive(MftSource::File(...)) instead"
)]
pub fn load_mft_file(
    mft_path: &std::path::Path,
    drive: Option<uffs_mft::platform::DriveLetter>,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    load_drive(&MftSource::File(mft_path.to_path_buf(), drive), no_cache)
}

/// Order a USN batch so that a created/renamed record is applied AFTER the
/// change that creates its parent directory, whenever that parent is itself
/// created in the same batch. The journal delivers records in time order
/// (a directory exists before anything appears inside it), but
/// [`uffs_mft::usn::aggregate_changes`] folds them into a `HashMap`, so the
/// batch arrives in arbitrary order at every consumer (the live journal loop
/// AND the cold-start cache replay) — fixing it here covers both.
///
/// A fixpoint pass handles arbitrarily deep same-batch trees (an unzip
/// creates `dir/sub/file` in one poll window). Changes whose parent is
/// genuinely absent from the batch are emitted as-is: the unknown-parent
/// fallback still applies, but is now reached only for truly missing parents
/// (lost/overflowed journal events), and is logged.
fn order_parent_first(changes: &[uffs_mft::usn::FileChange]) -> Vec<&uffs_mft::usn::FileChange> {
    use std::collections::HashSet;

    // Only a parent that is CREATED in this batch can become resolvable by
    // reordering; everything else keeps its relative order.
    let batch_creates: HashSet<u64> = changes
        .iter()
        .filter(|change| change.created)
        .map(|change| change.frs.raw())
        .collect();
    let mut applied_creates: HashSet<u64> = HashSet::new();
    let mut out: Vec<&uffs_mft::usn::FileChange> = Vec::with_capacity(changes.len());
    let mut deferred: Vec<&uffs_mft::usn::FileChange> = Vec::new();

    for change in changes {
        let parent = change.parent_frs.raw();
        let waits_for_parent = (change.created || change.renamed)
            && parent != change.frs.raw()
            && batch_creates.contains(&parent)
            && !applied_creates.contains(&parent);
        if waits_for_parent {
            deferred.push(change);
        } else {
            if change.created {
                applied_creates.insert(change.frs.raw());
            }
            out.push(change);
        }
    }

    // Fixpoint: each pass releases the changes whose parent has now been
    // emitted; depth-N same-batch trees settle in N passes.
    while !deferred.is_empty() {
        let before = deferred.len();
        deferred.retain(|change| {
            if applied_creates.contains(&change.parent_frs.raw()) {
                if change.created {
                    applied_creates.insert(change.frs.raw());
                }
                out.push(change);
                false
            } else {
                true
            }
        });
        if deferred.len() == before {
            // No progress: a dependency cycle (corrupt journal) or parents
            // whose own creates never surfaced. Emit as-is — the
            // unknown-parent fallback handles them — but say so.
            for change in &deferred {
                tracing::warn!(
                    frs = change.frs.raw(),
                    parent_frs = change.parent_frs.raw(),
                    name = %change.filename,
                    "usn apply: parent create not resolvable within the batch; \
                     falling back to unknown-parent"
                );
                out.push(change);
            }
            break;
        }
    }
    out
}

/// Apply USN changes in-place to the compact index.
///
/// Mutates records (`parent_idx`, names, flags) and the
/// `frs_to_compact` mapping then rebuilds the children CSR + path
/// lengths + trigram + extension index once at the end.  Typical
/// cost: <5ms for record mutations + ~100ms for CSR rebuild on a
/// 7M-record drive (the rebuild dominates).
///
/// **Platform note.**  The function itself is pure data manipulation
/// over `DriveCompactIndex` + the platform-agnostic
/// [`uffs_mft::usn::FileChange`] DTO and compiles + runs on all
/// targets.  Only the *journal source* that produces the
/// `&[FileChange]` slice (`uffs_mft::usn::read_usn_journal`) is
/// Windows-only.  This split is what makes the Phase 7 per-shard
/// patch path Mac-testable end-to-end via synthesised
/// [`uffs_mft::usn::FileChange`] arrays.
///
/// **Phase 8.** The `frs_to_compact` mapping is read from
/// [`DriveCompactIndex::frs_to_compact`] (no longer a separate
/// parameter) and maintained in lock-step with the records:
///
/// * **Create** \u2014 a new compact slot is appended at `records.len()` and
///   `frs_to_compact[new_frs]` is updated to point at it (extending the table
///   if the FRS exceeds the current `frs_to_compact.len()`).
/// * **Delete** \u2014 `frs_to_compact[deleted_frs] = u32::MAX` so subsequent
///   batches referencing the deleted FRS take the skip branch instead of
///   mis-applying to a tombstoned slot.
/// * **Rename** \u2014 the FRS keeps its compact slot; only `parent_idx` + name
///   move.  Mapping is unchanged.
///
/// **Empty-mapping fallback.** When `drive.frs_to_compact.is_empty()`
/// (v9 caches loaded before Phase 8 cache format v10) every change
/// looks up to `u32::MAX` and the function increments `skipped` for
/// the whole batch \u2014 the surgical patch silently degrades to a
/// no-op so the caller's full-reload fallback path runs.
pub fn apply_usn_patch(
    drive: &mut DriveCompactIndex,
    changes: &[uffs_mft::usn::FileChange],
) -> PatchStats {
    let mut stats = PatchStats::default();

    // Phase 1: collect the records whose path_len must be refreshed so the
    // post-loop rebuild can do an O(changed) path update instead of the
    // O(total) BFS (incremental-index-maintenance §5.5).  `path_changes`
    // doubles as the Phase-2b trigram-ADD set (every created / renamed record
    // re-adds its new name's trigrams to the delta).
    let mut path_changes: Vec<crate::compact::PathChange> = Vec::new();
    // Phase 2b: records whose stale base trigram postings must be masked —
    // deletes, renames, and FRS-reuse overwrites.
    let mut tombstones: Vec<u32> = Vec::new();

    // Wall-clock the whole apply (O(changed) mutation loop + the post-loop
    // overlay/path refresh) for the DEBUG batch summary.
    let t_apply = Instant::now();
    // Parents before children: `aggregate_changes` hands the batch over in
    // HashMap (arbitrary) order, so without reordering a file's create could
    // apply before its parent directory's create — the parent lookup then
    // misses and bakes the unknown-parent sentinel (`u32::MAX`), which the
    // path machinery legitimately renders as a ROOT-LEVEL entry. That is the
    // #510 bug: freshly-unzipped files reported at `C:\<name>` and an
    // uninstall "removing" phantom paths while the real files survived.
    let ordered = order_parent_first(changes);
    for change in ordered {
        // Typed `Frs` → raw `u64` lift at the frs_to_compact CSR lookup
        // boundary.  The mapping table is `Vec<u32>` indexed by `usize`,
        // so demoting once per change keeps the inner index arithmetic on
        // raw values without leaking raw FRS into the `FileChange` API.
        let frs_usize = uffs_mft::frs_to_usize(change.frs.raw());
        let compact_idx = drive
            .frs_to_compact
            .get(frs_usize)
            .copied()
            .unwrap_or(u32::MAX);

        // Per-change disposition trace — enable with `--log-level trace` to
        // see exactly which branch each USN event takes and the slot it
        // resolved to (the field-debug hook for USN-delta investigations).
        tracing::trace!(
            drive = %drive.letter,
            frs = change.frs.raw(),
            name = %change.filename,
            created = change.created,
            deleted = change.deleted,
            renamed = change.renamed,
            compact_idx,
            mapped = (compact_idx != u32::MAX),
            "usn apply: change"
        );

        // The flags are mutually-exclusive net states (resolved in
        // `aggregate_changes`), so a simple priority dispatch is correct.
        if change.deleted {
            apply::apply_delete(drive, frs_usize, compact_idx, &mut stats, &mut tombstones);
        } else if change.created {
            apply::apply_create(
                drive,
                change,
                frs_usize,
                compact_idx,
                &mut stats,
                &mut path_changes,
                &mut tombstones,
            );
        } else if change.renamed {
            apply::apply_rename(
                drive,
                change,
                compact_idx,
                &mut stats,
                &mut path_changes,
                &mut tombstones,
            );
        } else {
            stats.skipped += 1;
        }
    }

    // Overlay the batch onto the base ∪ delta indexes + refresh path lengths
    // (incremental-index-maintenance); the occasional compaction folds the
    // delta back into fresh bases.  Extracted to `rebuild.rs`.
    let compacted = rebuild::rebuild_derived(drive, &path_changes, &tombstones);

    if !changes.is_empty() {
        rebuild::log_batch_summary(
            drive,
            changes.len(),
            &stats,
            compacted,
            t_apply.elapsed().as_micros(),
        );
    }

    stats
}

#[cfg(test)]
#[path = "compact_loader_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compact_loader_usn_order_tests.rs"]
mod usn_order_tests;

#[cfg(test)]
#[path = "compact_loader_path_oracle_tests.rs"]
mod path_oracle_tests;

#[cfg(test)]
#[path = "compact_loader_trigram_oracle_tests.rs"]
mod trigram_oracle_tests;
