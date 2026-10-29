// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Cache-poisoning regression cluster: shapes a serialized compact
//! cache must survive (empty trigram CSR) or must never assume
//! (delta-carrying base sections), plus the save-boundary refusal.
//! Split from [`super`] (the `tests` module) for the 800-LOC policy;
//! shares its [`super::make_test_index`] fixture.

use super::super::*;
use super::make_test_index;

/// Regression: a cache saved with an EMPTY trigram index must still
/// round-trip every later section intact.
///
/// The writer always emits the full trigram CSR — offsets carries
/// `keys.len() + 1` entries, so an empty index still writes its `[0]`
/// offsets entry plus the values count.  The reader's old `tkc > 0`
/// fast-path skipped only the 4-byte key count in the empty case,
/// leaving 8 bytes unconsumed and shifting the ext-names / bloom /
/// trie / frs sections — surfacing as `bloom k_hashes out of range`
/// and a quarantined cache (2026-08 "drive C for ten days" incident;
/// 2026-08-24 winbox C/D/S quarantine storm).
#[test]
fn empty_trigram_round_trips_without_shifting_later_sections() {
    let mut index = make_test_index();
    index.trigram = Arc::new(TrigramIndex::empty());
    // Give the later sections real content so a shift is detectable.
    index.ext_names = vec![Box::from("rs"), Box::from("toml")];
    index.bloom = Some(index.build_bloom());
    index.path_trie = Some(index.build_path_trie());

    let serialized = serialize_compact(&index);
    let (loaded, _tri_ms) = deserialize_compact(&serialized, uffs_mft::platform::DriveLetter::T)
        .unwrap_or_else(|err| panic!("empty-trigram cache must deserialize, got: {err}"));

    assert_eq!(
        loaded.ext_names,
        vec![Box::from("rs"), Box::from("toml")],
        "ext_names must survive an empty trigram section"
    );
    let bloom = loaded.bloom.as_ref().expect("bloom section must load");
    assert!(
        bloom.contains(b"rs"),
        "the loaded bloom must answer for the drive's real extensions"
    );
    assert_eq!(
        loaded.frs_to_compact, index.frs_to_compact,
        "frs_to_compact must survive an empty trigram section"
    );
    // The empty on-disk trigram is rebuilt from records (an empty
    // index over a non-empty drive would silently break substring
    // search); records are non-empty here, so the rebuild is real.
    assert!(
        loaded.trigram.posting_count() > 0,
        "trigram must be rebuilt from records, not adopted empty"
    );
}

/// The parked-tier loader walks the same section chain with its own
/// arithmetic (`find_v9_filters_offset`) — pin that it, too, lands on
/// the real bloom when the trigram section is empty (its old
/// `key_count > 0` branch believed in a bare-sentinel shape no writer
/// ever produced, shifting the filter reads by 8 bytes).
#[test]
fn parked_loader_finds_filters_past_an_empty_trigram() {
    let mut index = make_test_index();
    index.trigram = Arc::new(TrigramIndex::empty());
    index.ext_names = vec![Box::from("rs"), Box::from("toml")];
    index.bloom = Some(index.build_bloom());
    index.path_trie = Some(index.build_path_trie());

    let serialized = serialize_compact(&index);
    let parked = deserialize_parked_body(&serialized, uffs_mft::platform::DriveLetter::T)
        .unwrap_or_else(|err| panic!("parked load must survive an empty trigram, got: {err}"));
    assert!(
        parked.bloom.contains(b"rs"),
        "the parked bloom must answer for the drive's real extensions"
    );
}

/// The surgical-patch state: one created record appended (name "qux"
/// appended to the blob), bases left Arc-shared/stale, delta overlay armed.
fn index_with_created_record_and_delta() -> DriveCompactIndex {
    let mut index = make_test_index();
    index.bloom = Some(index.build_bloom());
    index.path_trie = Some(index.build_path_trie());

    // Simulate the surgical-patch state: one created record appended
    // (name "qux" appended to the blob), bases left Arc-shared/stale,
    // delta overlay armed.
    let mut records: Vec<CompactRecord> = index.records.as_slice().to_vec();
    let mut names: Vec<u8> = index.names.as_slice().to_vec();
    let name_offset = u32::try_from(names.len()).unwrap_or(u32::MAX);
    names.extend_from_slice(b"qux");
    records.push(CompactRecord {
        name_offset,
        parent_idx: 0,
        name_len: 3,
        name_first_byte: b'q',
        ..CompactRecord::default()
    });
    index.records = ColumnStorage::from_vec(records);
    index.names = ColumnStorage::from_vec(names);
    index.delta = Some(crate::compact::IndexDelta::default());
    index
}

