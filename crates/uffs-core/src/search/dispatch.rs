// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search dispatch helpers: pattern-rewrite safety nets and per-branch
//! fan-out functions.
//!
//! Extracted from `backend.rs` to keep that file under the 800-LOC
//! file-size policy.  All symbols here are used only by
//! `MultiDriveBackend::search` and the free `search_index` function in
//! `backend.rs`, so visibility is scoped to `pub(super)`.
//!
//! Two categories:
//!
//! 1. **Pattern-rewrite safety nets** ([`apply_dispatch_safety_nets`]) — mirror
//!    the parse-time rewrites in
//!    `uffs_client::protocol::cli_args::into_search_params`.  Direct JSON-RPC
//!    `search` callers and library users that build `SearchParams` manually
//!    skip the parse-time layer; these catch the two rewrites at dispatch time
//!    so every entry point lands on the same hot paths.  `is_pure_ext_glob` is
//!    an internal helper consumed by `apply_dispatch_safety_nets`; the
//!    drive-prefix split is the shared `uffs_mft::platform::split_drive_prefix`
//!    (same primitive the CLI parse layer uses, so both agree).
//!
//! 2. **Per-branch dispatchers** ([`dispatch_match_all`], [`dispatch_regex`],
//!    [`dispatch_trigram_or_tree`]) — the three leaf dispatch paths +
//!    [`pick_mode_label`] for tracing.

use alloc::borrow::Cow;

use rayon::prelude::*;

use super::backend::{DisplayRow, FilterMode, PhaseTimings, SortSpec};
use super::filters::SearchFilters;
use super::sorting::sort_rows;
use crate::compact::DriveCompactIndex;
use crate::search::field::FieldId;

/// Build the search needle from `pattern`, applying case-folding when
/// case-insensitive matching is requested.
///
/// Returns [`Cow::Borrowed`] for the case-sensitive path (zero
/// allocation — the needle reuses the caller's `&str`) and
/// [`Cow::Owned`] for the case-insensitive path (one allocation for
/// the folded result).
///
/// Replaces a previous duplicated `if … pattern.to_owned() … else …
/// .to_owned()` pattern in both [`super::backend::MultiDriveBackend::search`]
/// and [`super::backend::search_index`]; the deduplication is the
/// Phase 6e (`Cow<'_, str>` expansion) deliverable per the phase-6
/// ownership/borrowing/allocation plan §3.5.  Downstream call sites
/// already accept `&str` (via [`Cow`]'s `Deref<Target = str>`), so
/// this is a behavior-preserving refactor.
pub(super) fn fold_needle(
    case_sensitive: bool,
    pattern: &str,
    fold: uffs_text::case_fold::CaseFold,
) -> Cow<'_, str> {
    if case_sensitive {
        Cow::Borrowed(pattern)
    } else {
        let mut buf = Vec::with_capacity(pattern.len());
        Cow::Owned(fold.fold_into(pattern, &mut buf).to_owned())
    }
}

// ─── Pattern-rewrite safety nets ───────────────────────────────────────

