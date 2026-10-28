// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Compact index cache: serialize/deserialize + encrypted disk I/O.
//!
//! Stores `DriveCompactIndex` as zstd-compressed, AES-256-GCM encrypted
//! `{DRIVE}_compact.uffs` alongside the full `.uffs` `MftIndex` cache.
//!
//! **v6** (current): char-trigram CSR stored on disk (keys `u64[]`, offsets
//! `u32[]`, values `u32[]`).  Zero rebuild on load — saves ~220 ms.
//!
//! **v5**: `names_lower` removed from disk — trigram rebuilt from on-the-fly
//! `CaseFold` lowered names on load.  Still accepted; trigram rebuilt.
//!
//! **v4**: trigram index not stored on disk — rebuilt from `names_lower` on
//! load.  Still accepted on load; `names_lower` is read then dropped.
//!
//! **v3**: adds `source_epoch` (u64) to the header.  Still accepted on load;
//! old byte-trigram CSR is skipped, char-trigram rebuilt.
//!
//! **v2**: old byte-trigram posting lists serialized in CSR format.
//! Accepted on load; `source_epoch` defaults to 0 (always stale).
//!
//! **v1** (legacy): rejected — returns error, caller rebuilds.
//! Exception: `file_size_policy` — serialize/deserialize pipeline, tight
//! coupling.

use alloc::borrow::Cow;
use alloc::sync::Arc;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use uffs_security::runtime_dir::{RuntimeDir, mmap_read_only};

use crate::compact::{
    ChildrenIndex, CompactRecord, DriveCompactIndex, ExtensionIndex, IndexSource,
};
use crate::compact_mmap;
use crate::compact_storage::ColumnStorage;
use crate::trigram::TrigramIndex;

/// Structured error variants for [`load_compact_cache`].
///
/// Phase 5 (#96): each failure path that previously collapsed into
/// `Option::None` now surfaces a typed variant.  Callers that only
/// care about success vs failure can still pattern-match on `Ok(_)`
/// vs `Err(_)`, but production logs and the daemon's stale-cache
/// detection logic can now distinguish "cache file missing" from
/// "decryption failed" from "stale by epoch" — the previous silent-
/// `None` return collapsed all eight failure modes into one log line.
///
/// `#[non_exhaustive]` is applied per Phase 5 §5c: cache-format
/// evolution adds new failure modes over time (e.g. a future
/// `UnsupportedFeature(&'static str)` variant when introducing
/// optional column-storage features, or a `MagicMismatch` variant
/// distinct from `ParseError`).  Downstream consumers (currently
/// the daemon's stale-cache detection logic in
/// `uffs-daemon::cache_manager`) match this enum non-exhaustively
/// via `?` propagation and `Result::is_err()` — workspace-wide
/// audit at PR-time confirmed zero external exhaustive matches
/// (refs #192).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadCacheError {
    /// Cache file does not exist on disk yet (typical on first boot
    /// or after a `cache clear`).  Caller should rebuild.
    #[error("compact cache file missing")]
    Missing,

    /// Cache file age exceeds the supplied TTL.  Caller should
    /// rebuild.
    #[error("compact cache stale by TTL: age={age_secs}s, ttl={ttl_secs}s")]
    StaleByTtl {
        /// Cache file age in seconds (computed from mtime).
        age_secs: u64,
        /// Caller-supplied TTL ceiling in seconds.
        ttl_secs: u64,
    },

    /// Cache file mtime is older than the underlying MFT cache
    /// (`.uffs`).  The MFT cache was rebuilt between the compact
    /// cache being saved and now, so the compact must be rebuilt.
    #[error("compact cache older than MFT cache — rebuild required")]
    StaleVsMft,

    /// `source_epoch` in the cache header is older than the live
    /// MFT `build_epoch`.  Caller should rebuild.
    #[error("compact cache stale by epoch: cached={cached_epoch}, current={current_epoch}")]
    StaleByEpoch {
        /// `source_epoch` value embedded in the cache header.
        cached_epoch: u64,
        /// `build_epoch` of the live `MftIndex` the caller passed in.
        current_epoch: u64,
    },

    /// Filesystem I/O error reading the cache file (disk gone, perms,
    /// path traversal blocked, etc.).
    #[error("compact cache I/O failed: {0}")]
    Io(#[from] io::Error),

    /// Cache encryption key unavailable (Keychain access denied,
    /// dev-mode file missing or unreadable, etc.).
    #[error("cache encryption key unavailable: {0}")]
    KeyUnavailable(String),

    /// AES-256-GCM decryption failed (wrong key, MAC mismatch,
    /// truncated ciphertext, unsupported header version).
    #[error("compact cache decryption failed: {0}")]
    DecryptFailed(String),

    /// zstd decompression failed (ciphertext was decrypted but the
    /// inner frame is corrupt).
    #[error("compact cache decompression failed: {0}")]
    DecompressFailed(String),

    /// Header / body parse error (bad magic, version mismatch,
    /// truncated body).  See `parse_compact_header` /
    /// `parse_compact_body` for the canonical messages.
    #[error("compact cache parse error: {0}")]
    ParseError(&'static str),

    /// Runtime tempfile setup failed (path collision, perms, parent
    /// directory missing).  See `compact_runtime_tempfile_path`.
    #[error("runtime tempfile setup failed: {0}")]
    RuntimeTempfile(String),

    /// Final deserialise + mmap step failed (alignment, bounds,
    /// `write_runtime_layout` invariant violation).
    #[error("compact cache deserialise failed: {0}")]
    Deserialize(String),
}

impl LoadCacheError {
    /// `true` when this failure means the on-disk cache file itself is
    /// unusable **permanently** — no retry with the same file can ever
    /// succeed — as opposed to a transient / environmental failure.
    ///
    /// Corruption-class variants are:
    ///
    /// * [`Self::DecryptFailed`] — GCM MAC mismatch: the ciphertext is damaged,
    ///   or it was written under a key that no longer exists (key rotation).
    ///   Either way the file can never be read again.
    /// * [`Self::DecompressFailed`] — decrypted fine but the zstd frame is
    ///   corrupt.
    /// * [`Self::ParseError`] / [`Self::Deserialize`] — the plaintext layout is
    ///   invalid (bad magic, out-of-range fields such as the 2026-08 `bloom
    ///   k_hashes out of range` incident, truncation).
    ///
    /// Everything else (missing, stale, I/O, key *unavailable*) is
    /// recoverable without touching the file and returns `false`.
    /// [`load_compact_cache`] uses this to quarantine a poisoned cache
    /// file so the next rebuild's save starts from a clean slate instead
    /// of every future load failing on the same bytes forever.
    #[must_use]
    pub const fn is_corruption(&self) -> bool {
        matches!(
            self,
            Self::DecryptFailed(_)
                | Self::DecompressFailed(_)
                | Self::ParseError(_)
                | Self::Deserialize(_)
        )
    }
}

/// Convenience alias — [`LoadCacheError`] is the canonical failure
/// type for compact-cache loads.
pub(crate) type LoadCacheResult<T> = Result<T, LoadCacheError>;

/// Magic bytes for compact cache files.
const COMPACT_MAGIC: &[u8; 8] = b"UFFSCOM\0";
/// Current compact cache format version.
/// - v7: `ext_names` table
/// - v8: `path_len: u16` added to `CompactRecord` (uses 2 bytes of former
///   `_pad`)
/// - v9: Phase 4 bloom + path-trie sections appended after `ext_names` (see
///   `compact_cache::filters_io`)
/// - v10: Phase 8 `frs_to_compact: Vec<u32>` section appended after the path
///   trie (`u32 len` + `len * u32` raw values, `u32::MAX` for unmapped slots).
///   This mapping is what the per-shard surgical patch path
///   ([`crate::compact_loader::apply_usn_patch`]) needs to translate USN
///   journal events to compact-record updates without re-reading the MFT.  v <
///   10 caches are rejected at the header check so the daemon does a fresh MFT
///   rebuild and writes a v10 cache; carrying them forward with an empty
///   `frs_to_compact` would silently disable the surgical-patch path.
/// - v12: `file_ref: u64` (the NTFS File Reference `(seq << 48) | frs`) added
///   to `CompactRecord`, growing the row 80 → 88 bytes. It is the identity key
///   for delete-diff and the forensic view. The record array is a bulk
///   `bytemuck` memcpy, so the row-size change alone invalidates older caches
///   at the header version check (a fresh MFT rebuild writes v12).
/// - v13: the MFT parsers now persist the record's `sequence_number`, so
///   `file_ref` carries the real slot-reuse generation instead of `0`. v12
///   caches were written with `file_ref == frs` (seq 0), which makes the
///   snapshot diff blind to deletions (MFT slot numbers are stable across
///   reuse); rejecting them forces a rebuild that captures the sequence number.
const COMPACT_VERSION: u16 = 13;

mod filters_io;
pub mod parked;
pub use parked::{ParkedBody, deserialize_parked_body, load_parked_body};
/// Bytes per `CompactRecord`.
const RECORD_BYTES: usize = size_of::<CompactRecord>();
/// zstd compression level for compact cache.
const ZSTD_LEVEL: i32 = 3;
/// zstd frame magic bytes (little-endian `0xFD2FB528`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Returns the cache file path for a compact index.
#[must_use]
pub fn compact_cache_path(drive_letter: uffs_mft::platform::DriveLetter) -> PathBuf {
    uffs_mft::cache::cache_dir().join(format!("{drive_letter}_compact.uffs"))
}

/// Returns the per-drive USN cursor file path used by the Phase 7
/// per-shard journal loop's [`CursorStore`] to persist the last
/// successfully-applied USN between daemon runs.
///
/// The cursor file lives alongside the compact cache (`<letter>_compact.uffs`)
/// in the same `cache_dir()` so a single owner-only directory holds
/// every per-drive cache artifact, making backup / cleanup
/// (`uffs --daemon forget <drive>`) a single-directory operation.
///
/// Storage format: 8 bytes little-endian `u64`.  See
/// `crate::cache::cursor_store::DiskCursorStore` (in `uffs-daemon`)
/// for the read / write helpers.
///
/// [`CursorStore`]: # "Trait defined in `uffs-daemon::cache::journal_loop`"
#[must_use]
pub fn usn_cursor_path(drive_letter: uffs_mft::platform::DriveLetter) -> PathBuf {
    uffs_mft::cache::cache_dir().join(format!("{drive_letter}_usn.cursor"))
}

/// Remove a stale **directory** at the compact-cache target path, if any.
///
/// # Background — the v0.4.23 legacy layout
///
/// Commit `a88041a30` (v0.4.23, 2026-03-28) introduced
/// [`save_compact_cache`] with the wrong call shape:
///
/// ```ignore
/// uffs_mft::cache::create_secure_dir(&path)?;          // BUG: makes <DRIVE>_compact.uffs a DIRECTORY
/// uffs_mft::cache::atomic_write(&path, &encrypted)?;   // rename(.tmp, dir) → EISDIR on Mac/Linux
/// ```
///
/// `create_secure_dir(<file_path>)` `mkdir -p`'d the cache *file path* as
/// a (always-empty) directory; the subsequent `atomic_write` rename then
/// failed silently with `EISDIR` (`os error 21`) on every Unix host.
/// The fix landed in commit `335028444` (v0.4.27) — the call now passes
/// `path.parent()` — but the empty legacy directories remain on disk for
/// any user who ran v0.4.23 → v0.4.26.  Without this helper every
/// subsequent `save_compact_cache(_background)` continues to fail
/// silently on those drives, the demote→promote cycle finds no on-disk
/// body, and search returns zero results from Parked drives.
///
/// # Behaviour
///
/// * Path doesn't exist (cold-boot first save) → `Ok(())`, no-op.
/// * Path is a regular file (or symlink) → `Ok(())`; [`atomic_write`] will
///   replace it.
/// * Path is an **empty** directory → `remove_dir`, warn-log the recovery,
///   return `Ok(())`.
/// * Path is a **non-empty** directory → return the underlying `ENOTEMPTY`
///   error and error-log; we deliberately refuse to `remove_dir_all` so any
///   future bug producing populated state at this path surfaces loudly instead
///   of silently destroying user data.  The v0.4.23 layout is empirically empty
///   (verified against dogfood Mac filesystem state on 2026-04-28).
///
/// Self-heal: after the first successful `save_compact_cache(_background)`
/// the path is a regular file and this helper is a metadata-only no-op
/// on every subsequent call.
///
/// [`atomic_write`]: uffs_mft::cache::atomic_write
fn purge_legacy_compact_cache_dir(
    path: &Path,
    drive: uffs_mft::platform::DriveLetter,
) -> io::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        // Path doesn't exist — nothing to purge.  `symlink_metadata`
        // (not `metadata`) so we don't follow a symlink and accidentally
        // probe the wrong inode.
        return Ok(());
    };
    if !meta.is_dir() {
        // Already a regular file (or symlink) — `atomic_write`'s rename
        // will replace it.
        return Ok(());
    }
    match std::fs::remove_dir(path) {
        Ok(()) => {
            tracing::warn!(
                drive = %drive,
                path = %path.display(),
                "Removed legacy v0.4.23 compact-cache directory; saving file at this path now",
            );
            Ok(())
        }
        Err(err) => {
            tracing::error!(
                drive = %drive,
                path = %path.display(),
                error = %err,
                "Compact-cache path is a non-empty directory — refusing to remove. \
                 Manual intervention required: inspect and remove the directory at this path.",
            );
            Err(err)
        }
    }
}

