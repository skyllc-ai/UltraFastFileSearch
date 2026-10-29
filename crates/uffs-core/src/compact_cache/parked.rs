// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 4 Commit E — PARKED-tier body loading.
//!
//! A [`ParkedBody`] carries only the bloom + path-trie sections of a
//! v9+ compact cache (~5 – 15 MB per drive on a 7 M-record fixture)
//! — enough to answer the search-skip pre-check without
//! materialising the ~1 GB of records / names / trigram / children
//! CSR that a `Warm`-tier shard holds resident.
//!
//! ## Format
//!
//! Built on top of the v9 [`compact_cache`](super) on-disk format
//! (`COMPACT_VERSION = 9`).  This module does **not** define a new
//! file layout — it reuses the same encrypted `.compact` file and
//! parses out only the tail two sections (bloom + trie) by walking
//! section offsets via header arithmetic, without materialising any
//! of the bulk arrays.
//!
//! v ≤ 8 caches were serialised before bloom + trie persistence
//! landed; loading them as a [`ParkedBody`] returns
//! `Err("parked-body load requires cache format v9+")` so the
//! caller does a full rebuild instead.  Once the cluster has been
//! re-saved with v9, parked-body loads succeed for every shard.
//!
//! ## Memory budget
//!
//! For the canonical 7 M-record / 1 % FPR sizing:
//! * Bloom: ~10 bits/element × 7 M ≈ 8.75 MB.
//! * Trie: ~24 B/dir × ~10 K dirs ≈ 240 KB plus the names buffer (~5 KB/avg dir
//!   × 10 K) ≈ ~5 MB.
//!
//! Total parked footprint per drive: **≈ 10 – 15 MB**, vs the ~1 GB
//! a full `DriveCompactIndex` holds resident.  This is the
//! architectural payoff that makes "7 drives idle ≤ 50 MB" feasible.
//!
//! ## Why a separate submodule
//!
//! Lives in its own file so the parked-only code path is obvious in
//! module-level grep — anything in `parked.rs` is part of the
//! PARKED tier's read path.  Keeps `compact_cache.rs` under the
//! workspace's file-size policy (≤ 1500 lines).

use std::time::Instant;

use uffs_text::case_fold::CaseFold;

use super::{
    COMPACT_VERSION, LoadCacheError, LoadCacheResult, RECORD_BYTES, ZSTD_MAGIC,
    check_compact_cache_freshness, compact_cache_path, filters_io, parse_compact_header,
    quarantine_corrupt_cache, read_u32,
};
use crate::bloom::Bloom;
use crate::path_trie::PathTrie;

/// Resident state of a Parked-tier shard.
///
/// Holds only the filters needed to answer the search-skip
/// pre-check (`bloom`) and directory-prefix queries (`path_trie`)
/// plus the metadata needed to validate freshness on promote
/// (`source_epoch`) and to fold queries the same way the indexer
/// did (`fold`).
///
/// **Not** a `DriveCompactIndex` lite — `ParkedBody` is a separate
/// type because a parked shard *cannot* answer record-level
/// queries.  Promotion to Warm requires re-loading the full cache
/// (or, in a future commit, lazily mmap-ing the records / names
/// columns).
#[derive(Clone)]
pub struct ParkedBody {
    /// Drive letter the cache was built for.
    pub letter: uffs_mft::platform::DriveLetter,
    /// `MftIndex.build_epoch` the source compact cache was built
    /// from — used as a staleness check on promote.
    pub source_epoch: u64,
    /// Bloom over folded basenames + extensions.  See
    /// `crate::compact::DriveCompactIndex::build_bloom`
    /// for the insertion contract that `bloom.contains()` callers
    /// must mirror.
    pub bloom: Bloom,
    /// Directory-only path trie.  See [`PathTrie`] for the lookup
    /// surface (`lookup_path`, `children_of`, `full_path`).
    pub path_trie: PathTrie,
    /// Case-fold table the source index was built against.  Query
    /// callers must fold their input through this before probing
    /// `bloom`.
    pub fold: CaseFold,
}

impl ParkedBody {
    /// Approximate resident footprint in bytes — bloom bits + trie
    /// columns.  Excludes the `Self` struct overhead and any inline
    /// `CaseFold` static (the table is `&'static [u16]`, not heap).
    #[must_use]
    pub const fn size_bytes(&self) -> usize {
        self.bloom.size_bytes() + self.path_trie.size_bytes()
    }
}

