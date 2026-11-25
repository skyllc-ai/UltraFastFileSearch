// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Filter application logic: retain/reject rows against `SearchFilters`.

use super::super::backend::DisplayRow;
use super::super::derived::is_system_name;
use super::super::tree::name_matches;
use super::{
    SearchFilters, extension_matches_filter, extract_extension_after_dot, lowercase_into,
    month_from_unix_micros,
};

impl SearchFilters {
    /// Returns `true` if any filter requires a resolved `DisplayRow`
    /// (full path, semantic type).
    #[must_use]
    pub const fn needs_display_row_filter(&self) -> bool {
        self.path_contains_lower.is_some()
            || self.path_excludes_lower.is_some()
            || self.type_filter.is_some()
    }
}

/// Returns `true` if `row` passes every non-empty filter in `filters`.
///
/// Factored out of the `apply_search_filters` retain predicate so
/// callers that want to filter DURING collection (e.g. the path-sort
/// tree walk in `collect_global_top_n`) can reuse the same logic
/// without a post-collection pass.  `fold_buf` is caller-owned for
/// reuse across successive rows; pass a freshly `Vec::with_capacity`-
/// allocated buffer once, reuse it per row.
#[must_use]
pub(crate) fn row_passes_filters(
    row: &DisplayRow,
    filters: &SearchFilters,
    fold: &uffs_text::case_fold::CaseFold,
    fold_buf: &mut Vec<u8>,
) -> bool {
    if filters.is_empty() {
        return true;
    }
    if filters.hide_system && is_system_name(row.name()) {
        return false;
    }
    if filters.hide_ads && row.name().contains(':') {
        return false;
    }
    // WI-4.4: malformed-name toggle for the DisplayRow paths (Path/PathOnly
    // tree-walk and the regex/trigram post-filter pass). The `malformed` bit
    // is precomputed against the lossless name bytes in `make_display_row`, so
    // we read the carrier here rather than re-deriving from the lossy `path`.
    if let Some(want) = filters.malformed
        && row.malformed != want
    {
        return false;
    }
    if let Some(min) = filters.min_size
        && row.size < min
    {
        return false;
    }
    if let Some(max) = filters.max_size
        && row.size > max
    {
        return false;
    }
    if let Some(bound) = filters.newer_us
        && row.modified < bound
    {
        return false;
    }
    if let Some(bound) = filters.older_us
        && row.modified >= bound
    {
        return false;
    }
    if let Some(bound) = filters.newer_created_us
        && row.created < bound
    {
        return false;
    }
    if let Some(bound) = filters.older_created_us
        && row.created >= bound
    {
        return false;
    }
    if let Some(bound) = filters.newer_accessed_us
        && row.accessed < bound
    {
        return false;
    }
    if let Some(bound) = filters.older_accessed_us
        && row.accessed >= bound
    {
        return false;
    }
    if filters.attr_require != 0 && (row.flags & filters.attr_require) != filters.attr_require {
        return false;
    }
    if filters.attr_exclude != 0 && (row.flags & filters.attr_exclude) != 0 {
        return false;
    }
    if let Some(min) = filters.min_descendants
        && row.descendants < min
    {
        return false;
    }
    if let Some(max) = filters.max_descendants
        && row.descendants > max
    {
        return false;
    }
    if !filters.extensions.is_empty() {
        // Same dot-gated extraction as the compact-path fallback in
        // `SearchFilters::matches_record` — a dotless name (e.g. the
        // directory `dbt`) must not match `--ext dbt`.
        let ext = extract_extension_after_dot(row.name());
        if ext.is_empty() {
            return false;
        }
        let normalized_ext = lowercase_into(ext, fold_buf);
        if !filters
            .extensions
            .iter()
            .any(|allowed| extension_matches_filter(allowed, normalized_ext))
        {
            return false;
        }
    }
    if let Some(excl) = &filters.exclude_lower {
        let folded_name = fold.fold_into(row.name(), fold_buf);
        if name_matches(folded_name, excl) {
            return false;
        }
    }
    apply_derived_filters(row, filters)
}

/// Apply extended search filters to display rows (in-place) — retains
/// only the rows that pass every non-empty filter in `filters`.
///
/// Public so a downstream crate holding its own richer row data (more
/// fields than [`DisplayRow`] carries) can still reuse this entire
/// filter engine unchanged: build a [`DisplayRow`] via its public
/// [`DisplayRow::new`] constructor from whatever subset of fields
/// overlaps, run it through this function to get every existing filter
/// axis applied verbatim, then apply any additional filters the extra
/// fields need as a separate, small pass of its own. This avoids
/// forking or reimplementing this engine for that case — see
/// `PRIVATE_RICH_INDEX_DESIGN.md` in the `uffs-products` repo for the
/// concrete motivating use case.
pub fn apply_search_filters(rows: &mut Vec<DisplayRow>, filters: &SearchFilters) {
    if filters.is_empty() {
        return;
    }
    let fold = uffs_text::case_fold::CaseFold::default_table();
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);
    rows.retain(|row| row_passes_filters(row, filters, &fold, &mut fold_buf));
}