/// Process-static counter used by [`compact_runtime_tempfile_path`] to
/// disambiguate re-entrant calls into the same daemon process.
///
/// Without this, the second `load_compact_cache` invocation for a given
/// drive (e.g. a USN-refresh-driven reload, or a Phase 3 demote/promote
/// cycle) would race the previous mmap's `FILE_FLAG_DELETE_ON_CLOSE` /
/// orphan-sweep cleanup and hit `ERROR_FILE_EXISTS` / `EEXIST` from
/// `CreateFileW(CREATE_NEW)` / `OpenOptions::create_new`.  Bumping a
/// monotonic counter sidesteps the race entirely; any leaked tempfiles
/// are caught by the next-startup orphan sweep on Unix and by
/// `FILE_FLAG_DELETE_ON_CLOSE` on Windows.
static RUNTIME_TEMPFILE_SEQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Returns the runtime-tempfile root directory shared by every UFFS
/// daemon process: `<cache_dir>/runtime/`.
///
/// Each daemon owns a per-PID subdirectory under this root
/// (`<runtime_root>/<pid>/`) and the next-startup orphan sweep
/// ([`uffs_security::runtime_dir::RuntimeDir::cleanup_orphans`])
/// reads this exact path to detect and wipe dead-PID leftovers.
///
/// # Notes
///
/// This function does **not** create the directory — that's the
/// caller's responsibility, typically through
/// [`uffs_mft::cache::create_secure_dir`].
#[must_use]
pub fn compact_runtime_root() -> PathBuf {
    uffs_mft::cache::cache_dir().join("runtime")
}

/// Build a unique runtime-tempfile path for `drive_letter` and ensure
/// its parent directory exists with owner-only permissions.
///
/// Layout: `<cache_dir>/runtime/<pid>/<drive>_compact_<seq>.live`.  The
/// per-pid subdir lets the next-startup orphan sweep
/// ([`uffs_security::runtime_dir::RuntimeDir::cleanup_orphans`]) wipe
/// dead-PID leftovers via the cross-platform liveness probe.  The
/// `<seq>` suffix disambiguates re-entrant calls within the same
/// daemon process.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the parent directory cannot be created
/// or if the owner-only permissions cannot be applied (Unix `0o700`,
/// Windows owner-only DACL).
fn compact_runtime_tempfile_path(
    drive_letter: uffs_mft::platform::DriveLetter,
) -> io::Result<PathBuf> {
    let pid = std::process::id();
    let seq = RUNTIME_TEMPFILE_SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let parent = compact_runtime_root().join(pid.to_string());
    uffs_mft::cache::create_secure_dir(&parent)?;
    Ok(parent.join(format!("{drive_letter}_compact_{seq}.live")))
}

