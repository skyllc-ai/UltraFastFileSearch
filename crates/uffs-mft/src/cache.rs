// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Index caching with TTL (Time-To-Live) support.
//!
//! This module provides automatic caching of MFT indices in a secure,
//! platform-appropriate directory with configurable TTL. Indices are
//! automatically refreshed when stale.
//!
//! # Cache Location
//!
//! Indices are stored in a platform-specific secure directory:
//! - **Windows**: `%LOCALAPPDATA%\uffs\cache\`
//! - **macOS**: `~/Library/Caches/com.uffs/`
//! - **Linux**: `$XDG_CACHE_HOME/uffs/` (default `~/.cache/uffs/`)
//!
//! Files:
//! - `C_index.uffs` - Index for C: drive
//! - `D_index.uffs` - Index for D: drive
//! - etc.
//!
//! # Security
//!
//! - Cache directory is created with owner-only permissions (0700 / DACL)
//! - Cache files are written with owner-only permissions (0600)
//! - Writes use atomic temp-file-then-rename to prevent partial exposure
//! - Legacy cache files in `{TEMP}/uffs_index_cache/` are automatically
//!   migrated to the secure location on first access.
//!
//! # TTL Behavior
//!
//! - **Single drive**: If TTL expired, refresh that drive's index only
//! - **Multi-drive**: If ANY file is expired, remove ALL and rebuild all
//! - **Cleanup**: Remove cache directory if all files are expired

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::SystemTime;

use crate::index::{IndexHeader, MftIndex, usize_to_f64};

/// Cached `DataFrame` load/build (Windows-only). Split out for file-size
/// policy.
#[path = "cache_dataframe.rs"]
mod cache_dataframe;
#[cfg(windows)]
pub use cache_dataframe::load_or_build_dataframe_cached;

/// Compression/encryption pipeline for cache files — split out for file-size
/// policy.
#[path = "cache_compress.rs"]
mod cache_compress;
/// Decompressed-size trailer for cache files (exact-size decompression).
#[path = "cache_size_trailer.rs"]
mod cache_size_trailer;
pub use cache_compress::{
    compress_encrypt_write, compress_encrypt_write_streaming,
    compress_encrypt_write_streaming_with_workers, compress_zstd_mt, default_zstd_workers,
    new_zstd_encoder_with_workers, new_zstd_mt_encoder,
};
pub use cache_size_trailer::{
    CountingWriter, append_size_trailer, decompress_sized, size_from_trailer,
};

/// Default TTL for cached indices (4 hours).
///
/// USN Journal integration handles incremental freshness, so the TTL only
/// serves as a safety-net periodic full rescan. Extended from 10 minutes to
/// 4 hours to avoid unnecessary full MFT rescans on every cache expiry.
pub const INDEX_TTL_SECONDS: u64 = 14400;

/// Name of the legacy cache directory in the system temp folder.
const LEGACY_CACHE_DIR_NAME: &str = "uffs_index_cache";

/// One-time migration guard.
static MIGRATION_ONCE: Once = Once::new();

// ────────────────────────────────────────────────────────────────────────────
// S1.1 — Cache Directory Relocation
// ────────────────────────────────────────────────────────────────────────────

/// Returns the platform-appropriate secure cache directory.
///
/// | Platform | Path |
/// |----------|------|
/// | Windows  | `%LOCALAPPDATA%\uffs\cache\` |
/// | macOS    | `~/Library/Caches/com.uffs/` |
/// | Linux    | `$XDG_CACHE_HOME/uffs/` (default `~/.cache/uffs/`) |
///
/// Falls back to `{TEMP}/uffs_index_cache/` if the platform directory
/// cannot be determined (should never happen in practice).
#[must_use]
pub fn secure_cache_dir() -> PathBuf {
    let Some(cache_base) = dirs_next::cache_dir() else {
        // Fallback: legacy location
        return std::env::temp_dir().join(LEGACY_CACHE_DIR_NAME);
    };

    #[cfg(target_os = "macos")]
    {
        // ~/Library/Caches/com.uffs/
        cache_base.join("com.uffs")
    }
    #[cfg(target_os = "windows")]
    {
        // %LOCALAPPDATA%\uffs\cache\
        cache_base.join("uffs").join("cache")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // $XDG_CACHE_HOME/uffs/  (default ~/.cache/uffs/)
        cache_base.join("uffs")
    }
}