/// Return `true` when `s` is exactly `*.<alnum+underscore>+` — a pure
/// extension glob that can be safely promoted to an `ExtensionIndex` lookup.
///
/// Used by the search-dispatch safety net: if a caller (e.g. direct
/// JSON-RPC `search` method) supplies `pattern="*.dll"` without setting
/// the `extensions` filter, we can still route through
/// `numeric_top_n::ext_fast_path` by rewriting to `pattern="*"` +
/// `extensions=["dll"]`.
///
/// Mirror of `uffs_client::protocol::cli_args::is_pure_ext_glob` — keep
/// the two in sync.  See that function's doc for the acceptance matrix.
fn is_pure_ext_glob(pattern: &str) -> bool {
    pattern.strip_prefix("*.").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// Extract a pure trailing-extension alternation from a regex pattern.
///
/// Returns `Some(exts)` when `pattern` matches a narrow shape that is
/// semantically equivalent to `.*\.(e1|e2|...)$` — i.e. the extensions
/// can be routed through the `ExtensionIndex` fast path without
/// changing the result set.  Returns `None` for any more complex shape
/// so the regex stays on the full-scan path.
///
/// # Accepted shapes
///
/// All forms **require a trailing `$` anchor**: without it, `\.jpg` (for
/// example) matches `.jpg` anywhere in the name, which the ext-index
/// cannot replicate (it matches by the `extension_id` on the trailing
/// dot-segment only).  Requiring `$` keeps the rewrite semantically
/// lossless.
///
/// - `>\.(a|b|c)$`                  — alternation, anchored
/// - `>.*\.(a|b|c)$`                — optional `.*` prefix
/// - `>^\.(a|b|c)$`                 — redundant `^` prefix accepted
/// - `>^.*\.(a|b|c)$`               — both anchors
/// - `>(?i).*\.(a|b|c)$`            — optional `(?i)` case-insensitive flag
/// - `>.*\.a$`                      — single extension, no alternation
/// - `>\.a$`                        — single extension, no `.*`
///
/// Each extension must match `is_pure_ext_glob`'s acceptance: non-empty,
/// pure ASCII alphanumeric + underscore.  The returned vec is
/// lower-cased so callers can push straight into
/// `SearchFilters::extensions`.
///
/// # Rejected shapes (stay on the regex full-scan path)
///
/// - `>.*\.jpg`                     — missing `$` anchor (would widen match)
/// - `>.*\.(tar\.gz|zip)$`          — dot inside alternation (multi-segment)
/// - `>.*\.(jp.?)$`                 — wildcard inside alternation
/// - `>.*\.[ch]$`                   — character class not alnum
/// - `>C:\\Users\\.*\.(a|b)$`       — literal prefix other than `.*` / `^` /
///   `(?i)` (route through regex scan so the path-anchor constraint is
///   honoured)
///
/// Mirror of
/// `uffs_client::protocol::cli_args_helpers::extract_extensions_from_regex`
/// — keep the two in sync.
fn extract_extensions_from_regex(pattern: &str) -> Option<Vec<String>> {
    // Pattern must start with the `>` regex sentinel.  Downstream
    // callers pass the raw pattern including the `>`.
    let mut body = pattern.strip_prefix('>')?;
    if body.is_empty() {
        return None;
    }

    // Strip optional inline case-insensitive flag group.
    body = body.strip_prefix("(?i)").unwrap_or(body);
    // Strip optional start-of-string anchor.
    body = body.strip_prefix('^').unwrap_or(body);
    // Strip optional `.*` prefix (match-any-prefix).
    body = body.strip_prefix(".*").unwrap_or(body);
    // Must now start with a literal dot `\.`.
    body = body.strip_prefix("\\.")?;
    // **Required** trailing `$` — see doc for semantic rationale.
    body = body.strip_suffix('$')?;

    // `body` is now either `"ext"` or `"(e1|e2|...)"`.
    let exts: Vec<String> = body
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .map_or_else(
            || vec![body.to_ascii_lowercase()],
            |group| group.split('|').map(str::to_ascii_lowercase).collect(),
        );

    // Each extension must be pure alnum + underscore, matching
    // `is_pure_ext_glob`'s acceptance.  Reject any pattern with regex
    // metacharacters, nested groups, or multi-segment extensions.
    exts.iter()
        .all(|ext| {
            !ext.is_empty()
                && ext
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
        .then_some(exts)
}

/// Apply dispatch-time pattern-rewrite safety nets, in canonical order.
///
/// Mirrors the parse-time rewrites in
/// `uffs_client::protocol::cli_args::into_search_params`.  Direct
/// JSON-RPC `search` callers or library users that build `SearchParams`
/// manually skip the parse-time layer; this function catches the three
/// rewrites at dispatch time so both entry points land on the same hot
/// paths.
///
/// # Rewrites (applied in order)
///
/// 1. `<letter>:<rest>` → `drives_filter = [<letter>]`, `pattern = <rest>`.
///    Only fires when the caller's `drives_filter_empty` and not `match_path`.
///    Delegates to `uffs_mft::platform::split_drive_prefix`, so the bare
///    (`C:`), drive-root (`C:\`), and path-anchored (`C:\Users\*.pdf`) forms
///    are all handled — the drive-relative remainder (`*` for the bare forms)
///    becomes the new pattern and downstream classification routes it to
///    match-all / name / tree as appropriate.  The promoted letter is pushed
///    into `drive_buf` (which the caller uses as backing storage for a
///    `&[DriveLetter]` slice that lives for the rest of the dispatch).
///
/// 2. `*.<ext>` → `pattern = "*"`, `extensions += [<ext_lower>]`. Only fires
///    when not `match_path`, not `case_sensitive`, and
///    `search_filters.extensions` is empty.  Uses `is_pure_ext_glob` to reject
///    multi-segment / wildcard / path-anchored shapes.
///
/// 3. `>.*\.(e1|e2|...)$` → `pattern = "*"`, `extensions += [e1, e2, ...]`.
///    Only fires when not `match_path`, not `case_sensitive`, and
///    `search_filters.extensions` is empty.  Uses
///    `extract_extensions_from_regex` which **requires** a trailing `$` anchor
///    so the rewrite is semantically lossless.  Saves ~200 ms on
///    `>.*\.(jpg|png|heic)$` over C: (3.5 M records) — 298 ms full-scan regex
///    drops to ~95 ms ext-index lookup, matching the equivalent `--ext
///    jpg,png,heic` glob path.
///
/// Rewrites #1 and #2 **compose**: `C:*.dll` is first stripped to
/// `*.dll` by rewrite #1, and rewrite #2 then promotes it to
/// `pattern="*"` + `extensions=["dll"]`, so the caller ends up with
/// `drive=C` + `ext=dll` + match-all — exactly the shape the
/// `numeric_top_n::ext_fast_path` expects.  Rewrite #3 does not
/// compose with rewrite #1 because regex patterns start with `>` (not
/// a drive letter), so `split_drive_prefix` rejects them; path
/// anchors inside the regex (`>C:\\Users\\.*\.dll$`) stay on the regex
/// scan path by design — the ext-index would widen the match to all
/// `.dll` files drive-wide.
pub(super) fn apply_dispatch_safety_nets(
    pattern: &mut &str,
    match_path: bool,
    case_sensitive: bool,
    drives_filter_empty: bool,
    search_filters: &mut SearchFilters,
    drive_buf: &mut Vec<uffs_mft::platform::DriveLetter>,
) {
    if drives_filter_empty
        && !match_path
        && let Some((letter, rest)) = uffs_mft::platform::split_drive_prefix(pattern)
    {
        tracing::debug!(
            original_pattern = *pattern,
            promoted_drive = %letter,
            promoted_rest = rest,
            "promoted <letter>:<rest> to drive filter (dispatch-time safety net)"
        );
        *pattern = rest;
        drive_buf.push(letter);
    }

    if !match_path
        && !case_sensitive
        && search_filters.extensions.is_empty()
        && is_pure_ext_glob(pattern)
    {
        let ext_lower = pattern
            .strip_prefix("*.")
            .unwrap_or_default()
            .to_ascii_lowercase();
        tracing::debug!(
            original_pattern = *pattern,
            promoted_ext = %ext_lower,
            "promoted *.<ext> to ext filter (dispatch-time safety net)"
        );
        search_filters.extensions.push(ext_lower);
        *pattern = "*";
    }

    // Rewrite #3: regex alternation → ext-index.  Independent of
    // rewrite #2 because that one already returned if it fired
    // (overwriting `pattern` to `"*"` which fails the `>` prefix
    // check below).  See `extract_extensions_from_regex` docs for the
    // acceptance matrix and the Phase 4 roadmap in
    // `docs/research/cross-tool-benchmark-analysis.md` §7.
    if !match_path
        && !case_sensitive
        && search_filters.extensions.is_empty()
        && let Some(exts) = extract_extensions_from_regex(pattern)
    {
        tracing::debug!(
            original_pattern = *pattern,
            promoted_exts = ?exts,
            "promoted regex alternation to ext filter (dispatch-time safety net)"
        );
        search_filters.extensions.extend(exts);
        *pattern = "*";
    }
}

// ─── Per-branch dispatchers ────────────────────────────────────────────

/// Dispatch the `pattern == "*"` fast path: global top-N from the ext
/// and size indices, optionally post-filtered by display-row predicates.
///
/// Returns `(rows, phase_timings)`.  `phase_timings` is `Some` when the
/// numeric-sort branch of `collect_global_top_n` ran (i.e. any sort column
/// other than `Path` / `PathOnly`) — that branch calls
/// `collect_global_top_n_numeric`, which populates the scan / sort /
/// `path_resolve` sub-phase breakdown.  The `PathOnly` tree-walk branch
/// produces `None`; callers treat that as "no sub-breakdown available".
pub(super) fn dispatch_match_all(
    active_drives: &[&DriveCompactIndex],
    limit: usize,
    sort_column: FieldId,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> (Vec<DisplayRow>, Option<PhaseTimings>) {
    let t_top_n = std::time::Instant::now();
    let (mut rows, phase_timings) = super::query::collect_global_top_n(
        active_drives,
        limit,
        sort_column,
        sort_desc,
        filter_mode,
        search_filters,
    );
    let top_n_ms = t_top_n.elapsed().as_millis();
    tracing::debug!(rows = rows.len(), top_n_ms, "[2] collect_global_top_n done");
    if search_filters.needs_display_row_filter() {
        let t_post = std::time::Instant::now();
        super::filters::apply_search_filters(&mut rows, search_filters);
        tracing::debug!(
            rows_after = rows.len(),
            post_filter_ms = t_post.elapsed().as_millis(),
            "[3] post-filter done"
        );
    }
    (rows, phase_timings)
}

/// How the rows of one search are ordered and cut: the sort key, its
/// direction, the further sort tiers, and the row limit.
pub(super) struct RowOrder<'a> {
    /// Primary sort column.
    pub(super) column: FieldId,
    /// Primary sort descending.
    pub(super) desc: bool,
    /// Further sort tiers after the primary one.
    pub(super) extra_tiers: &'a [SortSpec],
    /// Maximum number of rows kept.
    pub(super) limit: usize,
}

/// Dispatch the regex branch (`>pattern`): fan the compiled regex (see
/// [`super::search_error::compile_search_regex`]) out across drives, then
/// filter + sort + truncate.
pub(super) fn dispatch_regex(
    active_drives: &[&DriveCompactIndex],
    compiled_re: &regex::Regex,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    order: &RowOrder<'_>,
) -> Vec<DisplayRow> {
    let drive_results: Vec<Vec<DisplayRow>> = active_drives
        .par_iter()
        .map(|drive| {
            if search_filters.is_cancelled() {
                return Vec::new();
            }
            super::query::search_compact_drive_regex(
                drive,
                compiled_re,
                order.limit,
                search_filters,
            )
        })
        .collect();
    let mut rows: Vec<DisplayRow> = drive_results.into_iter().flatten().collect();
    super::filters::apply_filter(&mut rows, filter_mode);
    super::filters::apply_search_filters(&mut rows, search_filters);
    sort_rows(&mut rows, order.column, order.desc, order.extra_tiers);
    rows.truncate(order.limit);
    rows
}

/// Dispatch the default branch: tree-walk for path patterns, trigram-
/// accelerated prefix scan for prefix patterns (`win*`), trigram for the
/// remaining name patterns — all fanned across drives then filtered +
/// sorted + truncated.
#[expect(clippy::too_many_arguments, reason = "single call site, flat args")]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the bools (is_path / is_prefix / case_sensitive / whole_word / match_path) are orthogonal runtime switches, each controlling a distinct aspect of trigram vs tree matching; bundling them into an enum would lose that orthogonality"
)]
pub(super) fn dispatch_trigram_or_tree(
    active_drives: &[&DriveCompactIndex],
    needle: &str,
    is_path: bool,
    is_prefix: bool,
    case_sensitive: bool,
    whole_word: bool,
    match_path: bool,
    limit: usize,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    sort_column: FieldId,
    sort_desc: bool,
    extra_sort_tiers: &[SortSpec],
) -> Vec<DisplayRow> {
    let drive_results: Vec<Vec<DisplayRow>> = active_drives
        .par_iter()
        .map(|drive| {
            if search_filters.is_cancelled() {
                return Vec::new();
            }
            if is_path {
                super::query::search_compact_drive_tree(drive, needle, limit, search_filters)
            } else if is_prefix {
                // `is_prefix` was validated upstream via `is_prefix_pattern`;
                // re-extract the prefix and fall back to the generic scan if
                // the (should-be-impossible) re-validation ever fails.
                super::tree::is_prefix_pattern(needle).map_or_else(
                    || {
                        super::query::search_compact_drive(
                            drive,
                            needle,
                            limit,
                            case_sensitive,
                            whole_word,
                            match_path,
                            search_filters,
                        )
                    },
                    |prefix| {
                        super::query::search_compact_drive_prefix(
                            drive,
                            prefix,
                            limit,
                            case_sensitive,
                            search_filters,
                        )
                    },
                )
            } else {
                super::query::search_compact_drive(
                    drive,
                    needle,
                    limit,
                    case_sensitive,
                    whole_word,
                    match_path,
                    search_filters,
                )
            }
        })
        .collect();
    let mut rows: Vec<DisplayRow> = drive_results.into_iter().flatten().collect();
    super::filters::apply_filter(&mut rows, filter_mode);
    super::filters::apply_search_filters(&mut rows, search_filters);
    sort_rows(&mut rows, sort_column, sort_desc, extra_sort_tiers);
    rows.truncate(limit);
    rows
}

/// Drop the rows of a scan that was cancelled mid-way.
///
/// A cancelled scan stopped early on some drive: whatever it had
/// collected is an arbitrary subset, and a subset presented as the
/// answer is worse than no answer.  The caller that cancelled already
/// knows why, so nothing is returned.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from search_index to keep it under the cognitive-complexity budget"
)]
pub(super) fn discard_if_cancelled(
    rows: Vec<DisplayRow>,
    search_filters: &SearchFilters,
    pattern: &str,
) -> Vec<DisplayRow> {
    if !search_filters.is_cancelled() {
        return rows;
    }
    tracing::debug!(
        pattern,
        partial_rows = rows.len(),
        "search cancelled by caller; partial result discarded"
    );
    Vec::new()
}