/// Serializes the compact index (records, names, children, char-trigram CSR).
///
/// **v7**: `ext_names` table appended after trigram CSR.
/// Format: `ext_count: u32`, then `ext_count` length-prefixed strings
/// (`u16 len` + `len` UTF-8 bytes each).
///
/// **v6**: char-trigram CSR is stored on disk — zero-rebuild on load.
/// Format after children CSR:
///   - `trigram_key_count: u32`
///   - `trigram_keys: u64[key_count]`
///   - `trigram_offsets: u32[key_count + 1]`
///   - `trigram_values_count: u32`
///   - `trigram_values: u32[values_count]`
#[must_use]
pub fn serialize_compact(index: &DriveCompactIndex) -> Vec<u8> {
    let record_count = index.records.len();
    let names_len = index.names.len();

    // Children CSR — already in contiguous layout.
    let (csr_offsets, csr_values) = index.children.as_csr();

    // Trigram CSR.
    let (tri_keys, tri_offsets, tri_values) = index.trigram.as_csr();

    let total = 26 // header: 8 (magic) + 2 (ver) + 4 (rc) + 4 (nl) + 8 (epoch)
        + record_count * RECORD_BYTES
        + names_len
        + csr_offsets.len() * 4
        + csr_values.len() * 4
        + 4                         // trigram_key_count
        + tri_keys.len() * 8        // trigram_keys (u64)
        + tri_offsets.len() * 4     // trigram_offsets (u32)
        + 4                         // trigram_values_count
        + tri_values.len() * 4; // trigram_values (u32)
    let mut buf = Vec::with_capacity(total);

    // Header (26 bytes for v3+)
    buf.extend_from_slice(COMPACT_MAGIC);
    buf.extend_from_slice(&COMPACT_VERSION.to_le_bytes());
    push_u32(&mut buf, record_count);
    push_u32(&mut buf, names_len);
    // v3+: source_epoch
    buf.extend_from_slice(&index.source_epoch.to_le_bytes());

    // Records — single bulk copy via bytemuck (Pod layout = on-disk layout)
    buf.extend_from_slice(bytemuck::cast_slice(&index.records));

    // Names (original case only)
    buf.extend_from_slice(&index.names);

    // Children CSR — bulk cast (u32 slices → &[u8] via bytemuck, zero-copy on
    // LE)
    buf.extend_from_slice(bytemuck::cast_slice(csr_offsets));
    buf.extend_from_slice(bytemuck::cast_slice(csr_values));

    // v6: char-trigram CSR
    push_u32(&mut buf, tri_keys.len());
    buf.extend_from_slice(bytemuck::cast_slice(tri_keys));
    buf.extend_from_slice(bytemuck::cast_slice(tri_offsets));
    push_u32(&mut buf, tri_values.len());
    buf.extend_from_slice(bytemuck::cast_slice(tri_values));

    // v7: `ext_names` table — length-prefixed strings.
    push_u32(&mut buf, index.ext_names.len());
    for name in &index.ext_names {
        let bytes = name.as_bytes();
        let len: u16 = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(bytes.get(..usize::from(len)).unwrap_or(bytes));
    }

    // v9: bloom + path-trie sections.  Use the index's pre-built
    // filters when present (every production-built or freshly-loaded
    // `DriveCompactIndex` has them); fall back to building inline so
    // the on-disk v9 format is always self-consistent regardless of
    // how the in-memory struct was constructed.  `Cow` keeps both
    // arms in a single expression so clippy's `option_if_let_else`
    // is satisfied, and avoids the borrow-vs-build duplication.
    let bloom_section: Cow<'_, crate::bloom::Bloom> = index
        .bloom
        .as_ref()
        .map_or_else(|| Cow::Owned(index.build_bloom()), Cow::Borrowed);
    filters_io::push_bloom_section(&mut buf, bloom_section.as_ref());

    let trie_section: Cow<'_, crate::path_trie::PathTrie> = index
        .path_trie
        .as_ref()
        .map_or_else(|| Cow::Owned(index.build_path_trie()), Cow::Borrowed);
    filters_io::push_trie_section(&mut buf, trie_section.as_ref());

    // v10: frs_to_compact section — `u32 len` + `len * u32` raw
    // values.  An empty mapping (e.g. tests that build the body
    // by struct literal without populating it) writes len=0 with
    // no values; the reader handles that branch symmetrically.
    push_u32(&mut buf, index.frs_to_compact.len());
    buf.extend_from_slice(bytemuck::cast_slice(&index.frs_to_compact));

    buf
}

/// Streaming variant of [`serialize_compact`]: writes the same byte layout
/// directly to any `impl Write` without allocating a contiguous buffer.
///
/// For a 7M-record drive the serialized size is ~1.1 GB.  Piping this into a
/// `zstd::Encoder<Vec<u8>>` reduces peak memory from ~1.3 GB to ~200 MB
/// (the compressed output size).
///
/// # Errors
///
/// Returns an error if any write fails.
pub(crate) fn serialize_compact_to_writer<W: io::Write>(
    index: &DriveCompactIndex,
    writer: &mut W,
) -> io::Result<()> {
    let record_count = index.records.len();
    let names_len = index.names.len();

    let (csr_offsets, csr_values) = index.children.as_csr();
    let (tri_keys, tri_offsets, tri_values) = index.trigram.as_csr();

    // Header (26 bytes for v3+)
    writer.write_all(COMPACT_MAGIC)?;
    writer.write_all(&COMPACT_VERSION.to_le_bytes())?;
    write_u32(writer, record_count)?;
    write_u32(writer, names_len)?;
    writer.write_all(&index.source_epoch.to_le_bytes())?;

    // Records — bulk copy via bytemuck
    writer.write_all(bytemuck::cast_slice(&index.records))?;

    // Names (original case)
    writer.write_all(&index.names)?;

    // Children CSR
    writer.write_all(bytemuck::cast_slice(csr_offsets))?;
    writer.write_all(bytemuck::cast_slice(csr_values))?;

    // v6: char-trigram CSR
    write_u32(writer, tri_keys.len())?;
    writer.write_all(bytemuck::cast_slice(tri_keys))?;
    writer.write_all(bytemuck::cast_slice(tri_offsets))?;
    write_u32(writer, tri_values.len())?;
    writer.write_all(bytemuck::cast_slice(tri_values))?;

    // v7: ext_names table — length-prefixed strings
    write_u32(writer, index.ext_names.len())?;
    for name in &index.ext_names {
        let bytes = name.as_bytes();
        let len: u16 = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(bytes.get(..usize::from(len)).unwrap_or(bytes))?;
    }

    // v9: bloom + path-trie sections.  See `serialize_compact` for
    // the rationale on the `Cow`-based borrow-or-build fallback.
    let bloom_section: Cow<'_, crate::bloom::Bloom> = index
        .bloom
        .as_ref()
        .map_or_else(|| Cow::Owned(index.build_bloom()), Cow::Borrowed);
    filters_io::write_bloom_section(writer, bloom_section.as_ref())?;

    let trie_section: Cow<'_, crate::path_trie::PathTrie> = index
        .path_trie
        .as_ref()
        .map_or_else(|| Cow::Owned(index.build_path_trie()), Cow::Borrowed);
    filters_io::write_trie_section(writer, trie_section.as_ref())?;

    // v10: frs_to_compact section.  Symmetric with the
    // [`Vec<u8>`]-backed [`serialize_compact`] path.
    write_u32(writer, index.frs_to_compact.len())?;
    writer.write_all(bytemuck::cast_slice(&index.frs_to_compact))?;

    writer.flush()?;
    Ok(())
}

/// Write a `usize` as little-endian `u32` to a writer.
///
/// Record/name counts fit in u32 for any realistic MFT (NTFS caps at
/// ~4 G entries per volume), so the saturating `try_from` fallback is
/// unreachable in practice and replaces the previous truncating cast.
fn write_u32<W: io::Write>(writer: &mut W, val: usize) -> io::Result<()> {
    let val_u32 = u32::try_from(val).unwrap_or(u32::MAX);
    writer.write_all(&val_u32.to_le_bytes())
}

