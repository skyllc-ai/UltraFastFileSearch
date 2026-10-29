// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Per-bucket min-heap for tracking top-N sample records.
//!
//! During the scan phase, each bucket can optionally maintain a bounded
//! min-heap that tracks the top-N records by a caller-chosen sort key.
//! Only record index + drive ordinal + sort key are stored — no paths or
//! names — so the per-record cost is just 16 bytes.
//!
//! After the scan,
//! [`SampleHeap::drain_sorted`](crate::aggregate::sample_heap::SampleHeap::drain_sorted)
//! returns the entries in final display order (descending or ascending)
//! so they can be materialized into [`SampleRow`](super::finalize::SampleRow)s
//! by the finalize step.

use alloc::collections::BinaryHeap;

use super::spec::TopHitsSpec;
use crate::compact::CompactRecord;
use crate::search::field::FieldId;

/// A single entry stored in the per-bucket sample heap.
///
/// Intentionally small (16 bytes) so that thousands of buckets × 5 entries
/// each remain negligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SampleEntry {
    /// Sort key extracted from the record at scan time.
    pub sort_key: i64,
    /// Index of the record within the drive's `records` array.
    pub rec_idx: u32,
    /// Drive ordinal (index into the `drives` Vec).
    pub drive_ordinal: u8,
}

impl PartialOrd for SampleEntry {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SampleEntry {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.sort_key
            .cmp(&other.sort_key)
            .then_with(|| self.rec_idx.cmp(&other.rec_idx))
    }
}

/// Heap key for **desc mode** (keep top-N largest `sort_key`, prefer
/// **smallest** `rec_idx` on ties).
///
/// Ordered so that `BinaryHeap::peek` returns the **worst** entry — the
/// one we evict first.  Worst = smallest `sort_key`, and on ties the
/// **largest** `rec_idx`.
///
/// This tiebreaker discipline is what makes the desc heap
/// order-independent, which is required now that `scan_drive` fans
/// records out across rayon chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DescKey(SampleEntry);

impl Ord for DescKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // "Greater" = worse for desc.
        //   * smaller `sort_key` is worse  → reverse the comparison.
        //   * on tie, larger `rec_idx` is worse → forward comparison.
        other
            .0
            .sort_key
            .cmp(&self.0.sort_key)
            .then_with(|| self.0.rec_idx.cmp(&other.0.rec_idx))
    }
}

impl PartialOrd for DescKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Heap key for **asc mode** (keep top-N smallest `sort_key`, prefer
/// **smallest** `rec_idx` on ties).
///
/// Ordered so that `BinaryHeap::peek` returns the **worst** entry — the
/// one we evict first.  Worst = largest `sort_key`, and on ties the
/// **largest** `rec_idx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AscKey(SampleEntry);

impl Ord for AscKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // "Greater" = worse for asc.
        //   * larger `sort_key` is worse → forward comparison.
        //   * on tie, larger `rec_idx` is worse → forward comparison.
        self.0
            .sort_key
            .cmp(&other.0.sort_key)
            .then_with(|| self.0.rec_idx.cmp(&other.0.rec_idx))
    }
}

impl PartialOrd for AscKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Bounded heap that keeps the top-N records per bucket.
///
/// Each mode uses its own key wrapper (`DescKey` / `AscKey`) chosen so
/// that `BinaryHeap::peek` always returns the **worst** retained entry.
/// That lets `push_entry` evict the correct entry in O(log N) without
/// walking the heap, and — critically — produces the same set of
/// winners regardless of the order records arrive, which is required
/// once `scan_drive` parallelises via `par_iter().fold().reduce()`.
#[derive(Debug, Clone)]
pub struct SampleHeap {
    /// Maximum entries to keep.
    capacity: u8,
    /// Sort field used to extract the key from a record.
    sort_field: FieldId,
    /// `true` = keep largest (desc), `false` = keep smallest (asc).
    sort_desc: bool,
    /// Desc-mode heap — `peek` returns the worst (most-evictable) entry.
    heap_desc: BinaryHeap<DescKey>,
    /// Asc-mode heap — `peek` returns the worst (most-evictable) entry.
    heap_asc: BinaryHeap<AscKey>,
}

