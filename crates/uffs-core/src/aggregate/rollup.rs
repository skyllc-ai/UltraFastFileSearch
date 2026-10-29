// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Rollup aggregations — hierarchical grouping by path or drive.
//!
//! A rollup groups records by a parent/ancestor directory at a given
//! depth from the drive root. This allows "top folders" analysis
//! without resolving full paths for every record.

use std::collections::HashMap;

use super::accumulators::StatsAccumulator;
use super::spec::RollupMode;
use crate::compact::{CompactRecord, DriveCompactIndex};

/// A rollup accumulator — groups records by a key derived from
/// path ancestry or drive letter.
///
/// Group keys are drive-qualified: the drive ordinal lives in the high
/// 32 bits and the per-drive record index (or drive-letter byte) in the
/// low 32 bits. Bare record indices would collide across drives when the
/// per-drive accumulators are merged, silently folding one drive's
/// folders into another's (and resolving their names against the wrong
/// drive's name table).
#[derive(Debug, Clone)]
pub struct RollupAccumulator {
    /// Per-group statistics keyed by drive-qualified group key
    /// (see `encode_rollup_key`).
    pub groups: HashMap<u64, StatsAccumulator>,
    /// Rollup mode.
    pub mode: RollupMode,
    /// Max groups to track.
    pub top: u16,
    /// Last computed group key (used by nested rollups to route
    /// sub-accumulator feeding without recomputing the key).
    pub last_key: u64,
}

impl RollupAccumulator {
    /// Create a new rollup accumulator.
    #[must_use]
    pub fn new(mode: RollupMode, top: u16) -> Self {
        Self {
            groups: HashMap::new(),
            mode,
            top,
            last_key: 0,
        }
    }

    /// Feed a record into the rollup.
    ///
    /// `drive_ordinal` is the position of `drive` in the aggregation's
    /// drive slice; it is encoded into the group key so keys from
    /// different drives never collide and resolve against the right
    /// drive's name table.
    ///
    /// Returns `true` when the record was bucketed. `Ancestor` rollups
    /// are scoped to the drive named in the spec — records on other
    /// drives are skipped (`false`), and callers must not feed nested
    /// sub-accumulators for skipped records.
    #[inline]
    pub fn feed(
        &mut self,
        record: &CompactRecord,
        drive: &DriveCompactIndex,
        idx: usize,
        drive_ordinal: u8,
    ) -> bool {
        let low = match self.mode {
            RollupMode::Drive => {
                u32::from(u8::try_from(u32::from(drive.letter.as_byte())).unwrap_or(b'?'))
            }
            RollupMode::Path { depth } => ancestor_at_depth(record, drive, idx, depth),
            RollupMode::Ancestor {
                record_idx,
                drive: target,
            } => {
                if drive.letter != target {
                    return false;
                }
                child_of_ancestor(drive, idx, record_idx)
            }
        };
        let key = encode_rollup_key(drive_ordinal, low);

        self.last_key = key;
        let stats = self.groups.entry(key).or_default();
        stats.feed_value(record.size, record.allocated);
        true
    }

    /// The group key computed by the most recent `feed()` call.
    ///
    /// Used by nested rollup logic to route sub-accumulator feeding
    /// without recomputing the key.
    #[inline]
    #[must_use]
    pub const fn last_key(&self) -> u64 {
        self.last_key
    }

    /// Merge another rollup accumulator.
    #[expect(
        clippy::iter_over_hash_type,
        reason = "per-key merge is order-independent: each group merges by key into self"
    )]
    pub fn merge(&mut self, other: &Self) {
        for (&key, other_stats) in &other.groups {
            self.groups
                .entry(key)
                .and_modify(|stats| stats.merge(other_stats))
                .or_insert_with(|| other_stats.clone());
        }
    }

    /// Finalize: sort by total bytes descending, truncate to top-N.
    /// Returns (key, stats) pairs; keys are drive-qualified
    /// (see `encode_rollup_key`).
    #[must_use]
    pub fn finalize(&self) -> Vec<(u64, &StatsAccumulator)> {
        let mut entries: Vec<_> = self.groups.iter().map(|(&key, val)| (key, val)).collect();
        entries.sort_by_key(|entry| core::cmp::Reverse(entry.1.sum));
        entries.truncate(usize::from(self.top));
        entries
    }
}

