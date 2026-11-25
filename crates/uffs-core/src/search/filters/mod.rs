// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Extended post-search filters and NTFS attribute helpers.
//!
//! [`SearchFilters`] holds pre-parsed filter criteria. All parsing (time
//! bounds, attribute bits) happens at construction time so the hot `retain`
//! loop is branch-only.
//!
//! Exception: the `SearchFilters` / `SearchFilterParams` definitions and the
//! `from_params` constructor stay together so the full per-field filter
//! contract is auditable in one place (see
//! `scripts/ci/file_size_exceptions.txt`).

mod apply;
mod attr_parsing;
mod ext_match;
mod path_normalize;
mod time_parsing;

// `apply_search_filters` is a deliberately public re-export — see its
// own doc comment for why a downstream crate needs to call it directly.
// The `apply::*` glob below stays `pub(crate)`: everything else in
// `apply` (e.g. `row_passes_filters`) is an internal helper.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

pub(crate) use apply::*;
pub use apply::{apply_search_filters, fold_exclude_pattern};
pub use attr_parsing::*;
pub(crate) use ext_match::extract_extension_after_dot;
pub(super) use ext_match::{extension_matches_filter, lowercase_into};
pub(super) use path_normalize::normalize_path_separators;
pub use time_parsing::*;

use super::backend::{DisplayRow, FilterMode};
use crate::compact::CompactRecord;
use crate::search::tree::name_matches;

/// Apply filter mode to a set of display rows.
pub(crate) fn apply_filter(rows: &mut Vec<DisplayRow>, filter: FilterMode) {
    match filter {
        FilterMode::All => {}
        FilterMode::FilesOnly => rows.retain(|row| !row.is_directory),
        FilterMode::DirsOnly => rows.retain(|row| row.is_directory),
    }
}

/// Extended post-search filters.
///
/// All fields are pre-parsed so the per-row `retain` loop is branch-only
/// (no parsing).
///
/// `Clone` is provided so the per-drive scan in
/// `collect_global_top_n_numeric` can hand each rayon worker its own
/// `resolved_ext_ids` without contending on a shared `&mut` reference.
/// The clone cost is one `Vec<String>` + one `Vec<u16>` per drive
/// (~100 B per drive at typical filter sizes) — negligible against the
/// ~4 M-record per-drive scan the parallelism enables.
#[derive(Debug, Default, Clone)]
pub struct SearchFilters {
    /// Hide reserved NTFS metafiles (`$MFT`, `$LogFile`, the `$Extend` family,
    /// …) — see [`crate::compact::is_ntfs_metafile_name`].  Ordinary
    /// `$`-prefixed files (`$Recycle.Bin`, `WinSxS` `$$_*.cdf-ms`) are NOT
    /// hidden.
    pub hide_system: bool,
    /// Hide NTFS Alternate Data Streams (names containing `:`).
    pub hide_ads: bool,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Modified-time lower bound (Unix µs, inclusive).
    pub newer_us: Option<i64>,
    /// Modified-time upper bound (Unix µs, exclusive).
    pub older_us: Option<i64>,
    /// Created-time lower bound (Unix µs, inclusive).
    pub newer_created_us: Option<i64>,
    /// Created-time upper bound (Unix µs, exclusive).
    pub older_created_us: Option<i64>,
    /// Accessed-time lower bound (Unix µs, inclusive).
    pub newer_accessed_us: Option<i64>,
    /// Accessed-time upper bound (Unix µs, exclusive).
    pub older_accessed_us: Option<i64>,
    /// Required attribute bits (all must be set).
    pub attr_require: u32,
    /// Excluded attribute bits (none may be set).
    pub attr_exclude: u32,
    /// Minimum descendant count (inclusive).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (inclusive).
    pub max_descendants: Option<u32>,
    /// Allowed extensions (lowercase, without dot). Empty = no filter.
    pub extensions: Vec<String>,
    /// Pre-resolved extension IDs for the current drive.
    /// Set via `resolve_ext_ids_for_drive` (internal helper) before the hot
    /// loop — enables O(1) `u16` comparison per record instead of per-record
    /// string parsing.
    pub resolved_ext_ids: Vec<u16>,
    /// Exclude pattern (glob, `$UpCase`-folded by [`fold_exclude_pattern`]).
    pub exclude_lower: Option<String>,
    /// Directory-path pattern (glob, lowered). Matches against `path_dir()`
    /// only.
    pub path_contains_lower: Option<String>,
    /// Directory-path **exclude** globs (lowered, separator-normalized). A
    /// record is dropped when its `path_dir()` matches any entry.
    pub path_excludes_lower: Option<Vec<String>>,
    /// File type/category filter (e.g. `"code"`, `"document"`, `"picture"`).
    pub type_filter: Option<String>,
    /// Minimum bulkiness in **per-million** scale.
    ///
    /// `1_000_000` = 100% (perfectly packed).  `2_000_000` = 200%.
    /// CLI percentages must be converted via `from_params` which multiplies
    /// by 10 000.
    pub min_bulkiness: Option<u64>,
    /// Maximum bulkiness in **per-million** scale (see
    /// [`Self::min_bulkiness`]).
    pub max_bulkiness: Option<u64>,