/// Deserializes a compact index from raw bytes.
///
/// **v6**: char-trigram CSR on disk — zero-rebuild.
/// **v5**: no trigram on disk — rebuilt with `CaseFold`.
/// **v4**: `names_lower` on disk, trigram rebuilt.
/// **v3/v2**: legacy byte-trigram / old format — trigram rebuilt.
/// **v1**: rejected — returns an error so the caller rebuilds.
///
/// Returns `(DriveCompactIndex, trigram_load_ms)`.
///
/// # Errors
/// Returns an error string if the data is truncated, wrong magic, or v1.
pub fn deserialize_compact(
    data: &[u8],
    drive_letter: uffs_mft::platform::DriveLetter,
) -> Result<(DriveCompactIndex, u128), &'static str> {
    let parsed = parse_compact_body(data, drive_letter)?;
    assemble_compact_index(parsed, |records_bytes, names_bytes| {
        Ok::<_, &'static str>((
            ColumnStorage::from_vec(aligned_vec_from_bytes::<CompactRecord>(records_bytes)),
            ColumnStorage::from_vec(names_bytes.to_vec()),
        ))
    })
}

/// Same as [`deserialize_compact`] but materialises the `records` and
/// `names` columns through a daemon-private runtime tempfile and
/// returns mmap-backed [`ColumnStorage`] views instead of heap copies.
///
/// Phase 2b memory-tiering hot path
/// (`docs/refactor/memory-tiering-implementation-plan.md` §3 Phase 2b
/// Commit D).  The kernel page-cache becomes the resident set for the
/// two largest columns; cold pages are evicted under memory pressure
/// and re-populated lazily on next access — RSS scales with access
/// frequency, not with index size.
///
/// `runtime_dir` is the platform-specific [`RuntimeDir`] implementation
/// (typically [`uffs_security::runtime_dir::DefaultRuntimeDir`]).
/// `runtime_path` is the absolute path of the per-shard runtime
/// tempfile.  The parent directory must exist with owner-only
/// permissions (use [`uffs_mft::cache::create_secure_dir`]).
///
/// # Errors
///
/// Returns an [`std::io::Error`] if:
/// * the cache bytes are truncated, wrong magic, or use a stale/unsupported
///   version (parse error lifted via `std::io::Error::other`);
/// * `runtime_dir.create_owner_only` fails (path collision, permissions, parent
///   missing);
/// * writing the layout into the tempfile fails (I/O / disk-full);
/// * mmap creation fails (the only safe entry: [`mmap_read_only`]);
/// * the layout fails `compact_mmap::load_from_runtime`'s alignment or bounds
///   checks (would indicate a `write_runtime_layout` bug).
pub fn deserialize_compact_into_runtime(
    data: &[u8],
    drive_letter: uffs_mft::platform::DriveLetter,
    runtime_dir: &dyn RuntimeDir,
    runtime_path: &Path,
) -> io::Result<(DriveCompactIndex, u128)> {
    let parsed = parse_compact_body(data, drive_letter).map_err(io::Error::other)?;
    assemble_compact_index(parsed, |records_bytes, names_bytes| {
        let mut runtime_file = runtime_dir.create_owner_only(runtime_path)?;
        let layout = compact_mmap::write_runtime_layout(
            records_bytes,
            names_bytes,
            runtime_file.as_file_mut(),
        )?;
        let mmap = Arc::new(mmap_read_only(&runtime_file)?);
        compact_mmap::load_from_runtime(layout, mmap).map_err(io::Error::other)
    })
}

/// Parsed-but-not-yet-stored view of a compact cache body.
///
/// Holds borrowed byte slices for records + names so the caller can
/// either heap-copy them into a [`Vec`] (legacy heap path) or stream
/// them into a runtime tempfile and mmap (Phase 2b mmap path).  All
/// other columns (children CSR, trigram CSR, ext-names) are small
/// enough to stay heap-resident.
struct ParsedCompactBody<'data> {
    /// Drive letter the cache was built for.
    drive_letter: uffs_mft::platform::DriveLetter,
    /// Source epoch (u64) parsed from the header.
    source_epoch: u64,
    /// Records column as raw bytes — exact multiple of
    /// `size_of::<CompactRecord>()`.
    records_bytes: &'data [u8],
    /// Names column as raw bytes.
    names_bytes: &'data [u8],
    /// Children CSR — already heap-allocated and aligned.
    children: ChildrenIndex,
    /// `Some` if a v6+ trigram CSR was loaded directly from the cache;
    /// `None` if the cache predates v6 and the trigram must be rebuilt
    /// from records + names on the fly.
    trigram_loaded: Option<TrigramIndex>,
    /// `Some` if a v7+ ext-names table was read from the cache; `None`
    /// if the cache predates v7 and the table must be rebuilt.
    ext_names_loaded: Option<Vec<Box<str>>>,
    /// `Some` if a v9+ bloom section was loaded directly from the
    /// cache; `None` if the cache predates v9 and the bloom must be
    /// rebuilt via [`DriveCompactIndex::build_bloom`].
    bloom_loaded: Option<crate::bloom::Bloom>,
    /// `Some` if a v9+ path-trie section was loaded directly from
    /// the cache; `None` if the cache predates v9 and the trie must
    /// be rebuilt via [`DriveCompactIndex::build_path_trie`].
    trie_loaded: Option<crate::path_trie::PathTrie>,
    /// `Some` if a v10+ `frs_to_compact` section was loaded directly
    /// from the cache; `None` if the cache predates v10 (which is
    /// rejected at the header check, so this is `None` only on
    /// future format extensions that revisit the layout).  An
    /// empty `Vec` is *not* `None` — a zero-record drive's mapping
    /// is legitimately empty.
    frs_to_compact_loaded: Option<Vec<u32>>,
    /// Resolved case-fold table for the drive.
    fold: uffs_text::case_fold::CaseFold,
}