/// Walk the parent chain to find the ancestor at a given depth from root.
///
/// `depth=1` means the immediate child of the drive root.
/// Returns the record index of that ancestor, or `idx` itself if the
/// record is shallower than the requested depth.
fn ancestor_at_depth(
    _record: &CompactRecord,
    drive: &DriveCompactIndex,
    idx: usize,
    target_depth: u32,
) -> u32 {
    // Build the parent chain by walking up.
    let records = &drive.records;
    let mut chain: Vec<u32> = Vec::with_capacity(16);
    let mut current = uffs_mft::len_to_u32(idx);

    // Walk up to root (parent_idx == 0 or self-referencing means root).
    loop {
        chain.push(current);
        let Some(record) = records.get(uffs_mft::u32_as_usize(current)) else {
            break;
        };
        let parent = record.parent_idx;
        if parent == current || parent == 0 {
            break;
        }
        current = parent;
        if chain.len() > 255 {
            break; // Safety: prevent infinite loops
        }
    }

    // chain is [leaf, ..., root]. Reverse to get [root, ..., leaf].
    chain.reverse();

    // depth=1 → index 1 in chain (first child of root).
    let depth_idx = uffs_mft::u32_as_usize(target_depth);
    chain
        .get(depth_idx)
        .copied()
        .unwrap_or_else(|| uffs_mft::len_to_u32(idx))
}

/// Walk the parent chain to find which direct child of `ancestor_idx`
/// the record at `idx` descends from.
///
/// If the record IS the ancestor, returns the ancestor itself.
/// If the record is not a descendant of the ancestor, returns `idx`
/// unchanged (so it falls into its own bucket).
fn child_of_ancestor(drive: &DriveCompactIndex, idx: usize, ancestor_idx: u32) -> u32 {
    let records = &drive.records;
    let mut current = uffs_mft::len_to_u32(idx);
    let mut child = current; // tracks the child one step below

    for _ in 0..256_u16 {
        if current == ancestor_idx {
            return child;
        }
        let Some(record) = records.get(uffs_mft::u32_as_usize(current)) else {
            break;
        };
        let parent = record.parent_idx;
        if parent == current {
            break; // self-referencing root — ancestor not found
        }
        child = current;
        current = parent;
    }

    // Not a descendant of ancestor — return own index.
    uffs_mft::len_to_u32(idx)
}

/// Encode a drive-qualified rollup group key: drive ordinal in the high
/// 32 bits, per-drive value (record index or drive-letter byte) in the
/// low 32 bits.
#[inline]
#[must_use]
pub(crate) fn encode_rollup_key(drive_ordinal: u8, low: u32) -> u64 {
    (u64::from(drive_ordinal) << 32_u32) | u64::from(low)
}