impl core::fmt::Debug for ParkedBody {
    /// Manual impl because [`CaseFold`] does not derive `Debug`
    /// (it's a thin `&'static [u16]` wrapper from `uffs-text`
    /// designed to stay out of `Debug` chains).  The other four
    /// fields print directly; `fold` collapses to a stable
    /// placeholder so test diagnostics still surface useful state.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParkedBody")
            .field("letter", &self.letter)
            .field("source_epoch", &self.source_epoch)
            .field("bloom", &self.bloom)
            .field("path_trie", &self.path_trie)
            .field("fold", &"<CaseFold>")
            .finish()
    }
}

/// Parse only the v9 bloom + trie sections out of a serialised
/// compact cache, skipping every other section without allocating.
///
/// # Errors
///
/// Returns `Err("…")` on:
/// * a malformed header (bad magic, unsupported version);
/// * a pre-v9 cache (the bloom + trie sections don't exist in v ≤ 8);
/// * any truncation between the header and the bloom + trie tail (each section
///   boundary is bounds-checked);
/// * filter-section invariant violations (see `filters_io::read_bloom_section`
///   / `filters_io::read_trie_section`).
pub fn deserialize_parked_body(
    data: &[u8],
    drive_letter: uffs_mft::platform::DriveLetter,
) -> Result<ParkedBody, &'static str> {
    let (filters_offset, source_epoch) = find_v9_filters_offset(data)?;
    let (bloom, after_bloom) = filters_io::read_bloom_section(data, filters_offset)?;
    let (path_trie, _after_trie) = filters_io::read_trie_section(data, after_bloom)?;
    let fold = crate::compact::resolve_case_fold(drive_letter);

    Ok(ParkedBody {
        letter: drive_letter,
        source_epoch,
        bloom,
        path_trie,
        fold,
    })
}

/// Locate the byte offset where the v9 bloom + trie sections begin,
/// using only header arithmetic — no allocations for records,
/// names, children CSR, trigram CSR, or `ext_names`.
///
/// Returns `(filters_offset, source_epoch)`.
///
/// # Errors
///
/// Same surface as [`deserialize_parked_body`] up to the filter
/// sections (which are read by `filters_io`, not by this function).
fn find_v9_filters_offset(data: &[u8]) -> Result<(usize, u64), &'static str> {
    let (source_epoch, body_offset, version) = parse_compact_header(data)?;
    if version < 9 {
        return Err("parked-body load requires cache format v9+");
    }
    // `version <= COMPACT_VERSION` is enforced by `parse_compact_header`.
    debug_assert!(
        version <= COMPACT_VERSION,
        "parse_compact_header should have rejected future versions"
    );

    // Records + names headers are at fixed offsets 10 + 14 (post-magic).
    let record_count = read_u32(data, 10) as usize;
    let names_len = read_u32(data, 14) as usize;
    let records_end = body_offset
        .checked_add(
            record_count
                .checked_mul(RECORD_BYTES)
                .ok_or("rc*RB overflow")?,
        )
        .ok_or("records_end overflow")?;
    let names_end = records_end
        .checked_add(names_len)
        .ok_or("names_end overflow")?;

    // v >= 9 implies version >= 5, so the v ≤ 4 `names_lower`
    // duplicate is never present.
    let csr_off_start = names_end;
    let csr_off_end = csr_off_start
        .checked_add(record_count.checked_add(1).ok_or("rc+1 overflow")? * 4)
        .ok_or("csr_off_end overflow")?;
    if data.len() < csr_off_end {
        return Err("compact cache truncated (CSR header)");
    }

    // The CSR-postings count is stored as the last `u32` of the
    // offsets array — the standard CSR convention.
    let postings_count = read_u32(data, csr_off_end - 4) as usize;
    let postings_end = csr_off_end
        .checked_add(postings_count.checked_mul(4).ok_or("postings*4 overflow")?)
        .ok_or("postings_end overflow")?;
    if data.len() < postings_end + 4 {
        return Err("truncated trigram header");
    }

    // Trigram CSR (always present for v >= 6, INCLUDING when empty:
    // the writer serialises the in-memory CSR invariant `offsets.len()
    // == keys.len() + 1`, so an empty index still emits its `[0]`
    // offsets entry plus a zero values count).  The old `key_count >
    // 0` fast-path treated the count word as a bare "no trigram"
    // sentinel — a shape no writer ever produced — and left 8 bytes
    // unconsumed, shifting the ext-names/bloom/trie reads that follow
    // (the `bloom k_hashes out of range` corruption class; see
    // `parse_compact_body` for the full incident note).
    let trigram_key_count = read_u32(data, postings_end) as usize;
    let after_trigram = {
        let keys_end = postings_end
            .checked_add(4)
            .and_then(|x| x.checked_add(trigram_key_count.checked_mul(8)?))
            .ok_or("trigram keys_end overflow")?;
        let offsets_end = keys_end
            .checked_add(trigram_key_count.checked_add(1).ok_or("tkc+1 overflow")? * 4)
            .ok_or("trigram offsets_end overflow")?;
        if data.len() < offsets_end + 4 {
            return Err("truncated trigram CSR");
        }
        let values_count = read_u32(data, offsets_end) as usize;
        let values_end = offsets_end
            .checked_add(4)
            .and_then(|x| x.checked_add(values_count.checked_mul(4)?))
            .ok_or("trigram values_end overflow")?;
        if data.len() < values_end {
            return Err("truncated trigram values");
        }
        values_end
    };

    // Skip the v7+ ext_names table without allocating.  v9 always
    // emits this section (possibly empty).
    let after_ext = skip_ext_names_table(data, after_trigram)?;

    Ok((after_ext, source_epoch))
}