    // ── Length filters ──────────────────────────────────────────────
    /// Minimum filename length (characters).
    pub min_name_len: Option<u16>,
    /// Maximum filename length (characters).
    pub max_name_len: Option<u16>,
    /// Minimum full-path length (characters).
    pub min_path_len: Option<u16>,
    /// Maximum full-path length (characters, useful for `MAX_PATH` detection).
    pub max_path_len: Option<u16>,

    // ── Size-on-disk filters ───────────────────────────────────────
    /// Minimum allocated (on-disk) size in bytes.
    pub min_allocated: Option<u64>,
    /// Maximum allocated (on-disk) size in bytes.
    pub max_allocated: Option<u64>,

    // ── Tree metric filters ─────────────────────────────────────────
    /// Minimum subtree logical size in bytes (directories).
    pub min_treesize: Option<u64>,
    /// Maximum subtree logical size in bytes (directories).
    pub max_treesize: Option<u64>,
    /// Minimum subtree allocated (on-disk) size in bytes (directories).
    pub min_tree_allocated: Option<u64>,
    /// Maximum subtree allocated (on-disk) size in bytes (directories).
    pub max_tree_allocated: Option<u64>,

    // ── Month-of-year / quarter filter ─────────────────────────────
    /// Set of allowed months (1-12). Empty = no filter.
    /// Used for "every January" or "Q1" style queries.
    pub allowed_months: Vec<u32>,

    // ── WI-4.4 malformed-name filter ────────────────────────────────
    /// Filter on whether the record's own leaf name is ill-formed (its true
    /// bytes are not valid UTF-8). `Some(true)` keeps only malformed names;
    /// `Some(false)` keeps only well-formed names; `None` = no filter.
    ///
    /// Evaluated in the hot path against [`CompactRecord::name_bytes`] (the
    /// lossless bytes), never the lossy `&str` view (which is always valid
    /// UTF-8 and would match nothing).
    pub malformed: Option<bool>,

    /// Filter on whether the record is a **deleted tombstone** — its
    /// [`crate::diff::DELETED_TOMBSTONE_FLAG`] bit is set. `Some(true)` keeps
    /// only deleted records; `Some(false)` only live ones; `None` = no filter.
    ///
    /// Populated by the snapshot-diff path, which marks the baseline rows that
    /// vanished from the current index before running the normal search over
    /// the baseline — so deleted files are filterable by every other criterion.
    pub deleted: Option<bool>,

    /// Render ill-formed names with greppable `<BAD:HHHH>` markers instead of
    /// the default U+FFFD (`�`). A display-only option (not a filter): it
    /// selects [`crate::compact::MalformedRender`] for the resolved path + name
    /// column so downstream tooling can spot/round-trip corrupt entries.
    /// `false` = default lossy rendering (matches the reference C++ tool).
    pub normalize_malformed: bool,