/// Split a drive-qualified rollup key back into (drive ordinal, low bits).
#[inline]
#[must_use]
fn decode_rollup_key(key: u64) -> (usize, u32) {
    // Both conversions are infallible after the shift/mask; the fallbacks
    // exist only to satisfy the no-panic lint policy.
    let ordinal = usize::try_from(key >> 32_u32).unwrap_or(usize::MAX);
    let low = u32::try_from(key & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    (ordinal, low)
}

/// Resolve a drive-qualified rollup key to a display name.
///
/// For drive rollups, the low bits are the drive-letter byte.
/// For path/ancestor rollups, the low bits are a record index into the
/// drive selected by the key's ordinal — never any other drive.
#[must_use]
pub(crate) fn resolve_rollup_key(
    key: u64,
    mode: RollupMode,
    drives: &[&DriveCompactIndex],
) -> String {
    let (ordinal, low) = decode_rollup_key(key);
    match mode {
        RollupMode::Drive => {
            let ch = char::from(u8::try_from(low).unwrap_or(b'?'));
            format!("{ch}:")
        }
        RollupMode::Path { .. } | RollupMode::Ancestor { .. } => {
            let idx = uffs_mft::u32_as_usize(low);
            drives
                .get(ordinal)
                .and_then(|drive| {
                    drive.records.get(idx).map(|record| {
                        let name = record.name(&drive.names);
                        format!("{}:\\{name}", drive.letter)
                    })
                })
                .unwrap_or_else(|| format!("record_{low}"))
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "tests assert against fixtures with known shape; indexing panic = test failure"
)]
mod tests {
    use alloc::sync::Arc;

    use super::*;

    #[test]
    fn rollup_accumulator_drive_mode() {
        let acc = RollupAccumulator::new(RollupMode::Drive, 26);
        assert!(acc.groups.is_empty());
        assert_eq!(acc.top, 26);
    }

    #[test]
    fn rollup_accumulator_path_mode() {
        let acc = RollupAccumulator::new(RollupMode::Path { depth: 1 }, 30);
        assert_eq!(acc.mode, RollupMode::Path { depth: 1 });
    }

    #[test]
    fn rollup_accumulator_ancestor_mode() {
        let ancestor = RollupMode::Ancestor {
            record_idx: 42,
            drive: uffs_mft::platform::DriveLetter::C,
        };
        let acc = RollupAccumulator::new(ancestor, 20);
        assert_eq!(acc.mode, ancestor);
        assert_eq!(acc.top, 20);
    }

    /// Build a minimal drive index for ancestor tests.
    ///
    /// Tree structure (record indices):
    /// ```text
    ///   0 (root)
    ///   ├── 1 (folder_a)
    ///   │   ├── 3 (file_x)
    ///   │   └── 4 (sub_folder)
    ///   │       └── 5 (file_y)
    ///   └── 2 (folder_b)
    ///       └── 6 (file_z)
    /// ```
    fn build_ancestor_test_drive() -> DriveCompactIndex {
        use std::path::PathBuf;

        use crate::compact::{ChildrenIndex, CompactRecord, ExtensionIndex, IndexSource};
        use crate::trigram::TrigramIndex;
        // Build names blob: concatenated UTF-8 strings.
        let name_strs = [
            "root",
            "folder_a",
            "folder_b",
            "file_x",
            "sub_folder",
            "file_y",
            "file_z",
        ];
        let mut names_blob = Vec::new();
        let mut offsets = Vec::new();
        for name in &name_strs {
            offsets.push(uffs_mft::len_to_u32(names_blob.len()));
            names_blob.extend_from_slice(name.as_bytes());
        }

        let dir = 0x0010_u32; // FILE_ATTRIBUTE_DIRECTORY
        let records = vec![
            CompactRecord {
                name_offset: offsets[0],
                flags: dir,
                parent_idx: 0,
                name_len: uffs_mft::len_to_u16(name_strs[0].len()),
                ..Default::default()
            },
            CompactRecord {
                name_offset: offsets[1],
                flags: dir,
                parent_idx: 0,
                name_len: uffs_mft::len_to_u16(name_strs[1].len()),
                ..Default::default()
            },
            CompactRecord {
                name_offset: offsets[2],
                flags: dir,
                parent_idx: 0,
                name_len: uffs_mft::len_to_u16(name_strs[2].len()),
                ..Default::default()
            },
            CompactRecord {
                size: 1000,
                allocated: 4096,
                name_offset: offsets[3],
                flags: 0,
                parent_idx: 1,
                name_len: uffs_mft::len_to_u16(name_strs[3].len()),
                ..Default::default()
            },
            CompactRecord {
                name_offset: offsets[4],
                flags: dir,
                parent_idx: 1,
                name_len: uffs_mft::len_to_u16(name_strs[4].len()),
                ..Default::default()
            },
            CompactRecord {
                size: 2000,
                allocated: 4096,
                name_offset: offsets[5],
                flags: 0,
                parent_idx: 4,
                name_len: uffs_mft::len_to_u16(name_strs[5].len()),
                ..Default::default()
            },
            CompactRecord {
                size: 3000,
                allocated: 4096,
                name_offset: offsets[6],
                flags: 0,
                parent_idx: 2,
                name_len: uffs_mft::len_to_u16(name_strs[6].len()),
                ..Default::default()
            },
        ];

        let children = ChildrenIndex::build(&records);

        DriveCompactIndex {
            letter: uffs_mft::platform::DriveLetter::C,
            records: crate::compact_storage::ColumnStorage::from_vec(records),
            names: crate::compact_storage::ColumnStorage::from_vec(names_blob),
            trigram: Arc::new(TrigramIndex::empty()),
            children: Arc::new(children),
            ext_index: Arc::new(ExtensionIndex::build(&[])),
            fold: uffs_text::case_fold::CaseFold::default_table(),
            ext_names: vec![],
            source: IndexSource::MftFile(PathBuf::from("C:")),
            source_epoch: 0,
            bloom: None,
            path_trie: None,
            // unused by aggregation tests — see compact.rs::frs_to_compact
            // docs.
            frs_to_compact: Vec::new(),
            delta: None,
        }
    }

    #[test]
    fn child_of_ancestor_direct_child() {
        let drive = build_ancestor_test_drive();
        // file_x (idx=3) parent is folder_a (idx=1).
        // ancestor=0 (root) → should return 1 (folder_a).
        let key = child_of_ancestor(&drive, 3, 0);
        assert_eq!(key, 1, "file_x's child-of-root should be folder_a");
    }

    #[test]
    fn child_of_ancestor_deep_descendant() {
        let drive = build_ancestor_test_drive();
        // file_y (idx=5) → parent=4 → parent=1 → parent=0.
        // ancestor=1 (folder_a) → should return 4 (sub_folder).
        let key = child_of_ancestor(&drive, 5, 1);
        assert_eq!(key, 4, "file_y's child-of-folder_a should be sub_folder");
    }

    #[test]
    fn child_of_ancestor_is_self_when_direct() {
        let drive = build_ancestor_test_drive();
        // file_x (idx=3) parent is folder_a (idx=1).
        // ancestor=1 → should return 3 (file_x itself is the direct child).
        let key = child_of_ancestor(&drive, 3, 1);
        assert_eq!(key, 3, "file_x is a direct child of folder_a");
    }

    #[test]
    fn child_of_ancestor_not_descendant() {
        let drive = build_ancestor_test_drive();
        // file_z (idx=6) parent is folder_b (idx=2).
        // ancestor=1 (folder_a) → not a descendant → returns own idx.
        let key = child_of_ancestor(&drive, 6, 1);
        assert_eq!(key, 6, "file_z is not a descendant of folder_a");
    }

    #[test]
    fn resolve_rollup_key_ancestor_mode() {
        let drive = build_ancestor_test_drive();
        let mode = RollupMode::Ancestor {
            record_idx: 0,
            drive: uffs_mft::platform::DriveLetter::C,
        };
        let name = resolve_rollup_key(encode_rollup_key(0, 1), mode, &[&drive]);
        assert_eq!(name, "C:\\folder_a");
    }

    #[test]
    fn rollup_key_roundtrip_encodes_drive_ordinal() {
        let key = encode_rollup_key(3, 0x00AB_CDEF);
        assert_eq!(decode_rollup_key(key), (3, 0x00AB_CDEF));
        // Same record index on different drives must produce distinct keys.
        assert_ne!(encode_rollup_key(0, 7), encode_rollup_key(1, 7));
    }

    #[test]
    fn resolve_rollup_key_out_of_range_ordinal_falls_back() {
        let drive = build_ancestor_test_drive();
        let mode = RollupMode::Path { depth: 1 };
        // Ordinal 5 with only one drive present — must not resolve against
        // the wrong drive.
        let name = resolve_rollup_key(encode_rollup_key(5, 1), mode, &[&drive]);
        assert_eq!(name, "record_1");
    }
}