/// Skip past a v7+ length-prefixed `ext_names` table without
/// materialising any strings.
///
/// On-disk layout (mirrors [`super::serialize_compact`] /
/// [`super::serialize_compact_to_writer`] / `super::read_ext_names_table`):
/// * `u32` count of entries
/// * for each entry: `u16` length followed by `length` UTF-8 bytes
///
/// # Errors
///
/// Returns `Err("…")` if any header or string body extends past the
/// end of `data`.
fn skip_ext_names_table(data: &[u8], offset: usize) -> Result<usize, &'static str> {
    if data.len() < offset + 4 {
        return Err("truncated ext_names header");
    }
    let count = read_u32(data, offset) as usize;
    let mut pos = offset.checked_add(4).ok_or("ext_names header overflow")?;
    for _ in 0..count {
        let lo = data
            .get(pos)
            .copied()
            .ok_or("truncated ext_names entry header")?;
        let hi = data
            .get(pos + 1)
            .copied()
            .ok_or("truncated ext_names entry header")?;
        let slen = usize::from(u16::from_le_bytes([lo, hi]));
        pos = pos
            .checked_add(2)
            .ok_or("ext_names cursor overflow (length)")?;
        let entry_end = pos
            .checked_add(slen)
            .ok_or("ext_names cursor overflow (body)")?;
        if data.len() < entry_end {
            return Err("truncated ext_names entry body");
        }
        pos = entry_end;
    }
    Ok(pos)
}

/// Per-stage timings for a parked-body load — read, decrypt,
/// decompress.  Captured by [`read_decompressed_plaintext`] and
/// emitted via [`emit_parked_load_profile`] when
/// `UFFS_CACHE_PROFILE` is set.
struct ParkedLoadIoTimings {
    /// Disk-read wall time (ms).
    read_ms: u128,
    /// Encrypted-file size on disk (bytes).
    raw_len: usize,
    /// AES-256-GCM decrypt wall time (ms).
    decrypt_ms: u128,
    /// `true` if the plaintext started with the zstd frame magic.
    is_compressed: bool,
    /// zstd decompress wall time (ms; `0` if `is_compressed` is
    /// `false`).
    decompress_ms: u128,
    /// Decompressed plaintext size (bytes).
    plaintext_len: usize,
}

