// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Compact in-memory index for search backends.
//!
//! Replaces the full `MftIndex` (224 bytes/record) with a lean 72-byte
//! `CompactRecord` that covers 100% of sortable/filterable columns.
//! Full metadata (ADS, forensic fields) is resolved on-demand from the
//! `.uffs` cache file.
//!
//! See `docs/architecture/COMPACT_INDEX_DESIGN.md` for the full design.
//!
//! This module owns [`DriveCompactIndex`] (the loaded drive + its search choke
//! points) and re-exports the row type, the CSR indexes, path-length
//! computation, and the MFT→compact builder from focused submodules
//! (`record`, `children`, `extension`, `path_len`, `builder`, `delta`).

use alloc::borrow::Cow;
use alloc::sync::Arc;

use crate::bloom::Bloom;
pub use crate::compact_loader::apply_usn_patch;
// Re-export loader types and functions so callers can still use `compact::*`.
#[expect(deprecated, reason = "re-export kept for backward compatibility")]
pub use crate::compact_loader::{
    IndexSource, LoadTiming, MftSource, PatchStats, load_drive, load_drive_then, load_mft_file,
    refresh_drive,
};
use crate::compact_storage::ColumnStorage;
use crate::path_trie::PathTrie;
use crate::trigram::TrigramIndex;

/// Mutable delta overlay over the immutable base CSR indexes (Phase 2+).
pub mod delta;

// File-size decomposition: the row type, the CSR indexes, path-length
// computation, and the MFT→compact builder live in focused submodules.  Every
// public item is re-exported below so the canonical `crate::compact::X` paths
// (used across the workspace) are unchanged.
mod builder;
mod children;
mod extension;
mod path_len;
mod record;

pub use builder::build_compact_index;
pub(crate) use builder::{INDEX_TTL_SECONDS, resolve_case_fold};
pub use children::ChildrenIndex;
pub use delta::IndexDelta;
pub use extension::ExtensionIndex;
pub(crate) use path_len::{PathChange, compute_path_lengths, update_path_lengths_incremental};
pub(crate) use record::NTFS_METAFILE_NAMES;
pub use record::{CompactRecord, MalformedRender, is_ntfs_metafile_name};

/// Touched-record count (adds + tombstones since the last compaction) above
/// which [`DriveCompactIndex::apply_index_delta`] folds the delta back into
/// fresh bases (design §5.4). Sized to amortize the ~340 ms base rebuild
/// across many small USN applies while bounding delta memory + per-search merge
/// cost; tune from the apply-cost bench + live USN-apply DEBUG summaries.
pub(crate) const TRIGRAM_COMPACT_THRESHOLD: u32 = 50_000;