    /// Cooperative cancellation handle for the scan that consumes these
    /// filters.  `None` (the default) means the scan runs to completion.
    ///
    /// The daemon sets this before it hands a search to the blocking
    /// pool and raises the flag when the search's scan budget expires:
    /// every per-drive scan polls it on its hot loop (see
    /// [`Self::cancelled_at`]) and stops instead of running the
    /// remaining records to the end for a result nobody will read.
    /// Before this existed a timed-out `*.*` over 43 M records kept its
    /// rayon workers busy for the full scan after the daemon had already
    /// answered the client (2026-10-03 benchmark run).
    ///
    /// Travels with the per-drive `clone()` so rayon workers share the
    /// one flag.  Not a filter: [`Self::is_empty`] ignores it.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Records a hot loop processes between two cancellation polls.
///
/// A power of two so the stride test is a mask.  One relaxed atomic
/// load per 4 096 records is below measurement noise on every scan path
/// while still bounding the overrun after a cancel to a few microseconds
/// per worker.
pub const CANCEL_CHECK_STRIDE: usize = 4096;

impl SearchFilters {
    /// The [`crate::compact::MalformedRender`] mode implied by
    /// [`Self::normalize_malformed`].
    #[must_use]
    pub const fn malformed_render(&self) -> crate::compact::MalformedRender {
        if self.normalize_malformed {
            crate::compact::MalformedRender::Normalized
        } else {
            crate::compact::MalformedRender::Lossy
        }
    }

    /// `true` once the owning search has been cancelled (see
    /// [`Self::cancel`]).  Cheap enough for per-drive granularity; hot
    /// per-record loops use [`Self::cancelled_at`] instead.
    #[inline]
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    /// `true` when `ordinal` sits on a [`CANCEL_CHECK_STRIDE`] boundary
    /// *and* the search has been cancelled.  Folds the stride test and
    /// the atomic load into one `if` so a hot loop pays a single mask
    /// compare per record and the load only every 4 096th.
    #[inline]
    #[must_use]
    pub fn cancelled_at(&self, ordinal: usize) -> bool {
        ordinal & (CANCEL_CHECK_STRIDE - 1) == 0 && self.is_cancelled()
    }
}

/// Raw parameter inputs for constructing [`SearchFilters`].
///
/// All fields default to `None` / `false` / empty — callers only set what
/// they need.  This replaces the former 27-argument positional function
/// signature with a named-field struct that is self-documenting and
/// extensible without touching every call site.
#[derive(Debug, Default)]
pub struct SearchFilterParams<'a> {
    /// Hide reserved NTFS metafiles (`$MFT`, `$LogFile`, the `$Extend` family,
    /// …).  Ordinary `$`-prefixed files are NOT hidden.
    pub hide_system: bool,
    /// Hide NTFS Alternate Data Streams.
    pub hide_ads: bool,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Minimum descendant count (directories).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (directories).
    pub max_descendants: Option<u32>,
    /// Modified-time lower bound spec (e.g. `"1h"`, `"2024-01-01"`).
    pub newer: Option<&'a str>,
    /// Modified-time upper bound spec.
    pub older: Option<&'a str>,
    /// Created-time lower bound spec.
    pub newer_created: Option<&'a str>,
    /// Created-time upper bound spec.
    pub older_created: Option<&'a str>,
    /// Accessed-time lower bound spec.
    pub newer_accessed: Option<&'a str>,
    /// Accessed-time upper bound spec.
    pub older_accessed: Option<&'a str>,
    /// NTFS attribute filter string (e.g. `"hidden,!system"`).
    pub attr_filter: Option<&'a str>,
    /// Extension filter string (e.g. `"rs,jpg,pictures"`).
    pub ext_filter: Option<&'a str>,
    /// Exclude pattern (glob, e.g. `"backup*"`).
    pub exclude: Option<&'a str>,
    /// Directory-path pattern (glob, matched against dir portion only).
    pub path_contains: Option<&'a str>,
    /// Directory-path exclude globs, comma-separated (matched against the dir
    /// portion only; a record is dropped if its directory matches **any**).
    pub path_excludes: Option<&'a str>,
    /// File type/category filter (e.g. `"code"`, `"document"`).
    pub type_filter: Option<&'a str>,
    /// Minimum bulkiness percentage (e.g. `200` = allocated ≥ 2× size).
    pub min_bulkiness: Option<u64>,
    /// Maximum bulkiness percentage.
    pub max_bulkiness: Option<u64>,
    /// Minimum filename length (characters).
    pub min_name_len: Option<u16>,
    /// Maximum filename length (characters).
    pub max_name_len: Option<u16>,
    /// Minimum full-path length (characters).
    pub min_path_len: Option<u16>,
    /// Maximum full-path length (characters).
    pub max_path_len: Option<u16>,
    /// Minimum allocated (on-disk) size in bytes.
    pub min_allocated: Option<u64>,
    /// Maximum allocated (on-disk) size in bytes.
    pub max_allocated: Option<u64>,
    /// Minimum subtree logical size in bytes.
    pub min_treesize: Option<u64>,
    /// Maximum subtree logical size in bytes.
    pub max_treesize: Option<u64>,
    /// Minimum subtree allocated (on-disk) size in bytes.
    pub min_tree_allocated: Option<u64>,
    /// Maximum subtree allocated (on-disk) size in bytes.
    pub max_tree_allocated: Option<u64>,
    /// Allowed month numbers (1-12).
    pub allowed_months: &'a [u32],
}