/// Pick the `cache_profile` `mode` tracing label for the chosen
/// dispatch branch.  Pure function — no side effects.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the four bools are independent runtime dispatch flags; bundling into an enum would lose orthogonality and not improve clarity"
)]
pub(super) const fn pick_mode_label(
    is_match_all: bool,
    is_regex: bool,
    is_path: bool,
    is_prefix: bool,
) -> &'static str {
    if is_match_all {
        "match-all"
    } else if is_regex {
        "regex"
    } else if is_path {
        "tree"
    } else if is_prefix {
        "prefix"
    } else {
        "trigram"
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::filters::SearchFilters;

    // ── extract_extensions_from_regex ──────────────────────────────

    #[test]
    fn regex_alternation_anchored() {
        assert_eq!(
            extract_extensions_from_regex(">.*\\.(jpg|png|heic)$"),
            Some(vec!["jpg".into(), "png".into(), "heic".into()]),
        );
    }

    #[test]
    fn regex_single_ext_anchored() {
        assert_eq!(
            extract_extensions_from_regex(">.*\\.dll$"),
            Some(vec!["dll".into()]),
        );
    }

    #[test]
    fn regex_alternation_no_star_prefix() {
        assert_eq!(
            extract_extensions_from_regex(">\\.(a|b|c)$"),
            Some(vec!["a".into(), "b".into(), "c".into()]),
        );
    }

    #[test]
    fn regex_start_anchor_accepted() {
        assert_eq!(
            extract_extensions_from_regex(">^.*\\.(rs)$"),
            Some(vec!["rs".into()]),
        );
        assert_eq!(
            extract_extensions_from_regex(">^\\.rs$"),
            Some(vec!["rs".into()]),
        );
    }

    #[test]
    fn regex_case_insensitive_flag_accepted() {
        assert_eq!(
            extract_extensions_from_regex(">(?i).*\\.(JPG|PNG)$"),
            Some(vec!["jpg".into(), "png".into()]),
        );
    }

    #[test]
    fn regex_uppercase_extensions_lowercased() {
        assert_eq!(
            extract_extensions_from_regex(">\\.(Dll|EXE)$"),
            Some(vec!["dll".into(), "exe".into()]),
        );
    }

    #[test]
    fn regex_missing_dollar_anchor_rejected() {
        // Without `$` the regex matches `.jpg` anywhere in the name
        // (e.g. `foo.jpg.txt`), which the ext-index cannot replicate.
        assert_eq!(extract_extensions_from_regex(">.*\\.jpg"), None);
        assert_eq!(extract_extensions_from_regex(">.*\\.(jpg|png)"), None);
        assert_eq!(extract_extensions_from_regex(">\\.rs"), None);
    }

    #[test]
    fn regex_multisegment_extension_rejected() {
        // `tar.gz` contains a literal dot inside the alternation — this
        // is NOT a single NTFS extension (the ext-index would only
        // match `gz`), so the full regex path stays.
        assert_eq!(extract_extensions_from_regex(">.*\\.(tar\\.gz|zip)$"), None,);
    }

    #[test]
    fn regex_wildcard_inside_alternation_rejected() {
        assert_eq!(extract_extensions_from_regex(">.*\\.(jp.?)$"), None);
        assert_eq!(extract_extensions_from_regex(">.*\\.(j.g|png)$"), None);
    }

    #[test]
    fn regex_character_class_rejected() {
        assert_eq!(extract_extensions_from_regex(">.*\\.[ch]$"), None);
    }

    #[test]
    fn regex_literal_prefix_rejected() {
        // A path-anchored regex like `>C:\Users\...\.dll$` must stay on
        // the regex scan path so the path-anchor constraint is honoured.
        // The ext-index rewrite would widen to all `.dll` files.
        assert_eq!(
            extract_extensions_from_regex(">C:\\\\Users\\\\.*\\.(jpg|png)$"),
            None,
        );
        assert_eq!(extract_extensions_from_regex(">foo.*\\.dll$"), None);
    }

    #[test]
    fn regex_empty_body_rejected() {
        assert_eq!(extract_extensions_from_regex(">"), None);
    }

    #[test]
    fn regex_without_leading_sentinel_rejected() {
        // No `>` prefix — this is not a regex pattern in UFFS parlance.
        assert_eq!(extract_extensions_from_regex(".*\\.jpg$"), None);
        assert_eq!(extract_extensions_from_regex("\\.dll$"), None);
    }

    #[test]
    fn regex_empty_extension_in_alternation_rejected() {
        // Empty extension via trailing `|` or `||` means "any file
        // with a trailing dot" — reject to avoid surprising matches.
        assert_eq!(extract_extensions_from_regex(">.*\\.(jpg|)$"), None);
        assert_eq!(extract_extensions_from_regex(">.*\\.(||)$"), None);
    }

    // ── apply_dispatch_safety_nets (regex branch) ──────────────────

    fn run_safety_nets<'a>(
        pattern: &'a str,
        filters: &mut SearchFilters,
    ) -> (&'a str, Vec<uffs_mft::platform::DriveLetter>) {
        let mut pat: &str = pattern;
        let mut drive_buf: Vec<uffs_mft::platform::DriveLetter> = Vec::new();
        apply_dispatch_safety_nets(
            &mut pat,
            false, // match_path
            false, // case_sensitive
            true,  // drives_filter_empty
            filters,
            &mut drive_buf,
        );
        (pat, drive_buf)
    }

    #[test]
    fn safety_net_promotes_regex_alternation() {
        let mut filters = SearchFilters::default();
        let (pat, _) = run_safety_nets(">.*\\.(jpg|png|heic)$", &mut filters);
        assert_eq!(pat, "*", "pattern must be rewritten to match-all");
        assert_eq!(
            filters.extensions,
            vec!["jpg".to_owned(), "png".to_owned(), "heic".to_owned(),],
            "extensions must be extracted from the alternation"
        );
    }

    #[test]
    fn safety_net_promotes_regex_single_ext() {
        let mut filters = SearchFilters::default();
        let (pat, _) = run_safety_nets(">.*\\.rs$", &mut filters);
        assert_eq!(pat, "*");
        assert_eq!(filters.extensions, vec!["rs".to_owned()]);
    }

    #[test]
    fn safety_net_leaves_regex_without_dollar_alone() {
        let mut filters = SearchFilters::default();
        let (pat, _) = run_safety_nets(">.*\\.jpg", &mut filters);
        assert_eq!(pat, ">.*\\.jpg", "no $ anchor — pattern must stay");
        assert_eq!(filters.extensions, Vec::<String>::new());
    }

    #[test]
    fn safety_net_leaves_regex_with_wildcard_alone() {
        let mut filters = SearchFilters::default();
        let (pat, _) = run_safety_nets(">.*\\.(jp.?|png)$", &mut filters);
        assert_eq!(pat, ">.*\\.(jp.?|png)$");
        assert_eq!(filters.extensions, Vec::<String>::new());
    }

    #[test]
    fn safety_net_does_not_clobber_explicit_extensions() {
        let mut filters = SearchFilters {
            extensions: vec!["exe".to_owned()],
            ..Default::default()
        };
        let (pat, _) = run_safety_nets(">.*\\.dll$", &mut filters);
        assert_eq!(
            pat, ">.*\\.dll$",
            "existing --ext filter must block the rewrite"
        );
        assert_eq!(
            filters.extensions,
            vec!["exe".to_owned()],
            "explicit --ext filter must stay untouched"
        );
    }

    // ── apply_dispatch_safety_nets (drive-prefix branch) ───────────

    #[test]
    fn safety_net_bare_drive_promotes_to_match_all() {
        let mut filters = SearchFilters::default();
        let (pat, drives) = run_safety_nets("C:", &mut filters);
        assert_eq!(pat, "*", "bare `C:` becomes match-all on drive C");
        assert_eq!(drives, vec![uffs_mft::platform::DriveLetter::C]);
    }

    #[test]
    fn safety_net_drive_root_promotes_to_match_all() {
        for raw in ["C:\\", "C:/"] {
            let mut filters = SearchFilters::default();
            let (pat, drives) = run_safety_nets(raw, &mut filters);
            assert_eq!(pat, "*", "`{raw}` becomes match-all on drive C");
            assert_eq!(drives, vec![uffs_mft::platform::DriveLetter::C]);
        }
    }

    #[test]
    fn safety_net_drive_plus_name_keeps_name_body() {
        let mut filters = SearchFilters::default();
        let (pat, drives) = run_safety_nets("C:report", &mut filters);
        assert_eq!(pat, "report");
        assert_eq!(drives, vec![uffs_mft::platform::DriveLetter::C]);
    }

    #[test]
    fn safety_net_drive_root_single_segment_collapses_to_name() {
        // `C:\report` must behave identically to `C:report`.
        let mut filters = SearchFilters::default();
        let (pat, drives) = run_safety_nets("C:\\report", &mut filters);
        assert_eq!(pat, "report");
        assert_eq!(drives, vec![uffs_mft::platform::DriveLetter::C]);
    }

    #[test]
    fn safety_net_drive_plus_glob_segment_promotes_to_ext() {
        // `C:\*.pdf` → drive C + (via ext-glob compose) pattern="*" + ext=pdf.
        let mut filters = SearchFilters::default();
        let (pat, drives) = run_safety_nets("C:\\*.pdf", &mut filters);
        assert_eq!(pat, "*");
        assert_eq!(drives, vec![uffs_mft::platform::DriveLetter::C]);
        assert_eq!(filters.extensions, vec!["pdf".to_owned()]);
    }

    #[test]
    fn safety_net_drive_plus_multi_segment_keeps_tree_body() {
        let mut filters = SearchFilters::default();
        let (pat, drives) = run_safety_nets("C:\\report\\*", &mut filters);
        assert_eq!(pat, "report\\*", "multi-segment stays a tree path");
        assert_eq!(drives, vec![uffs_mft::platform::DriveLetter::C]);
    }

    #[test]
    fn safety_net_leaves_no_drive_path_pattern_alone() {
        let mut filters = SearchFilters::default();
        let (pat, drives) = run_safety_nets("\\rnio\\*", &mut filters);
        assert_eq!(pat, "\\rnio\\*", "no drive prefix → untouched");
        assert_eq!(drives, Vec::<uffs_mft::platform::DriveLetter>::new());
    }
}