/// A loaded drive with compact index.
#[derive(Clone)]
pub struct DriveCompactIndex {
    /// Drive letter (e.g., 'C').
    pub letter: uffs_mft::platform::DriveLetter,
    /// Compact records — one per MFT file/directory.
    ///
    /// Backed by [`ColumnStorage`] so Phase 2b can transparently
    /// swap the heap-resident `Vec` for a memory-mapped runtime
    /// tempfile.  Read-side call sites use [`Deref<[T]>`]; mutating
    /// callers (Windows USN-patch path) go through
    /// `ColumnStorage::vec_for_append` (internal helper).
    pub records: ColumnStorage<CompactRecord>,
    /// All filenames concatenated (UTF-8 bytes, original case).
    ///
    /// Backed by [`ColumnStorage`]; see [`Self::records`] for the
    /// rationale.
    pub names: ColumnStorage<u8>,
    /// Trigram inverted index built from folded names (char-level, `$UpCase`).
    ///
    /// `Arc`-shared (Phase 3): the per-apply whole-body clone the daemon takes
    /// before patching pointer-clones this immutable base (a refcount bump)
    /// instead of deep-copying its ~hundreds-of-MB CSR arrays. The apply path
    /// never mutates it in place — it overlays changes on [`Self::delta`] and
    /// only ever *replaces* the whole `Arc` at compaction.
    pub trigram: Arc<TrigramIndex>,
    /// CSR children index: `children.get(i)` → child indices of record i.
    /// `Arc`-shared (Phase 3) — see [`Self::trigram`]; rebuilt (Arc replaced)
    /// each apply until Phase 4 gives it a delta overlay.
    pub children: Arc<ChildrenIndex>,
    /// Extension inverted index: `ext_id → record indices`.
    /// Enables O(K) `--ext` queries where K = matching records, not O(N).
    /// `Arc`-shared (Phase 3) — see [`Self::trigram`].
    pub ext_index: Arc<ExtensionIndex>,
    /// NTFS `$UpCase` case folding engine for this volume.
    pub fold: uffs_text::case_fold::CaseFold,
    /// Extension name table: `ext_names[extension_id]` → lowercase extension
    /// string (e.g. `"rs"`, `"txt"`). Index 0 = no extension.
    /// Used to resolve `--ext` filter strings to `u16` IDs for O(1)
    /// per-record matching instead of per-record string parsing.
    pub ext_names: Vec<Box<str>>,
    /// Where this index was loaded from (for future refresh).
    pub source: IndexSource,
    /// `MftIndex.build_epoch` this compact index was built from.
    /// Used as a staleness check when loading from cache.
    pub source_epoch: u64,
    /// Phase 4 bloom filter over folded basenames + extensions.
    ///
    /// `None` for indices built before bloom integration landed (e.g.
    /// in-process unit-test fixtures that don't exercise the Phase 4
    /// search-skip path) and for v ≤ 8 caches before the rebuild step
    /// runs.  After [`build_compact_index`] or a v9+ cache load this
    /// is always `Some(_)`; downstream callers
    /// (`search_dispatch::bloom_pre_check`) treat `None` as "no
    /// pre-check available; fall through to the full search" which
    /// is the safe (correct-but-slower) default.
    pub bloom: Option<Bloom>,
    /// Phase 4 directory-only path trie.  Same `None`-handling
    /// rationale as [`Self::bloom`].
    pub path_trie: Option<PathTrie>,
    /// Phase 8: FRS → `compact_idx` mapping.
    ///
    /// Indexed by FRS-as-`usize`; values are the matching primary
    /// `compact_idx` in [`Self::records`], or [`u32::MAX`] for
    /// unmapped slots (system metafiles 0-15, FRS values higher
    /// than the build-time max, deleted records).
    ///
    /// Populated by [`build_compact_index`] from
    /// [`uffs_mft::MftIndex::frs_to_idx`] (which is otherwise
    /// dropped when the `MftIndex` goes out of scope).  Maintained
    /// in lock-step with [`Self::records`] by
    /// [`crate::compact_loader::apply_usn_patch`] across
    /// create / delete / rename batches: creates extend the table
    /// and assign the new compact slot, deletes mark the slot
    /// `u32::MAX`, renames leave the slot intact (only `parent_idx`
    /// + name move).
    ///
    /// **Why not stored in `MftIndex`?**  The `MftIndex` is
    /// transient — `build_compact_index` consumes it and drops it.
    /// The compact body is what survives to serve search queries
    /// and accept journal patches.  Keeping the mapping next to the
    /// records it indexes means [`crate::compact_loader::apply_usn_patch`]
    /// can patch the body in place without touching the MFT.
    ///
    /// **Backward compatibility**: caches written before v10
    /// (Phase 8) didn't persist this mapping; for those, the field
    /// loads as an empty `Vec` and the surgical patch path
    /// silently degrades to the full-reload fallback.  See the
    /// v9 → v10 cache format bump in `compact_cache::COMPACT_VERSION`.
    pub frs_to_compact: Vec<u32>,
    /// Incremental-index-maintenance overlay (design §5.1).
    ///
    /// `None` on a freshly built / freshly compacted / cache-loaded index:
    /// the base CSR indexes ([`Self::trigram`], [`Self::children`],
    /// [`Self::ext_index`]) are authoritative and search reads them with zero
    /// overhead. Once [`crate::compact_loader::apply_usn_patch`] starts
    /// overlaying USN deltas (Phase 2b) this becomes `Some`, and the search
    /// choke points ([`Self::trigram_search`], …) merge base ∪ delta minus
    /// tombstones. Compaction folds the delta into a fresh base and resets it
    /// to `None`. Never serialized — the on-disk cache is always delta-free
    /// (compact before save), so a cache load yields `None`.
    pub delta: Option<IndexDelta>,
}