/// Returns the legacy (insecure) cache directory path.
///
/// Used only during migration.
#[must_use]
fn legacy_cache_dir() -> PathBuf {
    std::env::temp_dir().join(LEGACY_CACHE_DIR_NAME)
}

/// Migrates cache files from the legacy temp-dir location to the new
/// secure directory.
///
/// This runs **once** per process. If the legacy directory contains `.uffs`
/// files, they are moved (renamed) to the new location. The legacy directory
/// is removed afterwards if empty.
///
/// Errors are logged but never propagated — migration is best-effort.
pub fn migrate_legacy_cache() {
    MIGRATION_ONCE.call_once(|| {
        let legacy = legacy_cache_dir();
        let secure = secure_cache_dir();

        // Nothing to migrate if dirs are the same (fallback case) or
        // legacy dir doesn't exist.
        if legacy == secure || !legacy.is_dir() {
            return;
        }

        let Ok(entries) = std::fs::read_dir(&legacy) else {
            return;
        };

        let mut moved = 0_u32;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().ends_with(".uffs") {
                continue;
            }

            // Ensure secure dir exists before first move
            if moved == 0
                && let Err(err) = create_secure_dir(&secure)
            {
                tracing::warn!(
                    path = %secure.display(),
                    error = %err,
                    "Failed to create secure cache dir during migration"
                );
                return;
            }

            if migrate_single_file(&entry.path(), &secure.join(&name)) {
                moved += 1;
            }
        }

        if moved > 0 {
            tracing::info!(
                files = moved,
                from = %legacy.display(),
                to = %secure.display(),
                "Migrated legacy cache files to secure location"
            );
        }

        // Remove legacy dir if now empty
        let _ignore = std::fs::remove_dir(&legacy);
    });
}

/// Move a single `.uffs` file from `src` to `dst`, falling back to
/// copy-then-remove for cross-device moves. Returns `true` on success.
fn migrate_single_file(src: &Path, dst: &Path) -> bool {
    if dst.exists() {
        return false;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => {
            let _ignore = set_file_permissions_owner_only(dst);
            true
        }
        Err(rename_err) => {
            if let Ok(data) = std::fs::read(src)
                && std::fs::write(dst, &data).is_ok()
            {
                let _perm = set_file_permissions_owner_only(dst);
                let _rm = std::fs::remove_file(src);
                return true;
            }
            tracing::debug!(
                src = %src.display(),
                dst = %dst.display(),
                error = %rename_err,
                "Failed to migrate cache file"
            );
            false
        }
    }
}

/// Minimum age before a `.uffs.tmp` file is treated as abandoned by a
/// crashed atomic write and eligible for cleanup.
///
/// A *live* temp can exist for the several seconds a multi-hundred-MB
/// background compact-cache save spends writing before its rename.
/// `cache_dir()` is called concurrently from the per-shard journal
/// loops, so an age-blind sweep raced those in-flight writers: Windows
/// opens the temp with `FILE_SHARE_DELETE`, the sweep's delete
/// succeeded (delete-pending), and the writer's final rename then
/// failed with `ERROR_FILE_NOT_FOUND` (os error 2) — the "Background
/// compact cache save failed" incidents of 2026-08. One hour is far
/// beyond any legitimate in-flight write while still reclaiming
/// crash debris promptly.
const STALE_TEMP_MAX_AGE_SECS: u64 = 3600;

/// `true` when a temp file whose mtime is `modified` counts as
/// abandoned at time `now` (older than [`STALE_TEMP_MAX_AGE_SECS`]).
///
/// A file younger than the threshold — or with a clock-skewed mtime in
/// the future (`duration_since` errors) — is treated as live and kept.
fn is_abandoned_temp(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .is_ok_and(|age| age.as_secs() >= STALE_TEMP_MAX_AGE_SECS)
}

