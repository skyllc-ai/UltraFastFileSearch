// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase-2b end-to-end oracle for the trigram base+delta overlay
//! (incremental-index-maintenance §4 Phase 2 / §7).
//!
//! Drives a **real** [`super::apply_usn_patch`] batch (create + rename +
//! delete) so the delta is populated exactly as the live USN path does, then
//! asserts that `trigram_search` through the base ∪ delta overlay returns
//! **identical** candidates to a fully **compacted** index (delta folded into a
//! fresh base). That equivalence is the Phase-2b correctness contract: "base +
//! delta must be byte-identical to a full rebuild" — for search results, across
//! every op.

use alloc::sync::Arc;
use std::path::PathBuf;

use uffs_mft::usn::FileChange;
use uffs_text::case_fold::CaseFold;

use super::{IndexSource, apply_usn_patch};
use crate::compact::{ChildrenIndex, CompactRecord, DriveCompactIndex, ExtensionIndex};
use crate::compact_storage::ColumnStorage;
use crate::trigram::TrigramIndex;

/// Push one record (root or file) and register its FRS→compact mapping.
fn push_record(
    names: &mut Vec<u8>,
    records: &mut Vec<CompactRecord>,
    frs_to_compact: &mut Vec<u32>,
    name: &str,
    frs: usize,
    parent: u32,
    dir: bool,
) {
    let idx = u32::try_from(records.len()).expect("fixture fits u32");
    let offset = u32::try_from(names.len()).expect("fixture names fit u32");
    names.extend_from_slice(name.as_bytes());
    records.push(CompactRecord {
        name_offset: offset,
        flags: if dir { 0x10 } else { 0 },
        parent_idx: parent,
        name_len: u16::try_from(name.len()).expect("fixture name fits u16"),
        name_first_byte: name.as_bytes().first().copied().unwrap_or(0),
        ..CompactRecord::default()
    });
    if frs >= frs_to_compact.len() {
        frs_to_compact.resize(frs + 1, u32::MAX);
    }
    if let Some(slot) = frs_to_compact.get_mut(frs) {
        *slot = idx;
    }
}

/// Root "C" (frs 5) + four files; FRS mapping populated so `apply_usn_patch`
/// can resolve every change.
fn build_drive() -> DriveCompactIndex {
    let mut names = Vec::new();
    let mut records = Vec::new();
    let mut frs_to_compact = Vec::new();
    push_record(
        &mut names,
        &mut records,
        &mut frs_to_compact,
        "C",
        5,
        u32::MAX,
        true,
    );
    push_record(
        &mut names,
        &mut records,
        &mut frs_to_compact,
        "report.txt",
        10,
        0,
        false,
    );
    push_record(
        &mut names,
        &mut records,
        &mut frs_to_compact,
        "alpha.txt",
        11,
        0,
        false,
    );
    push_record(
        &mut names,
        &mut records,
        &mut frs_to_compact,
        "config.json",
        12,
        0,
        false,
    );
    push_record(
        &mut names,
        &mut records,
        &mut frs_to_compact,
        "datafile.bin",
        13,
        0,
        false,
    );

    let fold = CaseFold::default_table();
    let trigram = TrigramIndex::build(&records, &names, fold);
    let children = ChildrenIndex::build(&records);
    let ext_index = ExtensionIndex::build(&records);
    DriveCompactIndex {
        letter: uffs_mft::platform::DriveLetter::T,
        records: ColumnStorage::from_vec(records),
        names: ColumnStorage::from_vec(names),
        trigram: Arc::new(trigram),
        children: Arc::new(children),
        ext_index: Arc::new(ext_index),
        fold,
        ext_names: vec![Box::from("")],
        source: IndexSource::MftFile(PathBuf::from("T:")),
        source_epoch: 1,
        bloom: None,
        path_trie: None,
        frs_to_compact,
        delta: None,
    }
}

fn sorted_candidates(drive: &DriveCompactIndex, needle: &str) -> Vec<u32> {
    let mut got = drive.trigram_search(needle).unwrap_or_default();
    got.sort_unstable();
    got
}