/// Per-component heap footprint of a [`DriveCompactIndex`].
#[derive(Debug, Clone)]
pub struct HeapReport {
    /// `records: ColumnStorage<CompactRecord>` capacity in bytes.
    /// Mmap-backed columns (Phase 2b) report `len * sizeof(T)`
    /// since the kernel-mapped pages have no extra slack.
    pub records: usize,
    /// `names: ColumnStorage<u8>` capacity in bytes.
    pub names: usize,
    /// `TrigramIndex` total heap (keys + offsets + values).
    pub trigram: usize,
    /// `ChildrenIndex` total heap (offsets + values).
    pub children: usize,
    /// `ExtensionIndex` total heap (offsets + values).
    pub ext_index: usize,
    /// `ext_names: Vec<Box<str>>` heap (Vec + string data).
    pub ext_names: usize,
    /// `frs_to_compact: Vec<u32>` capacity in bytes (Phase 8 —
    /// `~max_frs * 4` bytes; ~40 MB on a 7M-record drive with
    /// max FRS ≈ 10M).
    pub frs_to_compact: usize,
    /// Sum of all components.
    pub total: usize,
}

impl AsRef<Self> for DriveCompactIndex {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl DriveCompactIndex {
    /// Trigram candidate search through the base ∪ delta overlay (design §5.2).
    ///
    /// The single choke point every trigram caller goes through. When
    /// [`Self::delta`] is `None` (fresh / compacted index) it delegates to the
    /// base [`TrigramIndex::search`] with **zero** overhead. When a delta is
    /// present it merges, per needle-trigram, the base posting with the delta
    /// posting, intersects across the needle's trigrams (the trigram AND), then
    /// resolves tombstones on the final candidate set.
    ///
    /// **Tombstone correctness:** a candidate whose record is tombstoned is
    /// kept **iff** it appears in the delta posting of *every* needle
    /// trigram — i.e. it was re-added (renamed-in) under a name that still
    /// contains the needle. A deleted record (tombstoned, no re-add) and a
    /// renamed-away record matched only via its stale base postings are
    /// both dropped. Filtering the final set (not per posting list) is what
    /// lets a renamed file remain visible under its new name while
    /// disappearing from its old one.
    ///
    /// Returns `None` for needles under 3 codepoints (caller falls back to a
    /// linear scan), mirroring [`TrigramIndex::search`].
    #[must_use]
    pub fn trigram_search(&self, needle: &str) -> Option<Vec<u32>> {
        let Some(delta) = &self.delta else {
            return self.trigram.search(needle, self.fold);
        };
        let trigrams = crate::trigram::needle_trigrams(needle, self.fold)?;
        if trigrams.is_empty() {
            return Some(Vec::new());
        }

        // Per needle-trigram effective posting = base ∪ delta. An absent
        // trigram (empty in both) is skipped, never zeroing the result
        // — the trigram index is a candidate pre-filter, exactly as the
        // base search treats it.
        let mut lists: Vec<Vec<u32>> = Vec::with_capacity(trigrams.len());
        for &tri in &trigrams {
            let base = self.trigram.get_posting(tri).unwrap_or(&[]);
            let merged = delta::merge_postings(base, delta.trigram_postings(tri));
            if !merged.is_empty() {
                lists.push(merged);
            }
        }
        if lists.is_empty() {
            return Some(Vec::new());
        }

        lists.sort_unstable_by_key(Vec::len);
        let mut result = lists.first().cloned().unwrap_or_default();
        for list in lists.iter().skip(1) {
            crate::trigram::intersect_in_place(&mut result, list);
            if result.is_empty() {
                break;
            }
        }

        // Final tombstone resolution: keep a tombstoned candidate only if it
        // was re-added under a name covering every needle trigram (see
        // doc above).
        if !delta.tombstones.is_empty() {
            result.retain(|&idx| {
                !delta.is_tombstoned(idx)
                    || trigrams
                        .iter()
                        .all(|&tri| delta.trigram_postings(tri).binary_search(&idx).is_ok())
            });
        }
        Some(result)
    }