/// Cleans up abandoned `.uffs.tmp` files left behind by crashed atomic
/// writes.
///
/// Only temps older than [`STALE_TEMP_MAX_AGE_SECS`] are removed; a
/// younger temp may be an atomic write in flight on another thread and
/// deleting it would make that writer's rename fail (see the constant's
/// doc for the incident this prevents). Files whose mtime cannot be
/// read are conservatively kept.
fn cleanup_stale_temps(dir: &Path) {
    let now = SystemTime::now();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let abandoned = name.to_string_lossy().ends_with(".uffs.tmp")
                && entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|modified| is_abandoned_temp(modified, now));
            if abandoned {
                let _ignore = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Gets the cache directory path.
///
/// Returns the platform-appropriate secure cache directory. On the first
/// call per process it also migrates any legacy cache files from the old
/// temp-dir location; every call sweeps abandoned `.uffs.tmp` files
/// (older than `STALE_TEMP_MAX_AGE_SECS`, one hour — younger temps may
/// be atomic writes in flight and are left alone).
#[must_use]
pub fn cache_dir() -> PathBuf {
    migrate_legacy_cache();
    let dir = secure_cache_dir();
    // Best-effort cleanup of stale temps (no-op if dir doesn't exist)
    cleanup_stale_temps(&dir);
    dir
}

/// Gets the cache file path for a specific drive.
///
/// Returns `{SECURE_CACHE_DIR}/{DRIVE}_index.uffs`.
#[must_use]
pub fn cache_file_path(drive: crate::platform::DriveLetter) -> PathBuf {
    cache_dir().join(format!("{}_index.uffs", drive.as_char()))
}

/// Checks if a cached index file exists and is fresh (within TTL).
///
/// # Arguments
///
/// * `drive` - Drive letter to check
/// * `ttl_seconds` - TTL in seconds (use `INDEX_TTL_SECONDS` for default)
///
/// # Returns
///
/// `true` if the cache file exists and was modified within the TTL window.
#[must_use]
pub fn is_cache_fresh(drive: crate::platform::DriveLetter, ttl_seconds: u64) -> bool {
    let path = cache_file_path(drive);
    std::fs::metadata(&path).is_ok_and(|meta| {
        meta.modified().is_ok_and(|modified| {
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or(Duration::MAX);
            age.as_secs() < ttl_seconds
        })
    })
}

/// Gets the age of a cache file in seconds.
///
/// Returns `None` if the file doesn't exist or age cannot be determined.
#[must_use]
pub fn cache_age_seconds(drive: crate::platform::DriveLetter) -> Option<u64> {
    let path = cache_file_path(drive);
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    Some(age.as_secs())
}

/// Loads a cached index if it exists and is fresh.
///
/// Acquires a shared (read) lock on the cache file while loading.
///
/// # Arguments
///
/// * `drive` - Drive letter to load
/// * `ttl_seconds` - TTL in seconds
///
/// # Returns
///
/// `Some((index, header))` if cache is fresh, `None` otherwise.
#[must_use]
pub fn load_cached_index(
    drive: crate::platform::DriveLetter,
    ttl_seconds: u64,
) -> Option<(MftIndex, IndexHeader)> {
    if !is_cache_fresh(drive, ttl_seconds) {
        return None;
    }

    let lock_path = cache_lock_path(drive);
    let result = with_file_lock(&lock_path, LockKind::Shared, CACHE_LOCK_TIMEOUT, || {
        let path = cache_file_path(drive);
        match MftIndex::load_from_file(&path) {
            Ok((index, header)) => Ok(Some((index, header))),
            Err(_) => Ok(None),
        }
    });

    match result {
        Ok(Some((index, header))) => {
            // Verify the volume matches
            (header.volume == drive).then_some((index, header))
        }
        _ => None,
    }
}

/// Saves an index to the cache.
///
/// Creates the cache directory with owner-only permissions if it doesn't
/// exist. Acquires an exclusive lock, then uses atomic write (temp file +
/// rename) to prevent partial exposure.
///
/// # Errors
///
/// Returns an error if directory creation, locking, or file writing fails.
pub fn save_to_cache(
    index: &MftIndex,
    drive: crate::platform::DriveLetter,
    volume_serial: u64,
    usn_journal_id: u64,
    next_usn: crate::usn::Usn,
) -> std::io::Result<PathBuf> {
    let dir = cache_dir();
    create_secure_dir(&dir)?;

    let lock_path = cache_lock_path(drive);
    let result = with_file_lock(&lock_path, LockKind::Exclusive, CACHE_LOCK_TIMEOUT, || {
        let path = cache_file_path(drive);
        index.save_to_file(&path, volume_serial, usn_journal_id, next_usn)?;
        Ok(path)
    });

    // Invalidate the companion compact cache so it gets rebuilt from the
    // fresh MftIndex on next access.
    invalidate_compact_cache(drive);

    result
}

/// Serializes the `MftIndex` synchronously and spawns a background thread
/// to compress, encrypt, and write the cache file.
///
/// This is the non-blocking version of [`save_to_cache`].  The serialization
/// (~500ms for 8M records) is done on the calling thread; the heavy work
/// (zstd compress + AES encrypt + disk write) runs in a detached thread.
///
/// The background thread uses [`atomic_write`], so the cache file is either
/// fully written or not written at all — process exit during the write is
/// safe (the cache simply won't exist, triggering a cold read next time).
///
/// # Errors
///
/// Returns an error only if serialization or directory creation fails.
/// Background compression/encryption/write errors are logged but not
/// propagated (best-effort save).
pub fn save_to_cache_background(
    index: &MftIndex,
    drive: crate::platform::DriveLetter,
    volume_serial: u64,
    usn_journal_id: u64,
    next_usn: crate::usn::Usn,
) -> std::io::Result<()> {
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();

    let dir = cache_dir();
    create_secure_dir(&dir)?;

    // Serialize synchronously — fast (~500ms), needs &MftIndex.
    let t_ser = std::time::Instant::now();
    let serialized = index.serialize(volume_serial, usn_journal_id, next_usn);
    let ser_ms = t_ser.elapsed().as_millis();
    if profile {
        #[expect(
            clippy::float_arithmetic,
            reason = "display-only MB conversion for profiling"
        )]
        let mb = usize_to_f64(serialized.len()) / (1_024.0_f64 * 1_024.0_f64);
        tracing::debug!(
            target: "cache_profile",
            ser_ms = %ser_ms,
            mb = %format_args!("{mb:.1}"),
            "mft_serialize"
        );
    }

    // Invalidate compact cache before writing new MftIndex.
    invalidate_compact_cache(drive);

    // Spawn background thread for compress → encrypt → write.
    let path = cache_file_path(drive);
    std::thread::Builder::new()
        .name(format!("mft-save-{drive}"))
        .spawn(move || {
            if let Err(err) = compress_encrypt_write(
                serialized, &path, 3, // ZSTD_LEVEL
                profile, "mft",
            ) {
                tracing::warn!(
                    drive = %drive,
                    error = %err,
                    "Background MFT cache save failed"
                );
            }
        })
        .map_err(|err| std::io::Error::other(format!("spawn failed: {err}")))?;

    Ok(())
}