/// Regression: a DELTA-CARRYING index must never serialize — its
/// Arc-shared base sections are stale against the patched records.
///
/// A create appends a record (the header's `rc` grows) while
/// `children` stays the base CSR with `rc_base + 1` offsets; the
/// reader sizes the children section from the header, so every later
/// section is read shifted and the cache quarantines on the next
/// start (field: `bloom k_hashes out of range`, winbox M/C/D/S
/// 2026-08-24 — caches written BY the then-current daemon between two
/// restarts).  [`DriveCompactIndex::fold_delta_for_save`] is the
/// repair the save path applies; pin both halves.
#[test]
fn delta_carrying_index_misaligns_and_fold_repairs_it() {
    let mut index = index_with_created_record_and_delta();

    // The poisoned bytes can NEVER round-trip faithfully: the children
    // CSR on disk was written for the 3 base records, so the created
    // record's parent→child edge does not exist in the file (the delta
    // that held it is not serialised), and the header-driven reader
    // walks the shifted sections — on big real-world drives that
    // surfaces as a parse error and a quarantined cache ("bloom
    // k_hashes out of range"); on this small fixture the shifted bytes
    // happen to parse, which is the SILENT face of the same bug.
    // Either face proves the shape must never reach disk.
    let poisoned = serialize_compact(&index);
    match deserialize_compact(&poisoned, uffs_mft::platform::DriveLetter::T) {
        Err(_parse_error) => {} // the quarantine face
        Ok((corrupt, _tri_ms)) => {
            let mut root_children: Vec<u32> = Vec::new();
            corrupt.for_each_child(0, |child| root_children.push(child));
            assert!(
                !root_children.contains(&3),
                "the created record's children edge was never written — a load that \
                 reports it would falsify the poison analysis"
            );
        }
    }

    // The repair the save path applies: fold the delta, then serialize.
    index.fold_delta_for_save();
    assert!(index.delta.is_none(), "fold must clear the overlay");
    let healthy = serialize_compact(&index);
    let (loaded, _tri_ms) = deserialize_compact(&healthy, uffs_mft::platform::DriveLetter::T)
        .unwrap_or_else(|err| panic!("folded index must round-trip, got: {err}"));
    assert_eq!(loaded.records.len(), 4, "the created record must persist");
    let mut root_children: Vec<u32> = Vec::new();
    loaded.for_each_child(0, |child| root_children.push(child));
    assert!(
        root_children.contains(&3),
        "the folded children CSR must carry the created record's edge"
    );
    assert!(
        loaded.bloom.is_some(),
        "the bloom section must land intact after the fold"
    );
}

/// The save boundary refuses a delta-carrying index outright — the
/// second line of defence behind the save path's fold, so any future
/// writer path that forgets to fold fails loudly instead of writing a
/// poisoned cache.
#[test]
fn background_save_refuses_a_delta_carrying_index() {
    let mut index = make_test_index();
    index.delta = Some(crate::compact::IndexDelta::default());
    let err = save_compact_cache_background(&index)
        .expect_err("a delta-carrying index must be refused at the save boundary");
    assert!(
        err.to_string().contains("delta"),
        "the refusal must name the delta invariant: {err}"
    );
}

/// The journal loop's save writes a delta-carrying body directly, rebuilding
/// the children and trigram sections for the write instead of folding a copy
/// (a copy of a 30 M-record drive's records and names is over 3 GB).  The
/// bytes must be exactly those of the folded index.
#[test]
fn folded_writer_matches_serializing_a_folded_copy() {
    let index = index_with_created_record_and_delta();

    let mut direct = Vec::new();
    serialize_compact_folded_to_writer(&index, &mut direct)
        .unwrap_or_else(|err| panic!("folded write failed: {err}"));

    let mut folded = index.clone();
    folded.fold_delta_for_save();
    let mut reference = Vec::new();
    serialize_compact_to_writer(&folded, &mut reference)
        .unwrap_or_else(|err| panic!("reference write failed: {err}"));

    assert_eq!(
        direct, reference,
        "folded write must equal the folded copy byte for byte"
    );
    assert!(
        index.delta.is_some(),
        "writing must not touch the live index"
    );
}

/// The cache writers append a decompressed-size trailer after the zstd frame
/// so the loader can allocate the plaintext once, at its exact size.  The
/// same compressed bytes must deserialize to the same index.
#[test]
fn sized_compression_round_trips_the_index() {
    use std::io::Write as _;

    let index = make_test_index();
    let mut writer = uffs_mft::cache::CountingWriter::new(
        uffs_mft::cache::new_zstd_mt_encoder(ZSTD_LEVEL).unwrap(),
    );
    serialize_compact_to_writer(&index, &mut writer).unwrap();
    writer.flush().unwrap();
    let decompressed_len = writer.written();
    let mut compressed = writer.into_inner().finish().unwrap();
    uffs_mft::cache::append_size_trailer(&mut compressed, decompressed_len);

    let plaintext = uffs_mft::cache::decompress_sized(&compressed).unwrap();
    assert_eq!(
        plaintext,
        serialize_compact(&index),
        "the trailer must not alter the payload"
    );
    assert_eq!(
        plaintext.capacity(),
        plaintext.len(),
        "allocated once, at the exact size"
    );
    let (loaded, _tri_ms) = deserialize_compact(&plaintext, uffs_mft::platform::DriveLetter::T)
        .unwrap_or_else(|err| panic!("sized plaintext must deserialize: {err}"));
    assert_eq!(loaded.records.len(), index.records.len());
}