    /// Fold the delta overlay back into fresh bases and clear it (design §5.4
    /// compaction). Rebuilds the trigram (Phase 2b) and extension (Phase 4a)
    /// bases from the current records — which already reflect every applied
    /// mutation — then resets `delta = None` so subsequent searches take the
    /// zero-overhead base fast path.
    ///
    /// Fold any pending delta overlay into fresh bases so this index
    /// is safe to serialize — the boundary every save path must cross.
    ///
    /// A delta-carrying index is UNSERIALIZABLE as-is: the patch path
    /// appends created records (the header's record count grows) while
    /// `children` / `trigram` / `ext_index` stay the Arc-shared bases,
    /// so the writer emits a children CSR sized for the OLD record
    /// count and the reader — which sizes the section from the header
    /// — mis-walks every later section (field signature: `bloom
    /// k_hashes out of range`, quarantining an otherwise-healthy cache
    /// on the next start; winbox M/C/D/S 2026-08-24, and the 2026-08
    /// "drive C for ten days" incident).  Even an aligned save would
    /// persist a trigram base missing the new records — a silent
    /// substring-search hole after reload.  No-op when `delta` is
    /// already `None`, so calling it unconditionally before a save is
    /// free in the steady state.
    pub fn fold_delta_for_save(&mut self) {
        if self.delta.is_some() {
            self.compact_base();
        }
    }

    /// O(total records); the per-apply path drives toward this running only
    /// occasionally (every [`TRIGRAM_COMPACT_THRESHOLD`] touched records) or
    /// before serialization (the on-disk cache is always delta-free —
    /// enforced by [`Self::fold_delta_for_save`] +
    /// `save_compact_cache_background`'s refusal guard).
    pub(crate) fn compact_base(&mut self) {
        self.trigram = Arc::new(TrigramIndex::build(&self.records, &self.names, self.fold));
        self.ext_index = Arc::new(ExtensionIndex::build(&self.records));
        self.children = Arc::new(ChildrenIndex::build(&self.records));
        self.delta = None;
    }

    /// Invoke `f` for each live child record index of `parent`, through the
    /// base ∪ delta overlay (Phase 4b). Zero allocation.
    ///
    /// When [`Self::delta`] is `None` the base CSR is authoritative (it was
    /// built from the current records and nothing has moved since) so it is
    /// iterated directly. With a delta present, base ∪ delta children are
    /// sorted-merged and each is validated against the live records — kept iff
    /// `records[c].parent_idx == parent` and the record is live. That records
    /// check is what makes a moved-away or deleted child correct **without** a
    /// children tombstone.
    pub fn for_each_child<F: FnMut(u32)>(&self, parent: u32, mut visit: F) {
        let base = self.children.get(parent as usize);
        let Some(delta) = &self.delta else {
            for &child in base {
                visit(child);
            }
            return;
        };
        let is_valid = |child: u32| {
            self.records
                .get(child as usize)
                .is_some_and(|rec| rec.parent_idx == parent && rec.name_len != 0)
        };
        delta::merge_filter(base, delta.child_postings(parent), is_valid, visit);
    }

    /// Live child record indices of `parent`, through the base ∪ delta overlay
    /// (Phase 4b) — the slice-returning form of [`Self::for_each_child`] for
    /// callers that need an owned/borrowed list. Zero-alloc `Cow::Borrowed`
    /// when [`Self::delta`] is `None`.
    #[must_use]
    pub fn children_of(&self, parent: u32) -> Cow<'_, [u32]> {
        if self.delta.is_none() {
            return Cow::Borrowed(self.children.get(parent as usize));
        }
        let mut out = Vec::new();
        self.for_each_child(parent, |child| out.push(child));
        Cow::Owned(out)
    }