/// Read + decrypt + (optionally) decompress a compact cache file.
///
/// Returns the structured [`LoadCacheError`] on failure so the caller
/// can tell a transient miss (IO error, key unavailable) from
/// corruption-class failures (decrypt / decompress) that warrant
/// quarantining the file — mirroring `load_compact_cache`'s pipeline.
fn read_decompressed_plaintext(
    path: &std::path::Path,
) -> LoadCacheResult<(Vec<u8>, ParkedLoadIoTimings)> {
    let t_read = Instant::now();
    let raw = std::fs::read(path)?;
    let read_ms = t_read.elapsed().as_millis();
    let raw_len = raw.len();

    let key = uffs_security::keystore::get_cache_key()
        .map_err(|err| LoadCacheError::KeyUnavailable(err.to_string()))?;
    let t_decrypt = Instant::now();
    let decrypted = uffs_security::crypto::decrypt_cache_in_place(raw, &key)
        .map_err(|err| LoadCacheError::DecryptFailed(err.to_string()))?;
    let decrypt_ms = t_decrypt.elapsed().as_millis();

    let t_decompress = Instant::now();
    let is_compressed = decrypted.get(..4).is_some_and(|magic| magic == ZSTD_MAGIC);
    let plaintext = if is_compressed {
        uffs_mft::cache::decompress_sized(&decrypted)
            .map_err(|err| LoadCacheError::DecompressFailed(err.to_string()))?
    } else {
        decrypted
    };
    let decompress_ms = t_decompress.elapsed().as_millis();
    let plaintext_len = plaintext.len();

    Ok((plaintext, ParkedLoadIoTimings {
        read_ms,
        raw_len,
        decrypt_ms,
        is_compressed,
        decompress_ms,
        plaintext_len,
    }))
}

/// `true` if the plaintext's `source_epoch` is older than the
/// caller-supplied `mft_build_epoch`, signalling a stale cache.
///
/// `mft_build_epoch == 0` short-circuits to "not stale" — that's
/// the "no MFT epoch known" sentinel the daemon uses on first
/// boot.  A malformed header is also treated as "not stale" since
/// the subsequent full parse will surface the corruption with a
/// real error.
fn is_compact_cache_stale_for_epoch(
    plaintext: &[u8],
    mft_build_epoch: u64,
    drive_letter: uffs_mft::platform::DriveLetter,
) -> bool {
    if mft_build_epoch == 0 {
        return false;
    }
    let Ok((source_epoch, _, _)) = parse_compact_header(plaintext) else {
        return false;
    };
    if source_epoch >= mft_build_epoch {
        return false;
    }
    tracing::debug!(
        target: "cache_profile",
        source_epoch,
        mft_build_epoch,
        "parked: STALE"
    );
    tracing::debug!(
        drive = %drive_letter,
        compact_epoch = source_epoch,
        mft_epoch = mft_build_epoch,
        "Compact cache stale (source_epoch < mft build_epoch) — rebuilding"
    );
    true
}

/// Emit a `cache_profile` tracing event with parked-load timings
/// when `UFFS_CACHE_PROFILE` is set.
fn emit_parked_load_profile(
    body: &ParkedBody,
    io: &ParkedLoadIoTimings,
    deser_ms: u128,
    total_ms: u128,
) {
    let raw_mb = io.raw_len / (1024 * 1024);
    let plain_mb = io.plaintext_len / (1024 * 1024);
    let parked_mb = body.size_bytes() / (1024 * 1024);
    tracing::debug!(
        target: "cache_profile",
        read_ms = %io.read_ms,
        raw_mb,
        decrypt_ms = %io.decrypt_ms,
        is_compressed = io.is_compressed,
        decompress_ms = %io.decompress_ms,
        plain_mb,
        deser_ms = %deser_ms,
        parked_mb,
        source_epoch = body.source_epoch,
        total_ms = %total_ms,
        "parked_load"
    );
}