/// Deletes the compact cache file for a drive (best-effort).
///
/// Called automatically by [`save_to_cache`] to ensure the compact index
/// is rebuilt from the updated `MftIndex`.
fn invalidate_compact_cache(drive: crate::platform::DriveLetter) {
    let compact_path = cache_dir().join(format!("{}_compact.uffs", drive.as_char()));
    if compact_path.exists() {
        if let Err(err) = std::fs::remove_file(&compact_path) {
            tracing::warn!(
                drive = %drive,
                error = %err,
                "⚠️ Failed to invalidate compact cache"
            );
        } else {
            tracing::debug!(
                drive = %drive,
                "🗑️ Compact cache invalidated (MftIndex updated)"
            );
        }
    }
}

/// Securely removes a cached index file for a specific drive.
///
/// Overwrites the file with zeros before deleting. Does nothing if the
/// file doesn't exist.
pub fn remove_cached_index(drive: crate::platform::DriveLetter) {
    let path = cache_file_path(drive);
    let _rm_cache = secure_remove(&path);
    // Also clean up the lock file
    let lock = cache_lock_path(drive);
    let _rm_lock = std::fs::remove_file(lock);
}

/// Securely removes all cached index files and the cache directory.
///
/// Each `.uffs` file is zero-overwritten before deletion. Does nothing
/// if the directory doesn't exist.
pub fn remove_all_cached_indices() {
    let dir = cache_dir();
    // Securely wipe each .uffs file individually
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".uffs") {
                let _ignore = secure_remove(&entry.path());
            }
        }
    }
    // Remove remaining files (.lock, .tmp) and the directory itself
    let _ignore = std::fs::remove_dir_all(dir);
}