/// Pure parser: validates the header + body offsets, then hands back
/// borrowed views into `data` plus the small heap-resident columns
/// (children, optionally trigram + ext-names).  Records and names are
/// returned as raw byte slices so the caller picks the storage variant.
///
/// Mirrors the original [`deserialize_compact`] body byte-for-byte —
/// the only structural change is that records + names stay borrowed
/// instead of being eagerly copied into [`Vec`]s.
fn parse_compact_body(
    data: &[u8],
    drive_letter: uffs_mft::platform::DriveLetter,
) -> Result<ParsedCompactBody<'_>, &'static str> {
    let (source_epoch, body_offset, version) = parse_compact_header(data)?;

    let rc = read_u32(data, 10) as usize;
    let nl = read_u32(data, 14) as usize;
    let re = body_offset + rc * RECORD_BYTES;
    let ne = re + nl;
    let cs = if version >= 5 { ne } else { ne + nl };
    let ce = cs + (rc + 1) * 4;
    if data.len() < ce {
        return Err("compact cache truncated");
    }

    let records_bytes = data.get(body_offset..re).ok_or("truncated records")?;
    let names_bytes = data.get(re..ne).ok_or("truncated names")?;
    let csr_off = data.get(cs..ce).ok_or("truncated CSR")?;
    let cp = read_u32(csr_off, rc * 4);
    let pe = ce + cp as usize * 4;
    if data.len() < pe {
        return Err("truncated CSR postings");
    }
    let cv = data.get(ce..pe).ok_or("CSR OOB")?;
    let children =
        ChildrenIndex::from_csr(aligned_vec_from_bytes(csr_off), aligned_vec_from_bytes(cv));
    let fold = crate::compact::resolve_case_fold(drive_letter);

    if data.len() < pe + 4 {
        return Err("truncated trigram header");
    }
    let tkc = read_u32(data, pe) as usize;
    let (trigram_loaded, after_tri) = if version >= 6 {
        // The v6+ writer ALWAYS emits the full trigram CSR — keys,
        // offsets, and the values count — even for an empty index:
        // `TrigramIndex` maintains the CSR invariant `offsets.len()
        // == keys.len() + 1` in memory, so an empty index still
        // serialises its `[0]` offsets entry plus `vc = 0`.  The old
        // `tkc > 0` fast-path consumed only the 4-byte key count in
        // the empty case, leaving those 8 bytes unread and shifting
        // every later section (ext names, bloom, trie, frs) — the
        // `bloom k_hashes out of range` corruption class that
        // quarantined otherwise-valid caches (2026-08 "drive C for
        // ten days"; 2026-08-24 winbox C/D/S quarantine storm).
        let (ks, ke, oe) = (pe + 4, pe + 4 + tkc * 8, pe + 4 + tkc * 8 + (tkc + 1) * 4);
        if data.len() < oe + 4 {
            return Err("truncated trigram CSR");
        }
        let vc = read_u32(data, oe) as usize;
        let ve = oe + 4 + vc * 4;
        if data.len() < ve {
            return Err("truncated trigram values");
        }
        if tkc > 0 {
            let tk: Vec<u64> = aligned_vec_from_bytes(data.get(ks..ke).ok_or("trigram keys")?);
            let to: Vec<u32> = aligned_vec_from_bytes(data.get(ke..oe).ok_or("trigram offsets")?);
            let tv: Vec<u32> =
                aligned_vec_from_bytes(data.get(oe + 4..ve).ok_or("trigram values")?);
            (Some(TrigramIndex::from_csr(tk, to, tv)), ve)
        } else {
            // Empty on disk: report `None` so the assembler rebuilds
            // from records — adopting the empty index verbatim would
            // silently break substring search on a non-empty drive.
            (None, ve)
        }
    } else {
        (None, pe + 4)
    };

    let (ext_names_loaded, after_ext) = if version >= 7 && data.len() >= after_tri + 4 {
        let (table, end) = read_ext_names_table(data, after_tri);
        (Some(table), end)
    } else {
        (None, after_tri)
    };

    // v9: bloom + path-trie sections.  Both are mandatory in v9; a
    // truncated section signals corruption rather than legacy
    // format.  v < 9 caches simply skip this branch and the
    // assembler rebuilds the filters from records / names.
    let (bloom_loaded, trie_loaded, after_trie) = if version >= 9 {
        let (bloom, after_bloom) = filters_io::read_bloom_section(data, after_ext)?;
        let (trie, after_trie) = filters_io::read_trie_section(data, after_bloom)?;
        (Some(bloom), Some(trie), after_trie)
    } else {
        (None, None, after_ext)
    };

    // v10: frs_to_compact section.  Reject mismatched bytes
    // explicitly so a truncated cache surfaces as a parse error
    // (which the daemon retries by rebuilding from MFT) instead of
    // a silent empty mapping that disables the surgical-patch path.
    let frs_to_compact_loaded = if version >= 10 {
        if data.len() < after_trie + 4 {
            return Err("truncated frs_to_compact len");
        }
        let count = read_u32(data, after_trie) as usize;
        let bytes_len = count
            .checked_mul(4)
            .ok_or("frs_to_compact bytes overflow")?;
        let start = after_trie + 4;
        let end = start
            .checked_add(bytes_len)
            .ok_or("frs_to_compact end overflow")?;
        if data.len() < end {
            return Err("truncated frs_to_compact values");
        }
        let values: Vec<u32> = if count == 0 {
            Vec::new()
        } else {
            aligned_vec_from_bytes(data.get(start..end).ok_or("frs_to_compact slice")?)
        };
        Some(values)
    } else {
        None
    };

    Ok(ParsedCompactBody {
        drive_letter,
        source_epoch,
        records_bytes,
        names_bytes,
        children,
        trigram_loaded,
        ext_names_loaded,
        bloom_loaded,
        trie_loaded,
        frs_to_compact_loaded,
        fold,
    })
}

/// Generic assembler: materialises the records + names columns via the
/// caller-provided `store_columns` closure, then builds the dependent
/// per-record indices (trigram fallback, `ext_names` fallback,
/// `ExtensionIndex`) using the freshly-aligned column slices.
///
/// The closure abstracts over the storage variant:
/// * Heap path ([`deserialize_compact`]): copies bytes into aligned `Vec`s.
/// * Runtime mmap path ([`deserialize_compact_into_runtime`]): writes the bytes
///   into a runtime tempfile, mmaps the result, slices page-aligned regions out
///   as `ColumnStorage::Mmap`.
///
/// `tri_ms` measures the time to materialise the trigram (zero for
/// v6+ caches; rebuild cost for legacy caches).
fn assemble_compact_index<F, E>(
    parsed: ParsedCompactBody<'_>,
    store_columns: F,
) -> Result<(DriveCompactIndex, u128), E>
where
    F: FnOnce(&[u8], &[u8]) -> Result<(ColumnStorage<CompactRecord>, ColumnStorage<u8>), E>,
{
    let (records, names) = store_columns(parsed.records_bytes, parsed.names_bytes)?;

    let tri_start = Instant::now();
    let trigram = parsed
        .trigram_loaded
        .unwrap_or_else(|| TrigramIndex::build(records.as_slice(), names.as_slice(), parsed.fold));
    let tri_ms = tri_start.elapsed().as_millis();

    let ext_names = parsed
        .ext_names_loaded
        .unwrap_or_else(|| rebuild_ext_names(records.as_slice(), names.as_slice(), parsed.fold));

    let ext_t0 = Instant::now();
    let ext_index = ExtensionIndex::build(records.as_slice());
    let ext_build_ms = ext_t0.elapsed().as_millis();
    tracing::info!(
        drive = %parsed.drive_letter,
        entries = ext_index.total_entries(),
        build_ms = ext_build_ms,
        "ExtensionIndex built (cache load)"
    );

    let mut compact_index = DriveCompactIndex {
        letter: parsed.drive_letter,
        records,
        names,
        trigram: Arc::new(trigram),
        children: Arc::new(parsed.children),
        ext_index: Arc::new(ext_index),
        fold: parsed.fold,
        ext_names,
        source: IndexSource::MftFile(PathBuf::from(format!("{}:", parsed.drive_letter))),
        source_epoch: parsed.source_epoch,
        bloom: None,
        path_trie: None,
        // Phase 8 B2: v10 caches embed the FRS → compact_idx mapping
        // directly.  v < 10 caches are rejected at the header check
        // (`parse_compact_header`); the `unwrap_or_default()` here
        // covers the future-format edge case where a new cache
        // version omits the section.
        frs_to_compact: parsed.frs_to_compact_loaded.unwrap_or_default(),
        // Cache load is always delta-free — the on-disk format stores base only
        // (compact before save), so a freshly loaded index has no overlay.
        delta: None,
    };

    // Phase 4 Commit D — v9+ caches embed the bloom + trie directly,
    // so the loader skips the rebuild step.  v ≤ 8 caches were
    // serialised before Phase 4 landed; rebuild from the freshly-
    // loaded records / names / fold / ext_names so the in-memory
    // index always carries valid filters.
    let bloom = parsed
        .bloom_loaded
        .unwrap_or_else(|| compact_index.build_bloom());
    let path_trie = parsed
        .trie_loaded
        .unwrap_or_else(|| compact_index.build_path_trie());
    compact_index.bloom = Some(bloom);
    compact_index.path_trie = Some(path_trie);

    Ok((compact_index, tri_ms))
}

/// Read a length-prefixed `ext_names` table from v7+ compact cache bytes.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from deserialize_compact for clarity"
)]
fn read_ext_names_table(data: &[u8], offset: usize) -> (Vec<Box<str>>, usize) {
    let ext_count = read_u32(data, offset) as usize;
    let mut out = Vec::with_capacity(ext_count);
    let mut pos = offset + 4;
    for _ in 0..ext_count {
        let Some(&lo) = data.get(pos) else { break };
        let Some(&hi) = data.get(pos + 1) else { break };
        let slen = usize::from(u16::from_le_bytes([lo, hi]));
        pos += 2;
        let Some(slice) = data.get(pos..pos + slen) else {
            break;
        };
        out.push(Box::from(core::str::from_utf8(slice).unwrap_or("")));
        pos += slen;
    }
    (out, pos)
}