/// Derived / post-filter checks for `apply_search_filters`.
///
/// Extracted to keep the retain closure under the `cognitive_complexity`
/// and `too_many_lines` lint thresholds — a 105-line helper is clearer
/// than inlining into the already-complex `retain` closure.
#[expect(
    clippy::single_call_fn,
    reason = "factored out for cognitive_complexity + too_many_lines"
)]
fn apply_derived_filters(row: &DisplayRow, filters: &SearchFilters) -> bool {
    // ── Name-length filters ────────────────────────────────────
    if filters.min_name_len.is_some() || filters.max_name_len.is_some() {
        let name_len = uffs_mft::len_to_u16(row.name().chars().count());
        if let Some(min) = filters.min_name_len
            && name_len < min
        {
            return false;
        }
        if let Some(max) = filters.max_name_len
            && name_len > max
        {
            return false;
        }
    }
    // ── Path-length filters ────────────────────────────────────
    // Note: path_len is measured in Unicode characters, consistent with
    // the precomputed `CompactRecord::path_len` used at scan level.
    if filters.min_path_len.is_some() || filters.max_path_len.is_some() {
        let path_len = uffs_mft::len_to_u16(row.path.chars().count());
        if let Some(min) = filters.min_path_len
            && path_len < min
        {
            return false;
        }
        if let Some(max) = filters.max_path_len
            && path_len > max
        {
            return false;
        }
    }
    // ── Directory-path pattern filter ───────────────────────────
    if let Some(pat) = &filters.path_contains_lower {
        let dir = row.path_dir();
        let dir_lower = dir.to_ascii_lowercase();
        if !name_matches(&dir_lower, pat) {
            return false;
        }
    }
    // ── Directory-path exclude filter (drop if dir matches ANY) ──
    if let Some(excludes) = &filters.path_excludes_lower {
        let dir_lower = row.path_dir().to_ascii_lowercase();
        if excludes.iter().any(|pat| name_matches(&dir_lower, pat)) {
            return false;
        }
    }
    // ── Type/category filter ────────────────────────────────────
    if let Some(wanted) = &filters.type_filter
        && crate::search::derived::semantic_type_for_row(row) != wanted.as_str()
    {
        return false;
    }
    // ── Bulkiness filters ───────────────────────────────────────
    if filters.min_bulkiness.is_some() || filters.max_bulkiness.is_some() {
        let bulk = crate::search::derived::bulkiness_for_row(row);
        if let Some(min) = filters.min_bulkiness
            && bulk < min
        {
            return false;
        }
        if let Some(max) = filters.max_bulkiness
            && bulk > max
        {
            return false;
        }
    }
    // ── Size-on-disk filters ───────────────────────────────────
    if let Some(min) = filters.min_allocated
        && row.allocated < min
    {
        return false;
    }
    if let Some(max) = filters.max_allocated
        && row.allocated > max
    {
        return false;
    }
    // ── Tree metric filters ─────────────────────────────────────
    if let Some(min) = filters.min_treesize
        && row.treesize < min
    {
        return false;
    }
    if let Some(max) = filters.max_treesize
        && row.treesize > max
    {
        return false;
    }
    if let Some(min) = filters.min_tree_allocated
        && row.tree_allocated < min
    {
        return false;
    }
    if let Some(max) = filters.max_tree_allocated
        && row.tree_allocated > max
    {
        return false;
    }
    // ── Month-of-year filter ───────────────────────────────────
    if !filters.allowed_months.is_empty() {
        let month = month_from_unix_micros(row.modified);
        if !filters.allowed_months.contains(&month) {
            return false;
        }
    }
    true
}

/// Fold an exclude glob the way each name is folded before it is matched.
///
/// Both use `$UpCase`, the NTFS case table, so they always meet in one case.
/// Every writer of [`super::SearchFilters::exclude_lower`] goes through
/// here: an ASCII-lowercased pattern never matched an upper-folded name, so
/// `--exclude` dropped nothing that had a letter in it.
#[must_use]
pub fn fold_exclude_pattern(pattern: &str) -> String {
    let mut buf = Vec::with_capacity(pattern.len());
    uffs_text::case_fold::CaseFold::default_table()
        .fold_into(pattern, &mut buf)
        .to_owned()
}