/// Load + decrypt + decompress a compact cache file from disk and
/// return only its bloom + trie as a [`ParkedBody`].
///
/// Mirrors [`super::load_compact_cache`]'s IO + crypto pipeline but
/// skips bulk-array materialisation entirely — no runtime tempfile,
/// no records / names allocation, no trigram / children CSR
/// allocation.  The caller decides via tier transitions when to
/// load the parked body vs the full body.
///
/// `ttl_seconds`, `mft_build_epoch`, and `trust_ttl_only` mirror
/// [`super::load_compact_cache`]'s freshness contract exactly so the
/// two loaders are interchangeable on the staleness front.
///
/// Returns `None` on any failure (cache missing, key missing,
/// decrypt fail, decompress fail, version < 9, parse error).  This
/// matches `load_compact_cache`'s "any failure → caller rebuilds"
/// contract — including its self-heal: a corruption-class failure
/// (decrypt / decompress / parse) quarantines the poisoned file as
/// `<name>.corrupt` so the next load rebuilds from scratch instead
/// of failing on the same bytes forever.
#[must_use]
pub fn load_parked_body(
    drive_letter: uffs_mft::platform::DriveLetter,
    ttl_seconds: u64,
    mft_build_epoch: u64,
    trust_ttl_only: bool,
) -> Option<ParkedBody> {
    let path = compact_cache_path(drive_letter);
    // `load_parked_body` keeps its `Option` return for now (#96 scope
    // limited the Result conversion to `load_compact_cache`); fold any
    // structured `LoadCacheError` into a debug-logged `None`.
    if let Err(err) =
        check_compact_cache_freshness(&path, drive_letter, ttl_seconds, trust_ttl_only)
    {
        tracing::debug!(
            drive = %drive_letter,
            error = %err,
            "parked-body load: freshness check failed",
        );
        return None;
    }

    let t_total = Instant::now();
    let (plaintext, io) = match read_decompressed_plaintext(&path) {
        Ok(loaded) => loaded,
        Err(err) => {
            // Corruption-class failures (decrypt / decompress) are
            // permanent for these bytes: quarantine the file so the next
            // load rebuilds, exactly like `load_compact_cache` does.
            // Transient failures (IO, key unavailable) leave it alone.
            if err.is_corruption() {
                quarantine_corrupt_cache(&path, drive_letter, &err);
            } else {
                tracing::debug!(
                    drive = %drive_letter,
                    error = %err,
                    "parked-body load: IO/crypto pipeline failed",
                );
            }
            return None;
        }
    };
    if is_compact_cache_stale_for_epoch(&plaintext, mft_build_epoch, drive_letter) {
        return None;
    }

    let t_deser = Instant::now();
    let body = match deserialize_parked_body(&plaintext, drive_letter) {
        Ok(body) => body,
        Err(parse_err) => {
            // Decrypted + decompressed fine but the layout is invalid
            // (this is where a poisoned bloom section — the 2026-08
            // `k_hashes out of range` incident — surfaces on the parked
            // path).  Same permanent-failure contract as above.
            quarantine_corrupt_cache(&path, drive_letter, &LoadCacheError::ParseError(parse_err));
            return None;
        }
    };
    let deser_ms = t_deser.elapsed().as_millis();

    if std::env::var_os("UFFS_CACHE_PROFILE").is_some() {
        emit_parked_load_profile(&body, &io, deser_ms, t_total.elapsed().as_millis());
    }

    Some(body)
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use std::path::PathBuf;

    use super::*;
    use crate::compact::{
        ChildrenIndex, CompactRecord, DriveCompactIndex, ExtensionIndex, IndexSource,
    };
    use crate::compact_storage::ColumnStorage;
    use crate::trigram::TrigramIndex;

    /// Build a small but realistic `DriveCompactIndex` with a
    /// directory ("`C`") and two children ("`Cargo.toml`" file +
    /// "`src`" dir) so the bloom carries multiple inserted names
    /// and the trie has at least one parent → child edge.
    fn make_test_index() -> DriveCompactIndex {
        // Names blob: "C" [0..1] "Cargo.toml" [1..11] "src" [11..14].
        let names = b"CCargo.tomlsrc".to_vec();
        let records = vec![
            CompactRecord {
                name_offset: 0,
                flags: 0x0010, // directory
                parent_idx: u32::MAX,
                name_len: 1,
                name_first_byte: b'C',
                ..CompactRecord::default()
            },
            CompactRecord {
                name_offset: 1,
                parent_idx: 0,
                name_len: 10,
                extension_id: 1,
                name_first_byte: b'C',
                ..CompactRecord::default()
            },
            CompactRecord {
                name_offset: 11,
                flags: 0x0010, // directory
                parent_idx: 0,
                name_len: 3,
                name_first_byte: b's',
                ..CompactRecord::default()
            },
        ];
        let fold = CaseFold::default_table();
        let trigram = TrigramIndex::build(&records, &names, fold);
        let children = ChildrenIndex::build(&records);
        let ext_index = ExtensionIndex::build(&records);
        let mut index = DriveCompactIndex {
            letter: uffs_mft::platform::DriveLetter::C,
            records: ColumnStorage::from_vec(records),
            names: ColumnStorage::from_vec(names),
            trigram: Arc::new(trigram),
            children: Arc::new(children),
            ext_index: Arc::new(ext_index),
            fold,
            ext_names: vec![Box::from(""), Box::from("toml")],
            source: IndexSource::MftFile(PathBuf::from("C:")),
            source_epoch: 1234,
            bloom: None,
            path_trie: None,
            frs_to_compact: Vec::new(),
            delta: None,
        };
        index.bloom = Some(index.build_bloom());
        index.path_trie = Some(index.build_path_trie());
        index
    }

    /// Round-trip: a v9 cache produced by [`super::super::serialize_compact`]
    /// must yield a [`ParkedBody`] whose bloom contains every
    /// folded basename and whose trie is byte-identical to the
    /// source's pre-built one.
    #[test]
    fn parked_round_trip_preserves_bloom_and_trie() {
        let index = make_test_index();
        let serialized = super::super::serialize_compact(&index);

        let body = deserialize_parked_body(&serialized, uffs_mft::platform::DriveLetter::C)
            .expect("parked deser");

        // Bloom: every record's folded basename hits.
        let mut fold_buf: Vec<u8> = Vec::new();
        for record in &index.records {
            let start = record.name_offset as usize;
            let end = start + usize::from(record.name_len);
            let bytes = index.names.get(start..end).expect("record name slice");
            let name = core::str::from_utf8(bytes).expect("UTF-8 fixture name");
            fold_buf.clear();
            let folded = index.fold.fold_into(name, &mut fold_buf);
            assert!(
                body.bloom.contains(folded.as_bytes()),
                "parked bloom missed {name:?} -> {folded:?}",
            );
        }

        // Trie: identical structure to the source's pre-built one.
        let expected = index.path_trie.as_ref().expect("source trie");
        assert_eq!(body.path_trie.nodes().len(), expected.nodes().len());
        assert_eq!(body.path_trie.names(), expected.names());
        assert_eq!(body.path_trie.child_offsets(), expected.child_offsets());
        assert_eq!(body.path_trie.child_indices(), expected.child_indices());

        // Metadata: epoch + letter + fold round-trip.
        assert_eq!(body.letter, uffs_mft::platform::DriveLetter::C);
        assert_eq!(body.source_epoch, 1234);
    }

    /// Pre-v10 caches don't carry the Phase 8 `frs_to_compact`
    /// mapping; the cross-cutting [`super::super::parse_compact_header`]
    /// rejection covers the parked-body load too.  Patching the
    /// version byte to 8 must surface the modern "v<10 → rebuild"
    /// error string.
    #[test]
    fn parked_load_rejects_pre_v10_caches() {
        let index = make_test_index();
        let mut serialized = super::super::serialize_compact(&index);

        serialized
            .get_mut(8..10)
            .expect("buffer too short for version")
            .copy_from_slice(&8_u16.to_le_bytes());

        let err = deserialize_parked_body(&serialized, uffs_mft::platform::DriveLetter::C)
            .expect_err("v8 must reject");
        assert!(
            err.contains("stale compact version"),
            "error should mention 'stale compact version': {err:?}",
        );
    }

    /// Magic-byte corruption is rejected by `parse_compact_header`
    /// before any section is read.
    #[test]
    fn parked_load_rejects_bad_magic() {
        let index = make_test_index();
        let mut serialized = super::super::serialize_compact(&index);

        // Flip the first magic byte.
        let byte = serialized.get_mut(0).expect("non-empty buffer");
        *byte = byte.wrapping_add(1);

        let _err = deserialize_parked_body(&serialized, uffs_mft::platform::DriveLetter::C)
            .expect_err("bad magic must reject");
    }

    /// Truncating the buffer at successively shorter prefixes must
    /// surface an error from whichever offset-arithmetic /
    /// section-read routine first runs out of bytes.  Pins the
    /// "no panic on corrupt input" contract.
    ///
    /// **Phase 8.** The v10 `frs_to_compact` section lives at the
    /// tail of the cache and is *not* read by the parked-body load
    /// path (parked bodies only need bloom + trie).  The prefix
    /// sweep therefore stops at the parked-body cutoff
    /// (`serialized.len() - frs_to_compact_section_bytes`); any
    /// prefix beyond that boundary only damages bytes the parker
    /// load deliberately skips and would not be rejected by design.
    #[test]
    fn parked_load_rejects_truncated_at_every_prefix() {
        let index = make_test_index();
        let serialized = super::super::serialize_compact(&index);

        // v10 frs_to_compact section size: 4-byte count + 4 bytes per
        // entry.  The parked body is complete just before this section.
        let frs_to_compact_bytes = 4 + index.frs_to_compact.len() * 4;
        let parked_end = serialized
            .len()
            .checked_sub(frs_to_compact_bytes)
            .expect("v10 cache must include the frs_to_compact section");

        // Sample header offsets + four positions strictly inside the
        // parked-body region.  Full-length parked region is the
        // success case and is excluded.
        let prefixes = [
            0,
            8,  // post-magic, pre-version
            10, // post-version
            18, // pre-epoch (v3+)
            26, // post-header
            parked_end / 4,
            parked_end / 2,
            parked_end * 3 / 4,
            parked_end - 1,
        ];
        for &prefix in &prefixes {
            let truncated = serialized.get(..prefix).expect("prefix in range");
            let result = deserialize_parked_body(truncated, uffs_mft::platform::DriveLetter::C);
            assert!(
                result.is_err(),
                "prefix {prefix}/{parked_end} (full {full}) must be rejected (got {result:?})",
                full = serialized.len(),
            );
        }
    }

    /// Empty `ext_names` table is well-formed and must skip cleanly.
    /// Pins the zero-extension edge case (drives with no files yet
    /// catalogued by extension still emit a 0-count header).
    #[test]
    fn skip_ext_names_table_handles_empty_table() {
        // Manual layout: `[count = 0u32]`.
        let buf = 0_u32.to_le_bytes();
        let after = skip_ext_names_table(&buf, 0).expect("empty table is valid");
        assert_eq!(after, 4, "empty table cursor advances past the count");
    }

    /// A truncated entry header (two bytes after the count claims
    /// the entry but the buffer ends mid-`u16`) is rejected.
    #[test]
    fn skip_ext_names_table_rejects_truncated_entry_header() {
        // `[count = 1u32][0xAA]` — claims one entry but only one of
        // the two length bytes is present.
        let mut buf = 1_u32.to_le_bytes().to_vec();
        buf.push(0xAA);
        let err = skip_ext_names_table(&buf, 0).expect_err("truncated entry header");
        assert!(
            err.contains("ext_names entry header"),
            "expected entry-header error: {err:?}",
        );
    }

    /// `ParkedBody::size_bytes` returns the sum of the bloom +
    /// trie footprints — non-zero, and within an envelope of the
    /// source index's filter sizes (round-trip is byte-exact for
    /// the trie and round-tripped for the bloom).
    #[test]
    fn parked_body_size_bytes_is_nonzero_and_bounded() {
        let index = make_test_index();
        let serialized = super::super::serialize_compact(&index);
        let body = deserialize_parked_body(&serialized, uffs_mft::platform::DriveLetter::C)
            .expect("parked deser");

        let bloom_bytes = body.bloom.size_bytes();
        let trie_bytes = body.path_trie.size_bytes();

        assert!(bloom_bytes > 0, "bloom must be non-empty");
        assert!(trie_bytes > 0, "trie must be non-empty (1 dir at minimum)");
        assert_eq!(body.size_bytes(), bloom_bytes + trie_bytes);
    }
}

#[cfg(test)]
mod pipeline_error_tests {
    use super::*;

    #[test]
    fn missing_file_is_transient_io_not_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T_compact.uffs");

        let err = read_decompressed_plaintext(&path).err().unwrap();

        assert!(
            matches!(err, LoadCacheError::Io(_)),
            "missing file must surface as Io, got: {err}"
        );
        assert!(!err.is_corruption());
    }

    #[test]
    fn garbage_bytes_classify_as_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T_compact.uffs");
        std::fs::write(&path, b"definitely not an encrypted cache").unwrap();

        let err = read_decompressed_plaintext(&path).err().unwrap();

        // Host-dependent early-out: without a cache key (locked Keychain /
        // headless CI) the pipeline fails before decryption ever sees the
        // garbage; the decrypt-failure classification itself is pinned by
        // the `is_corruption` tests in `compact_cache::tests`.
        if !matches!(err, LoadCacheError::KeyUnavailable(_)) {
            assert!(
                err.is_corruption(),
                "garbage bytes must classify as corruption, got: {err}"
            );
        }
        assert!(path.exists(), "the pipeline itself must not touch the file");
    }
}