/// Rebuild `ext_names` from compact records for pre-v7 caches.
#[expect(
    clippy::single_call_fn,
    reason = "legacy v6 fallback — separate concern from v7 deserialization"
)]
fn rebuild_ext_names(
    records: &[CompactRecord],
    names: &[u8],
    fold: uffs_text::case_fold::CaseFold,
) -> Vec<Box<str>> {
    let max_id = records
        .iter()
        .map(|rec| rec.extension_id)
        .max()
        .map_or(0, usize::from);
    let mut table: Vec<Option<Box<str>>> = vec![None; max_id + 1];
    if let Some(slot) = table.get_mut(0) {
        *slot = Some(Box::from(""));
    }
    let mut fold_buf = Vec::with_capacity(64);
    for rec in records {
        let idx = usize::from(rec.extension_id);
        if table.get(idx).is_some_and(Option::is_some) {
            continue;
        }
        let nm = rec.name(names);
        if let Some(dot) = nm.rfind('.')
            && let Some(ext_raw) = nm.get(dot + 1..)
        {
            let folded = fold.fold_into(ext_raw, &mut fold_buf);
            if let Some(slot) = table.get_mut(idx) {
                *slot = Some(Box::from(folded));
            }
        }
    }
    table
        .into_iter()
        .map(|opt| opt.unwrap_or_else(|| Box::from("")))
        .collect()
}

/// Validates magic/version and returns `(source_epoch, body_offset, version)`.
fn parse_compact_header(data: &[u8]) -> Result<(u64, usize, u16), &'static str> {
    if data.len() < 18 {
        return Err("compact cache too short");
    }
    if data.get(..8) != Some(COMPACT_MAGIC.as_slice()) {
        return Err("bad compact magic");
    }
    let version = data
        .get(8..10)
        .and_then(|slice| <[u8; 2]>::try_from(slice).ok())
        .map_or(0, u16::from_le_bytes);
    if version < 11 {
        // Two cache-format breaks gate the v11 floor:
        //   • Phase 8 (v10): added the `frs_to_compact` mapping the surgical
        //     USN patch path needs.
        //   • WI-4.4 (v11): the names blob is now raw WTF-8 (ill-formed NTFS
        //     names retained byte-faithfully) rather than guaranteed UTF-8.
        // Either way a pre-v11 cache must be rebuilt from the MFT; the
        // caller's full-rebuild fallback then writes a fresh v11 cache.
        return Err(
            "stale compact version (v<11 → rebuild for WI-4.4 WTF-8 names / Phase 8 frs_to_compact)",
        );
    }
    if version > COMPACT_VERSION {
        return Err("unsupported compact version (future)");
    }
    if version >= 3 {
        if data.len() < 26 {
            return Err("compact cache truncated (v3 header)");
        }
        let epoch = data
            .get(18..26)
            .and_then(|slice| <[u8; 8]>::try_from(slice).ok())
            .map_or(0, u64::from_le_bytes);
        Ok((epoch, 26, version))
    } else {
        Ok((0, 18, version))
    }
}

// ─── Save / Load ────────────────────────────────────────────────────────────

/// Saves a compact index to its cache file (zstd + AES-256-GCM), blocking.
///
/// Uses streaming serialization — no ~1.1 GB intermediate buffer.
/// Prefer [`save_compact_cache_background`] for non-blocking saves.
///
/// # Errors
/// Returns an error if compression, encryption, or file writing fails.
pub fn save_compact_cache(index: &DriveCompactIndex) -> io::Result<()> {
    save_compact_cache_with_workers(index, uffs_mft::cache::default_zstd_workers())
}

/// [`save_compact_cache`] with an explicit zstd worker count, blocking until
/// the file is written.
///
/// The journal loop's background saves use this with a small count: a save
/// that takes every core competes with the build or copy that caused the
/// churn in the first place (issue #639).  Returning only after the atomic
/// write is what lets a caller persist the USN cursor strictly after the body
/// it describes.
///
/// # Errors
/// Returns an error if `index` still carries a delta overlay, or if
/// compression, encryption, or file writing fails.
pub fn save_compact_cache_with_workers(
    index: &DriveCompactIndex,
    zstd_workers: u32,
) -> io::Result<()> {
    refuse_delta_carrying(index)?;
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let path = compact_cache_path(index.letter);
    if let Some(dir) = path.parent() {
        uffs_mft::cache::create_secure_dir(dir)?;
    }
    // Purge the v0.4.23 legacy directory at the cache file path before
    // `atomic_write` (called inside `compress_encrypt_write_streaming`)
    // hits an EISDIR rename failure.  No-op for cold-boot first saves
    // and for every save after the first successful one.
    purge_legacy_compact_cache_dir(&path, index.letter)?;
    uffs_mft::cache::compress_encrypt_write_streaming_with_workers(
        |encoder| serialize_compact_to_writer(index, encoder),
        &path,
        ZSTD_LEVEL,
        zstd_workers,
        profile,
        "compact",
    )
}

/// Refuse to persist a delta-carrying index.
///
/// A delta-carrying index serialises a children CSR sized for the pre-patch
/// record count while the header carries the post-patch count, mis-aligning
/// every later section for the reader (see
/// [`DriveCompactIndex::fold_delta_for_save`] for the full mechanism and the
/// field incidents).  Callers fold the delta first; this guard turns any path
/// that forgets into a loud error instead of a poisoned cache that
/// quarantines on the next start.
fn refuse_delta_carrying(index: &DriveCompactIndex) -> io::Result<()> {
    if index.delta.is_some() {
        return Err(io::Error::other(
            "refusing to persist a delta-carrying compact index: fold the delta first \
             (DriveCompactIndex::fold_delta_for_save) — its base sections are stale \
             against the patched records and would serialize misaligned",
        ));
    }
    Ok(())
}

/// Streams the compact index into a zstd encoder on the calling thread
/// (no ~1.1 GB intermediate buffer), then spawns a background thread
/// for encryption and disk write.
///
/// Calling-thread work: serialize → zstd compress (~200 MB output).
/// Background-thread work: AES-256-GCM encrypt → atomic disk write.
///
/// Peak memory: ~200 MB (compressed) + ~200 MB (encrypted) = ~400 MB,
/// down from ~1.3 GB with the old `serialize_compact` + `compress` path.
///
/// # Errors
/// Returns an error only if compression or directory creation fails.
pub fn save_compact_cache_background(index: &DriveCompactIndex) -> io::Result<()> {
    // Boundary enforcement of the "on-disk cache is always delta-free"
    // invariant (see `refuse_delta_carrying`).
    refuse_delta_carrying(index)?;
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let t_compress = Instant::now();

    // Serialize directly into the zstd encoder — no 1.1 GB buffer.
    let mut encoder = uffs_mft::cache::new_zstd_mt_encoder(ZSTD_LEVEL)?;
    serialize_compact_to_writer(index, &mut encoder)?;
    let compressed = encoder.finish()?;

    let compress_ms = t_compress.elapsed().as_millis();
    if profile {
        let comp_mb = compressed.len() / (1024 * 1024);
        tracing::debug!(
            target: "cache_profile",
            compress_ms = %compress_ms,
            comp_mb,
            "compact_streaming_ser+compress"
        );
    }

    let path = compact_cache_path(index.letter);
    if let Some(dir) = path.parent() {
        uffs_mft::cache::create_secure_dir(dir)?;
    }
    let drive = index.letter;

    // Background: encrypt + atomic write (only ~200 MB moves to thread).
    std::thread::Builder::new()
        .name(format!("compact-save-{drive}"))
        .spawn(move || {
            if let Err(err) = encrypt_and_write(drive, compressed, &path, profile) {
                tracing::warn!(drive = %drive, error = %err, "Background compact cache save failed");
            }
        })
        .map_err(|err| io::Error::other(format!("spawn failed: {err}")))?;
    Ok(())
}