    /// Record indices whose extension is `ext_id`, through the base ∪ delta
    /// overlay (Phase 4a). The choke point every `--ext` query goes through.
    ///
    /// When [`Self::delta`] is `None` this borrows the base CSR posting slice
    /// with **zero** allocation. With a delta present it merges the base and
    /// delta postings, then validates each candidate against the live records —
    /// keeping `idx` only if `records[idx].extension_id == ext_id` and the
    /// record is live (`name_len != 0`). That records check is what makes a
    /// renamed extension (`foo.log` → `foo.pdf`) and a delete correct
    /// **without** a separate ext tombstone: a stale base posting fails the
    /// check.
    #[must_use]
    pub fn records_with_ext(&self, ext_id: u16) -> Cow<'_, [u32]> {
        let base = self.ext_index.get(ext_id);
        let Some(delta) = &self.delta else {
            return Cow::Borrowed(base);
        };
        let merged = delta::merge_postings(base, delta.ext_postings(ext_id));
        let filtered: Vec<u32> = merged
            .into_iter()
            .filter(|&idx| {
                self.records
                    .get(idx as usize)
                    .is_some_and(|rec| rec.extension_id == ext_id && rec.name_len != 0)
            })
            .collect();
        Cow::Owned(filtered)
    }

    /// Overlay one USN apply's changes onto the base+delta index instead of
    /// rebuilding the trigram (Phase 2b) and extension (Phase 4a) bases.
    ///
    /// `adds` are the created / renamed / reused records (their post-mutation
    /// name trigrams + extension + parent are added to the delta); `tombstones`
    /// are the deleted / renamed-away / reused-slot records (their stale base
    /// **trigram** postings are masked — the ext/children overlays validate
    /// candidates against the live records instead, so they need no tombstone).
    /// Returns `true` if the accumulated delta crossed
    /// [`TRIGRAM_COMPACT_THRESHOLD`] and triggered a [`Self::compact_base`]
    /// fold this call.
    pub(crate) fn apply_index_delta(&mut self, adds: &[PathChange], tombstones: &[u32]) -> bool {
        // Fast path for a batch that will cross the compaction threshold anyway
        // (e.g. a 100k-file burst): populating the delta only to discard it is
        // pure waste. Refold the base directly from the records — which already
        // reflect every change in this batch — and drop any prior overlay.
        let pending = self.delta.as_ref().map_or(0, IndexDelta::len);
        let batch = u32::try_from(adds.len().saturating_add(tombstones.len())).unwrap_or(u32::MAX);
        if pending.saturating_add(batch) > TRIGRAM_COMPACT_THRESHOLD {
            self.compact_base();
            return true;
        }

        let mut delta = self.delta.take().unwrap_or_default();
        for &idx in tombstones {
            delta.tombstone(idx);
        }
        let fold = self.fold;
        for change in adds {
            let Some(rec) = self.records.get(change.idx as usize) else {
                continue;
            };
            // A record tombstoned this same batch (e.g. created-then-deleted)
            // is gone; skip its add entirely.
            if rec.name_len == 0 {
                continue;
            }
            // Trigram postings only for names ≥ 3 codepoints (shorter names are
            // found via linear scan, not the trigram pre-filter — matching the
            // base build); but the extension + children overlays are added for
            // EVERY record regardless of name length, so an `--ext` / tree
            // query never misses a short-named create/rename.
            let trigrams =
                crate::trigram::needle_trigrams(rec.name(&self.names), fold).unwrap_or_default();
            delta.add_record(change.idx, &trigrams, rec.extension_id, rec.parent_idx);
        }
        // The early check above guarantees `pending + batch ≤ threshold`, and
        // the populated delta can only be ≤ that, so no compaction is due here.
        self.delta = Some(delta);
        false
    }

    /// Compute the total heap footprint of this index (in bytes).
    ///
    /// This measures *capacity* (what the allocator reserved), not *len*
    /// (what we're using).  The gap between the two is what `shrink_to_fit`
    /// reclaims.  Use this after loading to verify memory usage.
    #[must_use]
    pub fn heap_size_bytes(&self) -> HeapReport {
        let records = self.records.capacity() * size_of::<CompactRecord>();
        let names = self.names.capacity();
        let trigram = self.trigram.heap_size_bytes();
        let children = self.children.heap_size_bytes();
        let ext_index = self.ext_index.heap_size_bytes();
        let ext_names_data: usize = self.ext_names.iter().map(|en| en.len()).sum();
        let ext_names_vec = self.ext_names.capacity() * size_of::<Box<str>>();
        let ext_names = ext_names_data + ext_names_vec;
        let frs_to_compact = self.frs_to_compact.capacity() * size_of::<u32>();
        HeapReport {
            records,
            names,
            trigram,
            children,
            ext_index,
            ext_names,
            frs_to_compact,
            total: records + names + trigram + children + ext_index + ext_names + frs_to_compact,
        }
    }

    /// Log the heap report at `info` level.
    pub fn log_heap_report(&self) {
        let hr = self.heap_size_bytes();
        let mb = |bytes: usize| bytes / (1024 * 1024);
        tracing::info!(
            drive = %self.letter,
            records_count = self.records.len(),
            records_mb = mb(hr.records),
            names_mb = mb(hr.names),
            trigram_mb = mb(hr.trigram),
            children_mb = mb(hr.children),
            ext_index_mb = mb(hr.ext_index),
            ext_names_mb = mb(hr.ext_names),
            frs_to_compact_mb = mb(hr.frs_to_compact),
            total_mb = mb(hr.total),
            "[HEAP] {}: rec={} names={} tri={} ch={} ext={} f2c={} | total={} MB",
            self.letter,
            mb(hr.records), mb(hr.names), mb(hr.trigram),
            mb(hr.children), mb(hr.ext_index), mb(hr.frs_to_compact),
            mb(hr.total),
        );
    }

    /// Resolve extension filter strings to their `u16` IDs on this drive.
    ///
    /// Returns a sorted, deduplicated `Vec<u16>` of matching IDs.
    /// Extensions not found on this drive are silently ignored.
    ///
    /// The lookup is a linear scan of `ext_names` (~500–2000 short strings),
    /// which takes < 1 µs.  This runs **once per search per drive**, not per
    /// record.
    ///
    /// Uses `.enumerate()` rather than `(0_u16..).zip(...)`: `ext_names` can
    /// legitimately grow to `u16::MAX` entries (see
    /// [`Self::intern_extension`]'s own ceiling check), and
    /// `RangeFrom<u16>::next()` computes its *next* state eagerly before
    /// yielding — so an unbounded `0_u16..` zipped against a full-length
    /// table overflows on the lookup's final step whenever the query misses
    /// (queried extension absent from this drive), even though every
    /// `ext_id` it ever needed to yield fit in `u16`.
    #[must_use]
    pub(crate) fn resolve_ext_ids(&self, extensions: &[String]) -> Vec<u16> {
        let mut ids = Vec::with_capacity(extensions.len());
        for ext in extensions {
            let normalized = ext.trim().trim_start_matches('.').to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            for (idx, name) in self.ext_names.iter().enumerate() {
                if name.as_ref() == normalized {
                    ids.push(u16::try_from(idx).unwrap_or(0));
                    break;
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Extract `filename`'s extension and intern it into [`Self::ext_names`],
    /// returning its `extension_id`.
    ///
    /// Mirrors the build-time logic (`MftIndex::intern_extension` +
    /// `ExtensionTable::intern`) so a record created by the USN journal patch
    /// path lands under the SAME `extension_id` a full rebuild would assign —
    /// otherwise `--ext <x>` (which resolves the name via [`Self::ext_names`]
    /// and looks it up in the [`ExtensionIndex`]) silently misses the new file.
    ///
    /// Returns `0` (the reserved "no extension" id) for a dotless name, a
    /// leading-dot dotfile (`.gitignore`), a trailing-dot name (`file.`), or if
    /// the table is already at the `u16::MAX` interning ceiling.
    pub(crate) fn intern_extension(&mut self, filename: &str) -> u16 {
        // Extension = substring after the LAST dot, where the dot is neither
        // the first byte (dotfile) nor the last (trailing dot).
        let Some(dot_pos) = filename.rfind('.') else {
            return 0;
        };
        if dot_pos == 0 || dot_pos + 1 >= filename.len() {
            return 0;
        }
        let Some(raw_ext) = filename.get(dot_pos + 1..) else {
            return 0;
        };
        let normalized = raw_ext.trim_start_matches('.').to_lowercase();
        if normalized.is_empty() {
            return 0;
        }

        // Find-or-append. `ext_names[0]` is the reserved "" (no-extension)
        // slot, so a real extension never collides with id 0.
        if let Some(existing) = self
            .ext_names
            .iter()
            .position(|name| name.as_ref() == normalized)
        {
            return u16::try_from(existing).unwrap_or(0);
        }
        let Ok(new_id) = u16::try_from(self.ext_names.len()) else {
            // Interning ceiling reached (>= 65 535 distinct extensions);
            // fall back to "no extension" rather than wrap.
            return 0;
        };
        if new_id == u16::MAX {
            return 0;
        }
        self.ext_names.push(normalized.into_boxed_str());
        new_id
    }
}

// ════════════════════════════════════════════════════════════════════════
// REGRESSION TESTS — Search Pipeline Parity Guards
//
// These tests protect critical behaviors that broke during the v0.4.30
// refactor attempt.  They run on synthetic data (no Windows/MFT needed).
// See `docs/architecture/2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.
// md` ════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compact_trigram_delta_tests.rs"]
mod trigram_delta_tests;