#[test]
fn apply_batch_delta_search_equals_compacted_rebuild_oracle() {
    let mut drive = build_drive();

    // A batch hitting every op: create a file, rename one, delete one.
    apply_usn_patch(&mut drive, &[
        FileChange {
            frs: 20_u64.into(),
            parent_frs: 5_u64.into(),
            filename: "newfile.log".to_owned(),
            created: true,
            ..FileChange::default()
        },
        FileChange {
            frs: 10_u64.into(),
            parent_frs: 5_u64.into(),
            filename: "summary.txt".to_owned(), // report.txt -> summary.txt
            renamed: true,
            ..FileChange::default()
        },
        FileChange {
            frs: 11_u64.into(),
            parent_frs: 5_u64.into(),
            deleted: true, // alpha.txt deleted
            ..FileChange::default()
        },
    ]);

    // The live drive now serves search through the base ∪ delta overlay.
    assert!(
        drive.delta.is_some(),
        "apply must have populated the trigram delta"
    );

    // Oracle reference: the same drive with the delta folded into a fresh base.
    let mut compacted = drive.clone();
    compacted.compact_base();
    assert!(compacted.delta.is_none(), "compaction must clear the delta");

    // Every needle must yield identical candidates from the overlay and the
    // compacted rebuild — covering created, renamed (new + old name), deleted,
    // and untouched files.
    for needle in [
        "summ", "summary", // renamed-in (new name)
        "report", "repo", // renamed-away (old name) — gone from both
        "newfile", "newf", // created
        "alpha", "lpha", // deleted — gone from both
        "config", "datafile", "bin", "txt", "log", // untouched / extensions
    ] {
        let overlay = sorted_candidates(&drive, needle);
        let rebuilt = sorted_candidates(&compacted, needle);
        assert_eq!(
            overlay, rebuilt,
            "needle {needle:?}: overlay {overlay:?} != compacted rebuild {rebuilt:?}",
        );
    }

    // Spot-check the semantics concretely (compact_idx: report/summary=1,
    // config=3, datafile=4, newfile appended at 5).
    assert_eq!(
        sorted_candidates(&drive, "summary"),
        vec![1],
        "renamed visible as summary"
    );
    assert!(
        sorted_candidates(&drive, "report").is_empty(),
        "old name gone"
    );
    assert_eq!(
        sorted_candidates(&drive, "newfile"),
        vec![5],
        "created visible"
    );
    assert!(
        sorted_candidates(&drive, "alpha").is_empty(),
        "deleted gone"
    );

    // Phase 4a ext oracle: records_with_ext through the overlay must equal the
    // compacted rebuild for every extension id (the create interns ".log", the
    // rename keeps ".txt", the delete drops ".txt"). Covers id 0 (no extension)
    // up past the highest interned id.
    let max_ext = drive
        .records
        .iter()
        .map(|rec| rec.extension_id)
        .max()
        .unwrap_or(0);
    for ext_id in 0..=max_ext {
        let mut overlay = drive.records_with_ext(ext_id).into_owned();
        overlay.sort_unstable();
        let mut rebuilt = compacted.records_with_ext(ext_id).into_owned();
        rebuilt.sort_unstable();
        assert_eq!(
            overlay, rebuilt,
            "ext_id {ext_id}: overlay {overlay:?} != compacted rebuild {rebuilt:?}",
        );
    }
}

/// Phase-4b children oracle, exercising the hardest case — a file **moving
/// between directories** (a rename that changes `parent_idx`). The base
/// children CSR is frozen (no per-apply rebuild), so `children_of` must merge
/// base ∪ delta and validate each candidate against the live records: the
/// moved file must leave its old parent's child list and join the new one.
/// `children_of` through the overlay must equal the compacted rebuild for every
/// parent, across move + create + delete.
/// Nested fixture for the children oracle: root C(frs5,idx0) → dirs
/// alpha(6,1) beta(7,2); moved.txt(8,3) under alpha, stay.txt(9,4) under beta.
fn build_nested_dir_fixture() -> DriveCompactIndex {
    let mut names = Vec::new();
    let mut records = Vec::new();
    let mut frs = Vec::new();
    push_record(&mut names, &mut records, &mut frs, "C", 5, u32::MAX, true);
    push_record(&mut names, &mut records, &mut frs, "alpha", 6, 0, true);
    push_record(&mut names, &mut records, &mut frs, "beta", 7, 0, true);
    push_record(&mut names, &mut records, &mut frs, "moved.txt", 8, 1, false);
    push_record(&mut names, &mut records, &mut frs, "stay.txt", 9, 2, false);

    let fold = CaseFold::default_table();
    let trigram = TrigramIndex::build(&records, &names, fold);
    let children = ChildrenIndex::build(&records);
    let ext_index = ExtensionIndex::build(&records);
    DriveCompactIndex {
        letter: uffs_mft::platform::DriveLetter::T,
        records: ColumnStorage::from_vec(records),
        names: ColumnStorage::from_vec(names),
        trigram: Arc::new(trigram),
        children: Arc::new(children),
        ext_index: Arc::new(ext_index),
        fold,
        ext_names: vec![Box::from("")],
        source: IndexSource::MftFile(PathBuf::from("T:")),
        source_epoch: 1,
        bloom: None,
        path_trie: None,
        frs_to_compact: frs,
        delta: None,
    }
}

#[test]
fn children_overlay_equals_compacted_rebuild_oracle() {
    let mut drive = build_nested_dir_fixture();

    apply_usn_patch(&mut drive, &[
        // Move moved.txt (frs8) from alpha(6) to beta(7) — same name, new
        // parent.
        FileChange {
            frs: 8_u64.into(),
            parent_frs: 7_u64.into(),
            filename: "moved.txt".to_owned(),
            renamed: true,
            ..FileChange::default()
        },
        // Create fresh.txt (frs10) under alpha(6).
        FileChange {
            frs: 10_u64.into(),
            parent_frs: 6_u64.into(),
            filename: "fresh.txt".to_owned(),
            created: true,
            ..FileChange::default()
        },
        // Delete stay.txt (frs9).
        FileChange {
            frs: 9_u64.into(),
            parent_frs: 7_u64.into(),
            deleted: true,
            ..FileChange::default()
        },
    ]);

    assert!(
        drive.delta.is_some(),
        "apply must have populated the children delta"
    );
    let mut compacted = drive.clone();
    compacted.compact_base();

    // children_of must match the compacted rebuild for every record index.
    let record_count = u32::try_from(drive.records.len()).expect("fits u32");
    for parent in 0..record_count {
        let overlay = drive.children_of(parent).into_owned();
        let rebuilt = compacted.children_of(parent).into_owned();
        assert_eq!(
            overlay, rebuilt,
            "children_of({parent}): overlay {overlay:?} != compacted {rebuilt:?}",
        );
    }

    // Concrete semantics: moved.txt(3) left alpha(1) for beta(2); fresh.txt(5)
    // joined alpha; stay.txt(4) deleted from beta.
    assert_eq!(
        drive.children_of(1).into_owned(),
        vec![5],
        "alpha = {{fresh.txt}}"
    );
    assert_eq!(
        drive.children_of(2).into_owned(),
        vec![3],
        "beta = {{moved.txt}}"
    );
}