/// Encrypt compressed cache data and write atomically to disk.
///
/// Extracted from the `save_compact_cache_background` closure to keep
/// cognitive complexity low.
fn encrypt_and_write(
    drive: uffs_mft::platform::DriveLetter,
    compressed: Vec<u8>,
    path: &Path,
    profile: bool,
) -> io::Result<()> {
    let t_enc = Instant::now();
    let key = uffs_security::keystore::get_cache_key()
        .map_err(|err| io::Error::other(format!("cache key unavailable: {err}")))?;
    let encrypted = uffs_security::crypto::encrypt_cache(&compressed, &key)?;
    // Drop compressed — only encrypted needed for write.
    drop(compressed);
    // Purge the v0.4.23 legacy directory at the target path so the
    // following `atomic_write` rename does not hit EISDIR.  Runs on the
    // background thread so the (rare) error path keeps full context.
    purge_legacy_compact_cache_dir(path, drive)?;
    uffs_mft::cache::atomic_write(path, &encrypted)?;
    if profile {
        let enc_write_ms = t_enc.elapsed().as_millis();
        tracing::debug!(
            target: "cache_profile",
            drive = %drive,
            enc_write_ms = %enc_write_ms,
            "compact_bg_encrypt+write"
        );
    }
    Ok(())
}

/// Check whether the compact cache at `path` is still fresh enough to
/// load, based on TTL and (optionally) mtime comparison against the
/// `MftIndex` `.uffs` file.
///
/// Returns `Ok(())` when the cache passes both checks and should be
/// read.  Returns the matching [`LoadCacheError`] variant when the
/// file is missing, older than `ttl_seconds`, or older than the
/// `MftIndex` source.  When `trust_ttl_only` is true the mtime
/// comparison is skipped — the caller vouches that the TTL alone is
/// sufficient freshness evidence.
///
/// Phase 5 (#96): replaces the previous `bool` return so the caller
/// can distinguish "file missing" from "stale by TTL" from
/// "stale vs MFT" without re-reading the metadata.
fn check_compact_cache_freshness(
    path: &Path,
    drive_letter: uffs_mft::platform::DriveLetter,
    ttl_seconds: u64,
    trust_ttl_only: bool,
) -> LoadCacheResult<()> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(LoadCacheError::Missing);
        }
        Err(err) => return Err(LoadCacheError::Io(err)),
    };
    let compact_mtime = meta.modified()?;
    let age = compact_mtime.elapsed().unwrap_or_default();
    let age_secs = age.as_secs();
    if age_secs > ttl_seconds {
        return Err(LoadCacheError::StaleByTtl {
            age_secs,
            ttl_secs: ttl_seconds,
        });
    }
    if trust_ttl_only {
        return Ok(());
    }
    let mft_path = uffs_mft::cache::cache_file_path(drive_letter);
    if let Ok(mft_meta) = std::fs::metadata(&mft_path)
        && let Ok(mft_mtime) = mft_meta.modified()
        && mft_mtime > compact_mtime
    {
        tracing::debug!(
            drive = %drive_letter,
            "Compact cache older than MftIndex cache — rebuilding"
        );
        return Err(LoadCacheError::StaleVsMft);
    }
    Ok(())
}

/// Loads a compact index from its cache file if fresh.
///
/// `mft_build_epoch` is the `build_epoch` of the current `MftIndex`.
/// If the compact cache was built from an older epoch it is considered
/// stale and the matching [`LoadCacheError::StaleByEpoch`] is returned
/// so the caller rebuilds.
///
/// When `trust_ttl_only` is `true`, the mtime comparison against the
/// `MftIndex` `.uffs` file is skipped — only the TTL age check is used.
/// This is useful for hot-path searches where the caller knows the
/// compact cache was just built or the `MftIndex` hasn't changed.
///
/// # Errors
///
/// Returns the matching [`LoadCacheError`] variant on any failure.
/// Phase 5 (#96): the previous `Option<DriveCompactIndex>` API
/// collapsed eight distinct failure modes into a single silent-`None`
/// return; callers can now distinguish e.g. "cache file missing"
/// (cold-boot rebuild) from "decryption failed" (key rotated;
/// rebuild + alert) from "stale by epoch" (incremental refresh).
pub fn load_compact_cache(
    drive_letter: uffs_mft::platform::DriveLetter,
    ttl_seconds: u64,
    mft_build_epoch: u64,
    trust_ttl_only: bool,
) -> LoadCacheResult<DriveCompactIndex> {
    let path = compact_cache_path(drive_letter);
    let result = load_compact_cache_at(
        &path,
        drive_letter,
        ttl_seconds,
        mft_build_epoch,
        trust_ttl_only,
    );
    // Self-heal: a corruption-class failure is permanent — the same bytes
    // will fail every future load (the 2026-08 incident had drive C fail
    // `bloom k_hashes out of range` on every start for ten days).  Move
    // the poisoned file aside so the next load reports `Missing`, the
    // caller rebuilds, and the rebuild's save writes a fresh file.
    if let Err(err) = &result
        && err.is_corruption()
    {
        quarantine_corrupt_cache(&path, drive_letter, err);
    }
    result
}

/// Move an unreadable cache file aside as `<name>.corrupt` (keeping one
/// sample for forensics — how the bytes went bad is itself a bug worth
/// diagnosing), falling back to deletion when the rename fails.
///
/// Best-effort: a quarantine failure is logged and swallowed — the load
/// error the caller already holds is the actionable failure, and the
/// worst case of a failed quarantine is the pre-existing behaviour
/// (the corrupt file stays and the next load fails the same way).
fn quarantine_corrupt_cache(
    path: &Path,
    drive_letter: uffs_mft::platform::DriveLetter,
    err: &LoadCacheError,
) {
    let mut quarantine_name = path.as_os_str().to_owned();
    quarantine_name.push(".corrupt");
    let quarantine = PathBuf::from(quarantine_name);
    // Replace any previous sample — Windows `rename` refuses to
    // overwrite an existing destination.
    let _replaced = std::fs::remove_file(&quarantine);
    match std::fs::rename(path, &quarantine) {
        Ok(()) => {
            tracing::warn!(
                drive = %drive_letter,
                error = %err,
                quarantine = %quarantine.display(),
                "Corrupt compact cache quarantined; next load rebuilds from scratch",
            );
        }
        Err(rename_err) => match std::fs::remove_file(path) {
            Ok(()) => {
                tracing::warn!(
                    drive = %drive_letter,
                    error = %err,
                    "Corrupt compact cache deleted (quarantine rename failed); next load rebuilds",
                );
            }
            Err(remove_err) => {
                tracing::warn!(
                    drive = %drive_letter,
                    error = %err,
                    rename_error = %rename_err,
                    remove_error = %remove_err,
                    "Corrupt compact cache could not be quarantined or deleted",
                );
            }
        },
    }
}

/// Path-parameterised body of [`load_compact_cache`] — split out so the
/// quarantine wrapper above sees every failure with the path in hand,
/// and so tests can exercise the load pipeline against an explicit file.
fn load_compact_cache_at(
    path: &Path,
    drive_letter: uffs_mft::platform::DriveLetter,
    ttl_seconds: u64,
    mft_build_epoch: u64,
    trust_ttl_only: bool,
) -> LoadCacheResult<DriveCompactIndex> {
    check_compact_cache_freshness(path, drive_letter, ttl_seconds, trust_ttl_only)?;

    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let t_total = Instant::now();

    let t_read = Instant::now();
    let raw = std::fs::read(path)?;
    let read_ms = t_read.elapsed().as_millis();
    let raw_len = raw.len();

    let key = uffs_security::keystore::get_cache_key()
        .map_err(|err| LoadCacheError::KeyUnavailable(err.to_string()))?;
    let t_decrypt = Instant::now();
    let decrypted = uffs_security::crypto::decrypt_cache(&raw, &key)
        .map_err(|err| LoadCacheError::DecryptFailed(err.to_string()))?;
    let decrypt_ms = t_decrypt.elapsed().as_millis();

    let t_decompress = Instant::now();
    let is_compressed = decrypted.get(..4).is_some_and(|magic| magic == ZSTD_MAGIC);
    let plaintext = if is_compressed {
        zstd::decode_all(decrypted.as_slice())
            .map_err(|err| LoadCacheError::DecompressFailed(err.to_string()))?
    } else {
        decrypted
    };
    let decompress_ms = t_decompress.elapsed().as_millis();
    let plaintext_len = plaintext.len();

    // Early staleness check — inspect header before full deserialization.
    if mft_build_epoch > 0 {
        let (source_epoch, _, _) =
            parse_compact_header(&plaintext).map_err(LoadCacheError::ParseError)?;
        if source_epoch < mft_build_epoch {
            tracing::debug!(
                target: "cache_profile",
                source_epoch,
                mft_build_epoch,
                "compact: STALE"
            );
            return Err(LoadCacheError::StaleByEpoch {
                cached_epoch: source_epoch,
                current_epoch: mft_build_epoch,
            });
        }
    }

    // Phase 2b: materialise records + names through a daemon-private
    // runtime tempfile so the kernel page-cache (not the heap) becomes
    // the resident set for the two largest columns.  Path is unique
    // per call (atomic sequence) so re-loading the same drive in the
    // same process never hits an `O_EXCL` / `CREATE_NEW` collision.
    let runtime_path = compact_runtime_tempfile_path(drive_letter)
        .map_err(|err| LoadCacheError::RuntimeTempfile(err.to_string()))?;
    let runtime_dir = uffs_security::runtime_dir::DefaultRuntimeDir::default();
    let t_deser = Instant::now();
    let (index, tri_ms) =
        deserialize_compact_into_runtime(&plaintext, drive_letter, &runtime_dir, &runtime_path)
            .map_err(|err| LoadCacheError::Deserialize(err.to_string()))?;
    let deser_ms = t_deser.elapsed().as_millis();

    if profile {
        log_compact_load_profile(&index, &CompactLoadProfile {
            raw_len,
            plaintext_len,
            read_ms,
            decrypt_ms,
            is_compressed,
            decompress_ms,
            deser_ms,
            tri_ms,
            total_ms: t_total.elapsed().as_millis(),
        });
    }
    Ok(index)
}