impl SampleHeap {
    /// Create a new sample heap from a [`TopHitsSpec`].
    #[must_use]
    pub(crate) fn from_spec(spec: &TopHitsSpec) -> Self {
        let cap = spec.count.clamp(1, 5);
        Self {
            capacity: cap,
            sort_field: spec.sort_field,
            sort_desc: spec.sort_desc,
            heap_desc: BinaryHeap::with_capacity(usize::from(cap) + 1),
            heap_asc: BinaryHeap::with_capacity(usize::from(cap) + 1),
        }
    }

    /// Extract the sort key from a record for the configured field.
    #[inline]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "FieldId is open-ended; non-orderable fields collapse to a constant sort key"
    )]
    fn extract_sort_key(field: FieldId, record: &CompactRecord) -> i64 {
        match field {
            FieldId::Size => record.size.cast_signed(),
            FieldId::SizeOnDisk => record.allocated.cast_signed(),
            FieldId::Modified => record.modified,
            FieldId::Created => record.created,
            FieldId::Accessed => record.accessed,
            FieldId::NameLength => i64::from(record.name_len),
            FieldId::PathLength => i64::from(record.path_len),
            FieldId::TreeSize => record.treesize.cast_signed(),
            FieldId::TreeAllocated => record.tree_allocated.cast_signed(),
            FieldId::Descendants => i64::from(record.descendants),
            // Boolean flags: 0 or 1.
            FieldId::DirectoryFlag => i64::from(record.flags & 0x0010 != 0),
            FieldId::Hidden => i64::from(record.flags & 0x0002 != 0),
            FieldId::System => i64::from(record.flags & 0x0004 != 0),
            FieldId::ReadOnly => i64::from(record.flags & 0x0001 != 0),
            FieldId::Compressed => i64::from(record.flags & 0x0800 != 0),
            FieldId::Encrypted => i64::from(record.flags & 0x4000 != 0),
            FieldId::Archive => i64::from(record.flags & 0x0020 != 0),
            _ => 0,
        }
    }

    /// Push a record into the heap, evicting if at capacity.
    #[inline]
    pub(crate) fn push(&mut self, record: &CompactRecord, rec_idx: u32, drive_ordinal: u8) {
        let sort_key = Self::extract_sort_key(self.sort_field, record);
        let entry = SampleEntry {
            sort_key,
            rec_idx,
            drive_ordinal,
        };
        self.push_entry(entry);
    }

    /// Push a pre-built [`SampleEntry`] into the heap, evicting if at
    /// capacity.  Shared code path for [`Self::push`] and [`Self::merge`].
    ///
    /// **Tiebreaker contract.**  When the incoming entry's sort key equals
    /// the worst heap entry's sort key, the entry with the **smaller
    /// `rec_idx` wins**.  This keeps the sample deterministic across
    /// insertion orders (required for intra-drive parallel scans using
    /// `par_iter().fold(…).reduce(…)`) while also preserving the
    /// "earliest records retained" behaviour of the pre-parallel
    /// sequential scan.  The invariant is enforced via the mode-specific
    /// [`DescKey`] / [`AscKey`] wrappers above.
    #[inline]
    fn push_entry(&mut self, entry: SampleEntry) {
        let cap = usize::from(self.capacity);

        if self.sort_desc {
            if self.heap_desc.len() < cap {
                self.heap_desc.push(DescKey(entry));
            } else if let Some(&DescKey(worst)) = self.heap_desc.peek()
                && (entry.sort_key > worst.sort_key
                    || (entry.sort_key == worst.sort_key && entry.rec_idx < worst.rec_idx))
            {
                self.heap_desc.pop();
                self.heap_desc.push(DescKey(entry));
            }
        } else if self.heap_asc.len() < cap {
            self.heap_asc.push(AscKey(entry));
        } else if let Some(&AscKey(worst)) = self.heap_asc.peek()
            && (entry.sort_key < worst.sort_key
                || (entry.sort_key == worst.sort_key && entry.rec_idx < worst.rec_idx))
        {
            self.heap_asc.pop();
            self.heap_asc.push(AscKey(entry));
        }
    }

    /// Merge another sample heap into this one, preserving top-N by the
    /// shared sort key / direction.
    ///
    /// Used by the parallel aggregation reducer when per-drive scans
    /// each produce their own sample heap for a given bucket.  Both
    /// heaps must have been built from the same [`TopHitsSpec`] (same
    /// capacity, sort field, and sort direction); callers arrange this
    /// by constructing heaps from the same spec via [`Self::from_spec`].
    pub(crate) fn merge(&mut self, other: &Self) {
        if self.sort_desc {
            for DescKey(entry) in &other.heap_desc {
                self.push_entry(*entry);
            }
        } else {
            for AscKey(entry) in &other.heap_asc {
                self.push_entry(*entry);
            }
        }
    }

    /// Number of entries currently in the heap.
    #[must_use]
    pub const fn len(&self) -> usize {
        if self.sort_desc {
            self.heap_desc.len()
        } else {
            self.heap_asc.len()
        }
    }

    /// Whether the heap is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drain all entries in **display order**.
    ///
    /// - `sort_desc=true`  → largest sort key first
    /// - `sort_desc=false` → smallest sort key first
    #[expect(
        clippy::min_ident_chars,
        reason = "`a`/`b` are conventional sort-comparator bindings; descriptive names would obscure the comparison structure"
    )]
    pub(crate) fn drain_sorted(&mut self) -> Vec<SampleEntry> {
        let mut entries: Vec<SampleEntry> = if self.sort_desc {
            self.heap_desc.drain().map(|DescKey(entry)| entry).collect()
        } else {
            self.heap_asc.drain().map(|AscKey(entry)| entry).collect()
        };
        // Final presentation order: by sort_key only (primary), and on
        // ties preserve the deterministic `rec_idx` ordering established
        // by the heap keys so the output is stable across parallel
        // scans.
        if self.sort_desc {
            entries.sort_by(|a, b| {
                b.sort_key
                    .cmp(&a.sort_key)
                    .then_with(|| a.rec_idx.cmp(&b.rec_idx))
            });
        } else {
            entries.sort_by(|a, b| {
                a.sort_key
                    .cmp(&b.sort_key)
                    .then_with(|| a.rec_idx.cmp(&b.rec_idx))
            });
        }
        entries
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "tests assert against fixtures with known shape; indexing panic = test failure"
)]
mod tests {
    use super::*;