/// Lists all cached drive letters.
///
/// Returns a vector of drive letters that have cached indices.
#[must_use]
pub fn list_cached_drives() -> Vec<crate::platform::DriveLetter> {
    let dir = cache_dir();
    let mut drives = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                // Parse "C_index.uffs" -> DriveLetter::C.  Non-letter
                // prefixes (e.g. stray `.tmp` / `.lock` files) fall
                // out via `DriveLetter::parse`.
                if name.ends_with("_index.uffs")
                    && name.len() >= 12
                    && let Some(drive_char) = name.chars().next()
                    && let Ok(letter) = crate::platform::DriveLetter::parse(drive_char)
                {
                    drives.push(letter);
                }
            }
        }
    }

    drives.sort_unstable();
    drives
}

/// Checks if ANY cached index is expired for multi-drive operations.
///
/// For multi-drive operations, if ANY index is expired (or close to expiry),
/// we should rebuild ALL indices to ensure consistency.
///
/// # Arguments
///
/// * `drives` - List of drives to check
/// * `ttl_seconds` - TTL in seconds
///
/// # Returns
///
/// `true` if any drive's cache is missing or expired.
#[must_use]
pub fn any_cache_expired(drives: &[crate::platform::DriveLetter], ttl_seconds: u64) -> bool {
    for &drive in drives {
        if !is_cache_fresh(drive, ttl_seconds) {
            return true;
        }
    }
    false
}

/// Checks if ALL cached indices are expired.
///
/// Used to determine if we should clean up the entire cache directory.
///
/// # Arguments
///
/// * `ttl_seconds` - TTL in seconds
///
/// # Returns
///
/// `true` if all cached indices are expired (or no indices exist).
#[must_use]
pub(crate) fn all_caches_expired(ttl_seconds: u64) -> bool {
    let drives = list_cached_drives();
    if drives.is_empty() {
        return true;
    }

    for drive in drives {
        if is_cache_fresh(drive, ttl_seconds) {
            return false;
        }
    }
    true
}

/// Cleans up the cache directory if all indices are expired.
///
/// Call this periodically to avoid accumulating stale cache files.
pub fn cleanup_expired_cache(ttl_seconds: u64) {
    if all_caches_expired(ttl_seconds) {
        remove_all_cached_indices();
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Security primitives — delegated to uffs-security crate
// ────────────────────────────────────────────────────────────────────────────

pub use uffs_security::fs::{
    FileLock, LockKind, atomic_write, create_secure_dir, secure_remove,
    set_file_permissions_owner_only, with_file_lock,
};

/// Default lock timeout for cache operations (5 seconds).
const CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Returns the lock file path for a drive's cache file.
///
/// E.g. `{SECURE_CACHE_DIR}/C_index.lock`
#[must_use]
pub fn cache_lock_path(drive: crate::platform::DriveLetter) -> PathBuf {
    cache_dir().join(format!("{}_index.lock", drive.as_char()))
}

/// Result of a cache check operation.
// The Fresh variant is intentionally large as it contains the loaded index.
// Boxing would add indirection overhead for the common case.
#[expect(
    clippy::large_enum_variant,
    reason = "Fresh variant is intentionally large; boxing would add indirection overhead"
)]
#[derive(Debug)]
pub enum CacheStatus {
    /// Cache is fresh and ready to use.
    Fresh {
        /// The loaded index.
        index: MftIndex,
        /// The index header with metadata.
        header: IndexHeader,
        /// Age of the cache in seconds.
        age_seconds: u64,
    },
    /// Cache is stale and needs refresh.
    Stale {
        /// Age of the stale cache in seconds (if it exists).
        age_seconds: Option<u64>,
    },
    /// No cache exists for this drive.
    Missing,
}