/// Timing profile for compact-cache loading stages.
struct CompactLoadProfile {
    /// Size of raw (encrypted/compressed) data in bytes.
    raw_len: usize,
    /// Size after decryption in bytes.
    plaintext_len: usize,
    /// Time to read from disk.
    read_ms: u128,
    /// Time to decrypt.
    decrypt_ms: u128,
    /// Whether the data was compressed.
    is_compressed: bool,
    /// Time to decompress (0 if not compressed).
    decompress_ms: u128,
    /// Time to deserialize.
    deser_ms: u128,
    /// Time to build/load trigram index.
    tri_ms: u128,
    /// Total wall-clock time.
    total_ms: u128,
}

/// Emit a `cache_profile` tracing event with compact-load timings.
fn log_compact_load_profile(index: &DriveCompactIndex, profile: &CompactLoadProfile) {
    let raw_mb = profile.raw_len / (1024 * 1024);
    let plain_mb = profile.plaintext_len / (1024 * 1024);
    let tri_label = if profile.tri_ms > 100 {
        "tri_rebuild"
    } else {
        "tri_load"
    };
    tracing::debug!(
        target: "cache_profile",
        read_ms = %profile.read_ms,
        raw_mb,
        decrypt_ms = %profile.decrypt_ms,
        is_compressed = profile.is_compressed,
        decompress_ms = %profile.decompress_ms,
        plain_mb,
        deser_ms = %profile.deser_ms,
        records = index.records.len(),
        tri_label,
        tri_ms = %profile.tri_ms,
        total_ms = %profile.total_ms,
        source_epoch = index.source_epoch,
        "compact_load"
    );
}

// ─── Build-or-load + save ────────────────────────────────────────────────────

/// `ensure_compact_cached` cache-load helper.
///
/// Extracted from [`ensure_compact_cached`] to keep that function's
/// cognitive complexity below clippy's strict-gate ceiling (Phase 5
/// added an extra match arm for #96 structured errors).
fn try_load_for_ensure(
    drive_letter: uffs_mft::platform::DriveLetter,
    build_epoch: u64,
) -> Option<DriveCompactIndex> {
    match load_compact_cache(
        drive_letter,
        super::compact::INDEX_TTL_SECONDS,
        build_epoch,
        false,
    ) {
        Ok(cached) => {
            tracing::debug!(
                target: "cache_profile",
                records = cached.records.len(),
                "compact: loaded from cache"
            );
            Some(cached)
        }
        Err(err) => {
            tracing::debug!(
                target: "cache_profile",
                drive = %drive_letter,
                error = %err,
                "compact: cache miss/stale; building from MftIndex",
            );
            None
        }
    }
}

/// Ensures the compact cache is up-to-date for a given drive.
///
/// - If a fresh compact cache exists on disk → loads and returns it.
/// - Otherwise → builds from the given `MftIndex` → saves → returns.
///
/// Emits `cache_profile` tracing events at `debug` level.
/// The caller may discard the returned index if only the `MftIndex` is needed.
pub fn ensure_compact_cached(
    drive_letter: uffs_mft::platform::DriveLetter,
    mft_index: &uffs_mft::MftIndex,
) -> DriveCompactIndex {
    // Try loading existing compact cache (epoch check catches stale caches).
    // Not TTL-only: we have the MftIndex, so mtime validation is cheap &
    // correct.
    if let Some(cached) = try_load_for_ensure(drive_letter, mft_index.build_epoch) {
        return cached;
    }

    // Build from MftIndex
    let t_build = Instant::now();
    let (compact, build_ms, tri_ms) = crate::compact::build_compact_index(drive_letter, mft_index);
    let total_build_ms = t_build.elapsed().as_millis();

    tracing::debug!(
        target: "cache_profile",
        build_ms = %build_ms,
        records = compact.records.len(),
        tri_ms = %tri_ms,
        total_ms = %total_build_ms,
        "compact_build"
    );

    // Save to disk (best-effort)
    if let Err(err) = save_compact_cache(&compact) {
        tracing::warn!(drive = %drive_letter, error = %err, "Failed to save compact cache");
    } else {
        log_disk_summary(drive_letter);
    }

    compact
}

/// Log on-disk sizes of both MFT and compact caches for a drive.
fn log_disk_summary(drive_letter: uffs_mft::platform::DriveLetter) {
    let compact_path = compact_cache_path(drive_letter);
    let mft_path = uffs_mft::cache::cache_file_path(drive_letter);
    let compact_disk = std::fs::metadata(&compact_path).map_or(0, |meta| meta.len());
    let mft_disk = std::fs::metadata(mft_path).map_or(0, |meta| meta.len());
    let compact_disk_mb = compact_disk / (1024 * 1024);
    let mft_disk_mb = mft_disk / (1024 * 1024);
    let total_disk_mb = compact_disk_mb + mft_disk_mb;
    tracing::debug!(
        target: "cache_profile",
        mft_disk_mb,
        compact_disk_mb,
        total_disk_mb,
        "disk_summary"
    );
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Writes a usize as u32 LE (saturates at `u32::MAX`).
fn push_u32(buf: &mut Vec<u8>, value: usize) {
    buf.extend_from_slice(&uffs_mft::len_to_u32(value).to_le_bytes());
}

/// Read a little-endian u32 from `data` at `offset`.
fn read_u32(data: &[u8], offset: usize) -> u32 {
    data.get(offset..offset + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .map_or(0, u32::from_le_bytes)
}

/// Alignment-safe bulk copy from a `&[u8]` slice into a properly aligned
/// `Vec<T>`.
///
/// Unlike `bytemuck::cast_slice`, this works regardless of the source
/// pointer's alignment. It allocates a `Vec<T>` (which the allocator
/// guarantees to be `align_of::<T>()`-aligned), then copies the raw bytes
/// in via `copy_from_slice`.
///
/// # Panics
///
/// Panics if `bytes.len()` is not an exact multiple of `size_of::<T>()`.
fn aligned_vec_from_bytes<T: bytemuck::Pod>(bytes: &[u8]) -> Vec<T> {
    let elem_size = size_of::<T>();
    assert!(
        elem_size > 0 && bytes.len().is_multiple_of(elem_size),
        "byte slice length {} is not a multiple of element size {}",
        bytes.len(),
        elem_size,
    );
    let count = bytes.len() / elem_size;
    let mut vec = vec![T::zeroed(); count];
    // The Vec<T> is guaranteed aligned by the allocator. Copy raw bytes in.
    let dst = bytemuck::cast_slice_mut::<T, u8>(&mut vec);
    dst.copy_from_slice(bytes);
    vec
}

#[cfg(test)]
mod tests;