impl SearchFilters {
    /// Build `SearchFilters` from a [`SearchFilterParams`] struct.
    ///
    /// This is the generic constructor shared by CLI, TUI, daemon, etc.
    /// All time-spec parsing and attribute parsing happens here so the
    /// hot-path `matches_record` loop is branch-only.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "constructs SearchFilters from 15+ filter parameters with extension \
                  normalization and type_filter→extension promotion; splitting the \
                  constructor would obscure the field-by-field setup"
    )]
    pub fn from_params(params: &SearchFilterParams<'_>) -> Self {
        let now_us = now_unix_micros();
        let extensions: Vec<String> = params.ext_filter.map_or_default(|ext_list| {
            let mut exts = Vec::new();
            for segment in ext_list.split(',') {
                let token = segment.trim().trim_start_matches('.').to_lowercase();
                if token.is_empty() {
                    continue;
                }
                if let Some(collection) = crate::extensions::expand_collection(&token) {
                    exts.extend(collection.iter().map(|ext| (*ext).to_owned()));
                } else {
                    exts.push(token);
                }
            }
            exts
        });
        if !extensions.is_empty() {
            tracing::trace!(
                raw_ext_filter = params.ext_filter.unwrap_or_default(),
                normalized_extensions = ?extensions,
                "normalized extension filter strings"
            );
        }
        let exclude_lower = params.exclude.map(fold_exclude_pattern);
        let path_contains_lower = params.path_contains.map(|pat| {
            // path_dir() is lowered via `to_ascii_lowercase()`, so the
            // pattern must also be plain ASCII-lowered — NOT $UpCase folded
            // (which produces uppercase and would mismatch).
            //
            // Normalize path separators:
            // 1. Replace `/` with `\` (users may use forward slashes).
            // 2. Collapse runs of `\` into a single `\` (transport layers may
            //    double-encode backslashes, turning `\` into `\\`).
            let lowered = pat.to_ascii_lowercase();
            normalize_path_separators(&lowered)
        });
        // Comma-list of dir globs (`*appdata*,*.cargo*,…`), normalized like
        // `path_contains`; see [`path_normalize::parse_path_excludes`].
        let path_excludes_lower = path_normalize::parse_path_excludes(params.path_excludes);

        // ── Promote type_filter → extensions for early filtering ─────
        //
        // When the type maps to a known extension list (e.g. "code" →
        // [rs, py, js, …]) we push those extensions into `extensions` so
        // that `matches_record` can filter records during the scan (O(1)
        // per record via ext-index) instead of the expensive post-filter
        // path that requires full path resolution for every candidate.
        //
        // If --ext was also provided, the type list is a superset — we
        // intersect them so only extensions satisfying BOTH constraints
        // survive.
        //
        // Un-mappable types ("directory", "file", "other") stay as
        // `type_filter` for post-filter via `apply_search_filters`.
        #[expect(
            clippy::shadow_reuse,
            reason = "intentional: refine extensions with type_filter"
        )]
        let (extensions, type_filter) = if let Some(type_name) = params.type_filter {
            let lower = type_name.to_ascii_lowercase();
            if let Some(type_exts) = crate::search::derived::extensions_for_type(&lower) {
                let merged = if extensions.is_empty() {
                    // No --ext: use the full type extension list.
                    type_exts.iter().map(|ext| (*ext).to_owned()).collect()
                } else {
                    // --ext present: intersect (keep only exts that
                    // belong to BOTH the explicit list and the type).
                    extensions
                        .into_iter()
                        .filter(|ext| type_exts.contains(&ext.as_str()))
                        .collect()
                };
                (merged, None)
            } else {
                // Un-mappable type (directory/file/other) — keep post-filter.
                (extensions, Some(lower))
            }
        } else {
            (extensions, None)
        };

        Self {
            hide_system: params.hide_system,
            hide_ads: params.hide_ads,
            min_size: params.min_size,
            max_size: params.max_size,
            newer_us: params
                .newer
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_us: params
                .older
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_created_us: params
                .newer_created
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_created_us: params
                .older_created
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_accessed_us: params
                .newer_accessed
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_accessed_us: params
                .older_accessed
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            attr_require: parse_attr_require(params.attr_filter.unwrap_or("")),
            attr_exclude: parse_attr_exclude(params.attr_filter.unwrap_or("")),
            min_descendants: params.min_descendants,
            max_descendants: params.max_descendants,
            extensions,
            resolved_ext_ids: Vec::new(),
            exclude_lower,
            path_contains_lower,
            path_excludes_lower,
            type_filter,
            // CLI bulkiness is a user-facing percentage (200 = 200%).
            // Internal scale is per-million (1_000_000 = 100%).
            // Convert: percentage × 10_000 = per-million.
            min_bulkiness: params.min_bulkiness.map(|pct| pct.saturating_mul(10_000)),
            max_bulkiness: params.max_bulkiness.map(|pct| pct.saturating_mul(10_000)),
            min_name_len: params.min_name_len,
            max_name_len: params.max_name_len,
            min_path_len: params.min_path_len,
            max_path_len: params.max_path_len,
            min_allocated: params.min_allocated,
            max_allocated: params.max_allocated,
            min_treesize: params.min_treesize,
            max_treesize: params.max_treesize,
            min_tree_allocated: params.min_tree_allocated,
            max_tree_allocated: params.max_tree_allocated,
            allowed_months: params.allowed_months.to_vec(),
            // The malformed-name filter is set by the daemon's canonical
            // predicate compiler (it is not a legacy positional param), so the
            // param-based constructor leaves it disabled.
            malformed: None,
            // Set by the snapshot-diff path (marks vanished baseline rows),
            // not a legacy positional param; disabled for a normal search.
            deleted: None,
            // Display-only; the daemon sets it from the request's
            // `normalize_malformed` flag, so it defaults off here.
            normalize_malformed: false,
            // Search-time state the daemon attaches per search, never a
            // parsed parameter.
            cancel: None,
        }
    }

    /// Set the minimum filename length filter.
    #[must_use]
    pub const fn with_min_name_len(mut self, len: Option<u16>) -> Self {
        self.min_name_len = len;
        self
    }

    /// Set the maximum filename length filter.
    #[must_use]
    pub const fn with_max_name_len(mut self, len: Option<u16>) -> Self {
        self.max_name_len = len;
        self
    }

    /// Set the minimum full-path length filter.
    #[must_use]
    pub const fn with_min_path_len(mut self, len: Option<u16>) -> Self {
        self.min_path_len = len;
        self
    }

    /// Set the maximum full-path length filter.
    #[must_use]
    pub const fn with_max_path_len(mut self, len: Option<u16>) -> Self {
        self.max_path_len = len;
        self
    }

    /// Set the minimum allocated (on-disk) size filter.
    #[must_use]
    pub const fn with_min_allocated(mut self, size: Option<u64>) -> Self {
        self.min_allocated = size;
        self
    }

    /// Set the maximum allocated (on-disk) size filter.
    #[must_use]
    pub const fn with_max_allocated(mut self, size: Option<u64>) -> Self {
        self.max_allocated = size;
        self
    }

    /// Set the allowed months filter (1-12).
    #[must_use]
    pub fn with_allowed_months(mut self, months: Vec<u32>) -> Self {
        self.allowed_months = months;
        self
    }

    /// Pre-resolve extension filter strings to `u16` IDs for a specific
    /// drive.  Call this **once per drive** before the hot record loop.
    pub(crate) fn resolve_ext_ids_for_drive(&mut self, drive: &crate::compact::DriveCompactIndex) {
        if self.extensions.is_empty() {
            self.resolved_ext_ids.clear();
            tracing::trace!(drive = %drive.letter, "no extension filter active for drive");
            return;
        }

        self.resolved_ext_ids = drive.resolve_ext_ids(&self.extensions);

        let requested_lower = self
            .extensions
            .iter()
            .map(|ext| ext.to_lowercase())
            .collect::<Vec<_>>();
        let lowercase_only_hits = requested_lower
            .iter()
            .filter(|ext| {
                drive
                    .ext_names
                    .iter()
                    .any(|name| name.as_ref() == ext.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let sample_ext_names = drive
            .ext_names
            .iter()
            .filter(|name| !name.is_empty())
            .take(8)
            .map(AsRef::as_ref)
            .collect::<Vec<_>>();

        tracing::debug!(
            drive = %drive.letter,
            requested_extensions = ?self.extensions,
            requested_lowercase = ?requested_lower,
            resolved_ext_ids = ?self.resolved_ext_ids,
            lowercase_only_hits = ?lowercase_only_hits,
            ext_name_count = drive.ext_names.len(),
            ext_name_sample = ?sample_ext_names,
            "extension filter resolution for drive"
        );
    }

    /// Returns `true` when the active filter set is simple enough to
    /// iterate via the extension inverted index — only `extensions`
    /// plus the cheap per-candidate predicates (`hide_system`,
    /// `hide_ads`) that the fast-path loop in
    /// `collect_global_top_n_numeric` can apply inline without any
    /// secondary data structure.  Any heavier filter (size, date,
    /// attribute, exclude, descendant, bulkiness, name/path length,
    /// `allocated`, `treesize`, `tree_allocated`, month, type) disqualifies
    /// the fast path because it would require per-record work that
    /// defeats the O(K) advantage of the CSR lookup.
    ///
    /// **Historical note (2026-04-19).**  Prior to this session
    /// `hide_system` and `hide_ads` were also in the rejection list,
    /// which meant every `uffs *.<ext> --hide-system --hide-ads`
    /// query — the default bench shape — fell back to an O(N) scan of
    /// every record on every loaded drive.  Measured cost on Drive D
    /// (7 M records): `*.dbt` (11 results) took **216 ms** in the
    /// daemon versus **< 1 ms** on the fast path.  See `Run 9` in
    /// `docs/research/perf-phase2-measurement-plan.md`.  The inline
    /// hide-system/hide-ads checks in the fast-path loop cost ~1 ns
    /// (cached bit) and ~30 ns (name-arena read + memchr) per
    /// candidate respectively, which is negligible compared to the
    /// 7 M-record full scan they replace.
    #[must_use]
    pub const fn is_ext_only(&self) -> bool {
        !self.extensions.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer_us.is_none()
            && self.older_us.is_none()
            && self.newer_created_us.is_none()
            && self.older_created_us.is_none()
            && self.newer_accessed_us.is_none()
            && self.older_accessed_us.is_none()
            && self.attr_require == 0
            && self.attr_exclude == 0
            && self.min_descendants.is_none()
            && self.max_descendants.is_none()
            && self.exclude_lower.is_none()
            && self.path_contains_lower.is_none()
            && self.path_excludes_lower.is_none()
            && self.type_filter.is_none()
            && self.min_bulkiness.is_none()
            && self.max_bulkiness.is_none()
            && self.min_name_len.is_none()
            && self.max_name_len.is_none()
            && self.min_path_len.is_none()
            && self.max_path_len.is_none()
            && self.min_allocated.is_none()
            && self.max_allocated.is_none()
            && self.min_treesize.is_none()
            && self.max_treesize.is_none()
            && self.min_tree_allocated.is_none()
            && self.max_tree_allocated.is_none()
            && self.allowed_months.is_empty()
    }

    /// Check whether a compact record passes all filters.
    ///
    /// Hot-path predicate used during global top-N scans.
    ///
    /// `fold_buf` is a caller-owned reusable buffer for on-the-fly
    /// `CaseFold` folding (avoids per-record heap allocation for exclude
    /// matching).
    #[must_use]
    #[inline]
    pub(crate) fn matches_record(
        &self,
        rec: &CompactRecord,
        names: &[u8],
        fold_buf: &mut Vec<u8>,
        fold: uffs_text::case_fold::CaseFold,
    ) -> bool {
        // `hide_system` excludes only *true* NTFS metafiles.  The cached
        // `name_first_byte` gate inside `is_system_metafile` avoids random
        // access into the names arena (25M records → cache misses) for the
        // ~all records that do not start with `$`; only `$`-prefixed
        // candidates pay the arena lookup + allowlist check.
        if self.hide_system && rec.is_system_metafile(names) {
            return false;
        }
        if self.hide_ads {
            let name = rec.name(names);
            if memchr::memchr(b':', name.as_bytes()).is_some() {
                return false;
            }
        }
        if let Some(min) = self.min_size
            && rec.size < min
        {
            return false;
        }
        if let Some(max) = self.max_size
            && rec.size > max
        {
            return false;
        }
        if let Some(bound) = self.newer_us
            && rec.modified < bound
        {
            return false;
        }
        if let Some(bound) = self.older_us
            && rec.modified >= bound
        {
            return false;
        }
        if let Some(bound) = self.newer_created_us
            && rec.created < bound
        {
            return false;
        }
        if let Some(bound) = self.older_created_us
            && rec.created >= bound
        {
            return false;
        }
        if let Some(bound) = self.newer_accessed_us
            && rec.accessed < bound
        {
            return false;
        }
        if let Some(bound) = self.older_accessed_us
            && rec.accessed >= bound
        {
            return false;
        }
        if self.attr_require != 0 && (rec.flags & self.attr_require) != self.attr_require {
            return false;
        }
        if self.attr_exclude != 0 && (rec.flags & self.attr_exclude) != 0 {
            return false;
        }
        if let Some(want_deleted) = self.deleted {
            let is_deleted = (rec.flags & DELETED_TOMBSTONE_FLAG) != 0;
            if is_deleted != want_deleted {
                return false;
            }
        }
        if let Some(min) = self.min_descendants
            && rec.descendants < min
        {
            return false;
        }
        if let Some(max) = self.max_descendants
            && rec.descendants > max
        {
            return false;
        }
        if !self.resolved_ext_ids.is_empty() {
            // Fast path: compare pre-resolved u16 IDs (O(1) per record).
            if !self.resolved_ext_ids.contains(&rec.extension_id) {
                return false;
            }
        } else if !self.extensions.is_empty() {
            // Fallback for callers that did not call resolve_ext_ids_for_drive.
            // Uses the same dot-gated extraction as `intern_extension` so a
            // dotless record (extension_id = 0 in the compact index) never
            // matches an `--ext foo` filter just because its name happens to
            // equal `foo`.  See `extract_extension_after_dot` for the
            // regression pin covering this behaviour.
            let name = rec.name(names);
            let ext = extract_extension_after_dot(name);
            if ext.is_empty() {
                return false;
            }
            let normalized_ext = lowercase_into(ext, fold_buf);
            if !self
                .extensions
                .iter()
                .any(|allowed| extension_matches_filter(allowed, normalized_ext))
            {
                return false;
            }
        }
        if let Some(excl) = &self.exclude_lower {
            // Zero-alloc via CaseFold: fold the name into a reusable buffer.
            let name = rec.name(names);
            let folded_name = fold.fold_into(name, fold_buf);
            if name_matches(folded_name, excl) {
                return false;
            }
        }
        self.matches_derived(rec, names)
    }

    /// Check derived/computed filters: name length, allocated, tree metrics,
    /// month.
    ///
    /// Split from [`Self::matches_record`] to keep each function under the
    /// `too_many_lines` lint threshold.
    fn matches_derived(&self, rec: &CompactRecord, names: &[u8]) -> bool {
        // ── Name-length filters (chars, not bytes) ─────────────────
        if self.min_name_len.is_some() || self.max_name_len.is_some() {
            let name_len = uffs_mft::len_to_u16(rec.name(names).chars().count());
            if let Some(min) = self.min_name_len
                && name_len < min
            {
                return false;
            }
            if let Some(max) = self.max_name_len
                && name_len > max
            {
                return false;
            }
        }
        // ── Size-on-disk filters ───────────────────────────────────
        if let Some(min) = self.min_allocated
            && rec.allocated < min
        {
            return false;
        }
        if let Some(max) = self.max_allocated
            && rec.allocated > max
        {
            return false;
        }
        // ── Tree metric filters ─────────────────────────────────────
        if let Some(min) = self.min_treesize
            && rec.treesize < min
        {
            return false;
        }
        if let Some(max) = self.max_treesize
            && rec.treesize > max
        {
            return false;
        }
        if let Some(min) = self.min_tree_allocated
            && rec.tree_allocated < min
        {
            return false;
        }
        if let Some(max) = self.max_tree_allocated
            && rec.tree_allocated > max
        {
            return false;
        }
        // ── Bulkiness filters (scan-level, no path needed) ────────
        if self.min_bulkiness.is_some() || self.max_bulkiness.is_some() {
            let (logical, allocated) = if rec.is_directory() {
                (rec.treesize, rec.tree_allocated)
            } else {
                (rec.size, rec.allocated)
            };
            let bulk = allocated
                .saturating_mul(crate::search::derived::BULKINESS_SCALE)
                .checked_div(logical)
                .unwrap_or(0);
            if let Some(min) = self.min_bulkiness
                && bulk < min
            {
                return false;
            }
            if let Some(max) = self.max_bulkiness
                && bulk > max
            {
                return false;
            }
        }
        // ── Path-length filters (precomputed on CompactRecord) ────
        if let Some(min) = self.min_path_len
            && rec.path_len < min
        {
            return false;
        }
        if let Some(max) = self.max_path_len
            && rec.path_len > max
        {
            return false;
        }
        // ── Month-of-year filter ───────────────────────────────────
        if !self.allowed_months.is_empty() {
            let month = month_from_unix_micros(rec.modified);
            if !self.allowed_months.contains(&month) {
                return false;
            }
        }
        // ── WI-4.4 malformed-name filter ───────────────────────────
        // Evaluate against the LOSSLESS name bytes, not the lossy `name()`
        // &str view: a lossy view is always valid UTF-8, so checking it would
        // make this filter match nothing. `from_utf8` is a fast validation and
        // only runs when the filter is active.
        if let Some(want) = self.malformed {
            let is_malformed = core::str::from_utf8(rec.name_bytes(names)).is_err();
            if is_malformed != want {
                return false;
            }
        }
        true
    }

    /// Returns `true` if all filters are at their default (no-op) values.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.hide_system
            && !self.hide_ads
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer_us.is_none()
            && self.older_us.is_none()
            && self.newer_created_us.is_none()
            && self.older_created_us.is_none()
            && self.newer_accessed_us.is_none()
            && self.older_accessed_us.is_none()
            && self.attr_require == 0
            && self.attr_exclude == 0
            && self.min_descendants.is_none()
            && self.max_descendants.is_none()
            && self.extensions.is_empty()
            && self.exclude_lower.is_none()
            && self.path_contains_lower.is_none()
            && self.path_excludes_lower.is_none()
            && self.type_filter.is_none()
            && self.min_bulkiness.is_none()
            && self.max_bulkiness.is_none()
            && self.min_name_len.is_none()
            && self.max_name_len.is_none()
            && self.min_path_len.is_none()
            && self.max_path_len.is_none()
            && self.min_allocated.is_none()
            && self.max_allocated.is_none()
            && self.min_treesize.is_none()
            && self.max_treesize.is_none()
            && self.min_tree_allocated.is_none()
            && self.max_tree_allocated.is_none()
            && self.allowed_months.is_empty()
            // WI-4.4: a malformed-name toggle (`--malformed` / `--well-formed`)
            // is a real filter — omitting it here makes the numeric match-all
            // gate (`has_filters = !is_empty()`) skip `matches_record`, so the
            // filter silently no-ops on `uffs "*" --malformed`.
            && self.malformed.is_none()
            // A deleted-tombstone toggle is a real filter — same match-all-gate
            // reasoning as `malformed` above.
            && self.deleted.is_none()
    }
}

use crate::diff::DELETED_TOMBSTONE_FLAG;

#[cfg(test)]
mod tests;