/// Checks the cache status for a drive.
///
/// This is a high-level function that returns the cache status and optionally
/// loads the index if fresh.
#[must_use]
pub fn check_cache_status(drive: crate::platform::DriveLetter, ttl_seconds: u64) -> CacheStatus {
    let path = cache_file_path(drive);

    // Check if file exists
    let Ok(meta) = std::fs::metadata(&path) else {
        return CacheStatus::Missing;
    };

    // Get age
    let age_seconds = meta
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|dur| dur.as_secs());

    // Check if fresh
    let is_fresh = age_seconds.is_some_and(|age| age < ttl_seconds);

    if is_fresh {
        // Try to load the index
        match MftIndex::load_from_file(&path) {
            Ok((index, header)) => {
                if header.volume == drive {
                    CacheStatus::Fresh {
                        index,
                        header,
                        age_seconds: age_seconds.unwrap_or(0),
                    }
                } else {
                    CacheStatus::Stale { age_seconds }
                }
            }
            Err(_) => CacheStatus::Stale { age_seconds },
        }
    } else {
        CacheStatus::Stale { age_seconds }
    }
}

/// Multi-drive cache status for coordinated operations.
#[derive(Debug)]
pub enum MultiDriveCacheStatus {
    /// All drives have fresh caches.
    AllFresh(Vec<(crate::platform::DriveLetter, MftIndex, IndexHeader)>),
    /// Some or all drives need refresh - rebuild all.
    NeedsRebuild {
        /// Drives that need rebuilding.
        stale_drives: Vec<crate::platform::DriveLetter>,
        /// Drives that are fresh (but will be rebuilt anyway for consistency).
        fresh_drives: Vec<crate::platform::DriveLetter>,
    },
}

/// Checks cache status for multiple drives.
///
/// For multi-drive operations, if ANY drive is stale, we recommend rebuilding
/// ALL drives to ensure consistency.
#[must_use]
pub fn check_multi_drive_cache(
    drives: &[crate::platform::DriveLetter],
    ttl_seconds: u64,
) -> MultiDriveCacheStatus {
    let mut fresh = Vec::new();
    let mut stale = Vec::new();
    let mut indices = Vec::new();

    for &drive in drives {
        match check_cache_status(drive, ttl_seconds) {
            CacheStatus::Fresh { index, header, .. } => {
                fresh.push(drive);
                indices.push((drive, index, header));
            }
            CacheStatus::Stale { .. } | CacheStatus::Missing => {
                stale.push(drive);
            }
        }
    }

    if stale.is_empty() {
        MultiDriveCacheStatus::AllFresh(indices)
    } else {
        MultiDriveCacheStatus::NeedsRebuild {
            stale_drives: stale,
            fresh_drives: fresh,
        }
    }
}

#[cfg(test)]
mod stale_temp_tests {
    use core::time::Duration;
    use std::time::SystemTime;

    use super::{STALE_TEMP_MAX_AGE_SECS, is_abandoned_temp};

    #[test]
    fn fresh_temp_is_kept() {
        // A temp written seconds ago is an atomic write likely still in
        // flight — deleting it would break that writer's rename.
        let now = SystemTime::now();
        let modified = now - Duration::from_secs(5);
        assert!(!is_abandoned_temp(modified, now));
    }

    #[test]
    fn temp_just_under_threshold_is_kept() {
        let now = SystemTime::now();
        let modified = now - Duration::from_secs(STALE_TEMP_MAX_AGE_SECS - 1);
        assert!(!is_abandoned_temp(modified, now));
    }

    #[test]
    fn temp_at_threshold_is_abandoned() {
        let now = SystemTime::now();
        let modified = now - Duration::from_secs(STALE_TEMP_MAX_AGE_SECS);
        assert!(is_abandoned_temp(modified, now));
    }

    #[test]
    fn temp_far_past_threshold_is_abandoned() {
        let now = SystemTime::now();
        let modified = now - Duration::from_secs(STALE_TEMP_MAX_AGE_SECS * 24);
        assert!(is_abandoned_temp(modified, now));
    }

    #[test]
    fn future_mtime_is_kept() {
        // Clock skew: an mtime in the future makes `duration_since` fail;
        // the conservative answer is "live, keep it".
        let now = SystemTime::now();
        let modified = now + Duration::from_secs(60);
        assert!(!is_abandoned_temp(modified, now));
    }
}