    fn make_spec(count: u8, sort_field: FieldId, sort_desc: bool) -> TopHitsSpec {
        TopHitsSpec::new(count, sort_field, sort_desc, vec![])
    }

    fn make_record(size: u64, flags: u32) -> CompactRecord {
        CompactRecord {
            size,
            allocated: size,
            created: size.cast_signed(),
            modified: size.cast_signed(),
            accessed: size.cast_signed(),
            flags,
            ..CompactRecord::default()
        }
    }

    #[test]
    fn desc_keeps_largest_n() {
        let spec = make_spec(3, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        for i in 0..10_u64 {
            let rec = make_record(i * 100, 0);
            heap.push(&rec, u32::try_from(i).unwrap_or(u32::MAX), 0);
        }
        assert_eq!(heap.len(), 3);
        let entries = heap.drain_sorted();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].sort_key, 900);
        assert_eq!(entries[1].sort_key, 800);
        assert_eq!(entries[2].sort_key, 700);
    }

    #[test]
    fn asc_keeps_smallest_n() {
        let spec = make_spec(3, FieldId::Size, false);
        let mut heap = SampleHeap::from_spec(&spec);
        for i in 0..10_u64 {
            let rec = make_record(i * 100, 0);
            heap.push(&rec, u32::try_from(i).unwrap_or(u32::MAX), 0);
        }
        assert_eq!(heap.len(), 3);
        let entries = heap.drain_sorted();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].sort_key, 0);
        assert_eq!(entries[1].sort_key, 100);
        assert_eq!(entries[2].sort_key, 200);
    }

    #[test]
    fn empty_heap() {
        let spec = make_spec(3, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        assert!(heap.is_empty());
        let entries = heap.drain_sorted();
        assert_eq!(entries, Vec::<SampleEntry>::new());
    }

    #[test]
    fn fewer_than_capacity() {
        let spec = make_spec(5, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        let rec = make_record(42, 0);
        heap.push(&rec, 0, 0);
        let rec2 = make_record(99, 0);
        heap.push(&rec2, 1, 0);
        assert_eq!(heap.len(), 2);
        let entries = heap.drain_sorted();
        assert_eq!(entries[0].sort_key, 99);
        assert_eq!(entries[1].sort_key, 42);
    }

    #[test]
    fn boolean_sort_field() {
        let spec = make_spec(2, FieldId::DirectoryFlag, true);
        let mut heap = SampleHeap::from_spec(&spec);
        for i in 0..5 {
            let rec = make_record(100, 0x20); // files
            heap.push(&rec, i, 0);
        }
        for i in 5..10 {
            let rec = make_record(100, 0x10); // dirs
            heap.push(&rec, i, 0);
        }
        let entries = heap.drain_sorted();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sort_key, 1);
        assert_eq!(entries[1].sort_key, 1);
    }

    #[test]
    fn multi_drive() {
        let spec = make_spec(3, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        heap.push(&make_record(100, 0), 0, 0);
        heap.push(&make_record(500, 0), 0, 1);
        heap.push(&make_record(300, 0), 0, 2);
        let entries = heap.drain_sorted();
        assert_eq!(entries[0].sort_key, 500);
        assert_eq!(entries[0].drive_ordinal, 1);
        assert_eq!(entries[1].sort_key, 300);
        assert_eq!(entries[1].drive_ordinal, 2);
    }

    /// Regression guard for the v0.5.41 intra-drive-parallelism work.
    ///
    /// Under `par_iter().fold().reduce()` the sample-heap can see records
    /// in *any* order within a chunk, and chunks then merge in any order.
    /// That exposes any silent order-dependence in `push_entry`.  This
    /// test feeds five size-100 records whose `rec_idx` values are
    /// shuffled across three different permutations and asserts the
    /// final top-3 sample rows are **identical** under every order.
    ///
    /// With the old "compare by `sort_key` only" logic, the first three
    /// records encountered won, so the result depended on iteration
    /// order.  With the `rec_idx` tiebreaker baked into `push_entry`,
    /// the heap now keeps the three records with the **smallest**
    /// `rec_idx` regardless of the order they arrive.
    #[test]
    fn push_tiebreaker_is_order_independent_on_ties() {
        let spec = make_spec(3, FieldId::Size, true);
        let rec = make_record(100, 0); // identical sort_key on every push
        let orders: [[u32; 5]; 3] = [
            [0, 1, 2, 3, 4], // ascending
            [4, 3, 2, 1, 0], // descending
            [2, 4, 0, 3, 1], // shuffled
        ];
        let mut results: Vec<Vec<(i64, u32)>> = Vec::new();
        for order in orders {
            let mut heap = SampleHeap::from_spec(&spec);
            for &rec_idx in &order {
                heap.push(&rec, rec_idx, 0);
            }
            let mut pairs: Vec<(i64, u32)> = heap
                .drain_sorted()
                .into_iter()
                .map(|entry| (entry.sort_key, entry.rec_idx))
                .collect();
            pairs.sort_by_key(|&(_, rec_idx)| rec_idx);
            results.push(pairs);
        }
        // All three orders must yield the same sorted set of winners.
        assert_eq!(
            results[0], results[1],
            "asc vs desc insertion order must produce the same top-N"
        );
        assert_eq!(
            results[0], results[2],
            "asc vs shuffled insertion order must produce the same top-N"
        );
        // And specifically: smallest three rec_idx win on an all-tie set.
        let winners: Vec<u32> = results[0].iter().map(|&(_, rec_idx)| rec_idx).collect();
        assert_eq!(
            winners,
            vec![0_u32, 1, 2],
            "on sort_key ties, push_entry must retain the smallest rec_idx values"
        );
    }

    /// Ascending-mode counterpart — same invariant, different heap type.
    #[test]
    fn push_tiebreaker_is_order_independent_on_ties_asc() {
        let spec = make_spec(3, FieldId::Size, false);
        let rec = make_record(100, 0);
        let orders: [[u32; 5]; 3] = [[0, 1, 2, 3, 4], [4, 3, 2, 1, 0], [2, 4, 0, 3, 1]];
        let mut results: Vec<Vec<u32>> = Vec::new();
        for order in orders {
            let mut heap = SampleHeap::from_spec(&spec);
            for &rec_idx in &order {
                heap.push(&rec, rec_idx, 0);
            }
            let mut winners: Vec<u32> = heap
                .drain_sorted()
                .into_iter()
                .map(|entry| entry.rec_idx)
                .collect();
            winners.sort_unstable();
            results.push(winners);
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[0], results[2]);
        assert_eq!(results[0], vec![0_u32, 1, 2]);
    }
}
