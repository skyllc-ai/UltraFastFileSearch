// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Parse raw CLI argument strings into [`SearchParams`].
//!
//! This module lives in `uffs-client` so the daemon can build a
//! `SearchParams` from the CLI's raw `argv` without the CLI needing
//! to parse each flag into typed fields first.  All sugar expansion
//! (`--begins-with`, `--between`, `--exact-size`, `--word`, etc.)
//! happens here.

// `CliArgsError` is re-exported at the `cli_args` module path so the
// public API of `from_cli_args` has a stable, canonically-named typed
// error.  Phase 5d migration: see the type's doc-comment in
// `cli_args_helpers.rs` for the full rationale.
pub use super::cli_args_helpers::CliArgsError as Error;
use super::cli_args_helpers::{
    CliArgsError, drives_csv, extract_extensions_from_regex, flag_val, is_pure_ext_glob, non_empty,
    parse_bool, parse_i32, parse_size, parse_u16, parse_u32, parse_u64,
};
use super::{SearchFilterMode, SearchParams, SearchResponseMode};

// ── Public entry point ─────────────────────────────────────────────────

impl SearchParams {
    /// Build a fully-populated `SearchParams` from raw CLI argument strings.
    ///
    /// Handles all sugar expansion (`--begins-with`, `--between`,
    /// `--exact-size`, `--word`, `--count`/`--facet`/`--stats`/`--histogram`
    /// → `--agg`, etc.) so the caller doesn't need to.
    ///
    /// # Errors
    ///
    /// Returns a [`CliArgsError`] variant on malformed arguments.  The
    /// [`core::fmt::Display`] strings stay byte-identical with the
    /// pre-Phase-5d `Result<_, String>` payloads so operator-facing CLI
    /// error output is unchanged.
    #[expect(
        clippy::too_many_lines,
        reason = "mechanical 1:1 flag-to-field mapping"
    )]
    pub fn from_cli_args(args: &[String]) -> Result<Self, CliArgsError> {
        let mut raw = RawCliArgs::default();
        let mut iter = args.iter().cloned().peekable();

        while let Some(arg) = iter.next() {
            let flag = arg.split('=').next().unwrap_or(&arg);
            match flag {
                "--verbose" | "-v" | "--no-bitmap" | "--debug-tree" => {}
                "--files-only" => raw.files_only = true,
                "--dirs-only" => raw.dirs_only = true,
                "--hide-system" => raw.hide_system = true,
                "--hide-ads" => raw.hide_ads = true,
                "--normalize-malformed" => raw.normalize_malformed = true,
                // Diagnostic only -- uffs-content sets this itself for real
                // jobs.
                "--resolve-lcn-order" => raw.resolve_lcn_order = true,
                // WI-4.4 forensic filters: find ill-formed (non-UTF-8) names.
                "--malformed" => raw.malformed = Some(true),
                "--well-formed" => raw.malformed = Some(false),
                "--malformed-path" => raw.malformed_path = Some(true),
                "--profile" => raw.profile = true,
                "--benchmark" => raw.benchmark = true,
                "--no-cache" => raw.no_cache = true,
                "--case" => raw.case = true,
                "--smart-case" => raw.smart_case = true,
                "--word" => raw.word = true,
                "--name-only" => raw.name_only = true,
                "--sort-desc" => raw.sort_desc = true,
                "--parity-compat" => raw.parity_compat = true,
                "--count" => raw.count = true,
                "--rows" => raw.rows = true,
                "--no-output" => raw.no_output = true,
                "--drive" | "-d" => {
                    let dv = flag_val(&arg, flag, &mut iter)?;
                    raw.drive = drives_csv(&dv)?.into_iter().next();
                }
                "--drives" => {
                    let dv = flag_val(&arg, "--drives", &mut iter)?;
                    raw.drives = Some(drives_csv(&dv)?);
                }
                "--mft-file" => {
                    let mv = flag_val(&arg, "--mft-file", &mut iter)?;
                    raw.mft_file = mv.split(',').map(|sv| sv.trim().to_owned()).collect();
                }
                "--data-dir" => raw.data_dir = Some(flag_val(&arg, "--data-dir", &mut iter)?),
                "--agg" => raw.agg.push(flag_val(&arg, "--agg", &mut iter)?),
                "--facet" => raw.facet.push(flag_val(&arg, "--facet", &mut iter)?),
                "--stats" => raw.stats.push(flag_val(&arg, "--stats", &mut iter)?),
                "--histogram" => raw
                    .histogram
                    .push(flag_val(&arg, "--histogram", &mut iter)?),
                "--agg-cursor" => raw.agg_cursor = Some(flag_val(&arg, "--agg-cursor", &mut iter)?),
                "--agg-page-size" => {
                    let pv = flag_val(&arg, "--agg-page-size", &mut iter)?;
                    raw.agg_page_size = Some(parse_u16("--agg-page-size", &pv)?);
                }
                "--attr" => raw.attr = Some(flag_val(&arg, "--attr", &mut iter)?),
                // Snapshot delete-visibility diff: search the deleted set of
                // the given baseline MFT capture vs the live index. Every other
                // flag then filters/shapes that set like a normal search.
                "--diff" => raw.diff_baseline = Some(flag_val(&arg, "--diff", &mut iter)?),
                "--newer" => raw.newer = Some(flag_val(&arg, "--newer", &mut iter)?),
                "--older" => raw.older = Some(flag_val(&arg, "--older", &mut iter)?),
                "--newer-created" => {
                    raw.newer_created = Some(flag_val(&arg, "--newer-created", &mut iter)?);
                }
                "--older-created" => {
                    raw.older_created = Some(flag_val(&arg, "--older-created", &mut iter)?);
                }
                "--newer-accessed" => {
                    raw.newer_accessed = Some(flag_val(&arg, "--newer-accessed", &mut iter)?);
                }
                "--older-accessed" => {
                    raw.older_accessed = Some(flag_val(&arg, "--older-accessed", &mut iter)?);
                }
                "--exclude" => raw.exclude = Some(flag_val(&arg, "--exclude", &mut iter)?),
                "--in-path" => raw.in_path = Some(flag_val(&arg, "--in-path", &mut iter)?),
                "--not-in-path" => {
                    raw.path_excludes = Some(flag_val(&arg, "--not-in-path", &mut iter)?);
                }
                "--type" => raw.type_filter = Some(flag_val(&arg, "--type", &mut iter)?),
                "--ext" => raw.ext = Some(flag_val(&arg, "--ext", &mut iter)?),
                "--month" => raw.month = Some(flag_val(&arg, "--month", &mut iter)?),
                "--between" => raw.between = Some(flag_val(&arg, "--between", &mut iter)?),
                "--begins-with" => {
                    raw.begins_with = Some(flag_val(&arg, "--begins-with", &mut iter)?);
                }
                "--ends-with" => raw.ends_with = Some(flag_val(&arg, "--ends-with", &mut iter)?),
                "--contains" => raw.contains = Some(flag_val(&arg, "--contains", &mut iter)?),
                "--not-contains" => {
                    raw.not_contains = Some(flag_val(&arg, "--not-contains", &mut iter)?);
                }
                "--sort" => raw.sort = Some(flag_val(&arg, "--sort", &mut iter)?),
                "--format" | "-f" => raw.format = flag_val(&arg, flag, &mut iter)?,
                "--out" => raw.out = flag_val(&arg, "--out", &mut iter)?,
                "--columns" => raw.columns = flag_val(&arg, "--columns", &mut iter)?,
                "--sep" => raw.sep = flag_val(&arg, "--sep", &mut iter)?,
                "--quotes" => raw.quotes = flag_val(&arg, "--quotes", &mut iter)?,
                "--header" => {
                    // Store as `Some(parsed)` so the assembly step
                    // can distinguish "user explicitly set the flag"
                    // from "user did not mention --header at all".
                    // An absent `--header` must leave
                    // `SearchParams::output_header` as `None` so the
                    // daemon's `uffs_format::OutputConfig` default
                    // (`header = true`) takes effect — otherwise the
                    // CSV blob fast path would ship without a header
                    // line, silently regressing the CLI's long-
                    // standing "header by default" contract.
                    raw.header = Some(parse_bool(
                        "--header",
                        &flag_val(&arg, "--header", &mut iter)?,
                    )?);
                }
                "--pos" => raw.pos = flag_val(&arg, "--pos", &mut iter)?,
                "--neg" => raw.neg = flag_val(&arg, "--neg", &mut iter)?,
                "--query-mode" => raw.query_mode = flag_val(&arg, "--query-mode", &mut iter)?,
                "--limit" | "-n" => raw.limit = parse_u32(flag, &flag_val(&arg, flag, &mut iter)?)?,
                "--tz-offset" => {
                    raw.tz_offset = Some(parse_i32(
                        "--tz-offset",
                        &flag_val(&arg, "--tz-offset", &mut iter)?,
                    )?);
                }
                "--min-size" => {
                    raw.min_size = Some(parse_size(&flag_val(&arg, "--min-size", &mut iter)?)?);
                }
                "--max-size" => {
                    raw.max_size = Some(parse_size(&flag_val(&arg, "--max-size", &mut iter)?)?);
                }
                "--exact-size" => {
                    raw.exact_size = Some(parse_size(&flag_val(&arg, "--exact-size", &mut iter)?)?);
                }
                "--min-size-on-disk" => {
                    raw.min_size_on_disk = Some(parse_size(&flag_val(
                        &arg,
                        "--min-size-on-disk",
                        &mut iter,
                    )?)?);
                }
                "--max-size-on-disk" => {
                    raw.max_size_on_disk = Some(parse_size(&flag_val(
                        &arg,
                        "--max-size-on-disk",
                        &mut iter,
                    )?)?);
                }
                "--exact-size-on-disk" => {
                    raw.exact_size_on_disk = Some(parse_size(&flag_val(
                        &arg,
                        "--exact-size-on-disk",
                        &mut iter,
                    )?)?);
                }
                "--min-treesize" => {
                    raw.min_treesize =
                        Some(parse_size(&flag_val(&arg, "--min-treesize", &mut iter)?)?);
                }
                "--max-treesize" => {
                    raw.max_treesize =
                        Some(parse_size(&flag_val(&arg, "--max-treesize", &mut iter)?)?);
                }
                "--min-tree-allocated" => {
                    raw.min_tree_allocated = Some(parse_size(&flag_val(
                        &arg,
                        "--min-tree-allocated",
                        &mut iter,
                    )?)?);
                }
                "--max-tree-allocated" => {
                    raw.max_tree_allocated = Some(parse_size(&flag_val(
                        &arg,
                        "--max-tree-allocated",
                        &mut iter,
                    )?)?);
                }
                "--min-descendants" => {
                    raw.min_descendants = Some(parse_u32(
                        "--min-descendants",
                        &flag_val(&arg, "--min-descendants", &mut iter)?,
                    )?);
                }
                "--max-descendants" => {
                    raw.max_descendants = Some(parse_u32(
                        "--max-descendants",
                        &flag_val(&arg, "--max-descendants", &mut iter)?,
                    )?);
                }
                "--exact-descendants" => {
                    raw.exact_descendants = Some(parse_u32(
                        "--exact-descendants",
                        &flag_val(&arg, "--exact-descendants", &mut iter)?,
                    )?);
                }
                "--min-name-length" => {
                    raw.min_name_length = Some(parse_u16(
                        "--min-name-length",
                        &flag_val(&arg, "--min-name-length", &mut iter)?,
                    )?);
                }
                "--max-name-length" => {
                    raw.max_name_length = Some(parse_u16(
                        "--max-name-length",
                        &flag_val(&arg, "--max-name-length", &mut iter)?,
                    )?);
                }
                "--min-path-length" => {
                    raw.min_path_length = Some(parse_u16(
                        "--min-path-length",
                        &flag_val(&arg, "--min-path-length", &mut iter)?,
                    )?);
                }
                "--max-path-length" => {
                    raw.max_path_length = Some(parse_u16(
                        "--max-path-length",
                        &flag_val(&arg, "--max-path-length", &mut iter)?,
                    )?);
                }
                "--min-bulkiness" => {
                    raw.min_bulkiness = Some(parse_u64(
                        "--min-bulkiness",
                        &flag_val(&arg, "--min-bulkiness", &mut iter)?,
                    )?);
                }
                "--max-bulkiness" => {
                    raw.max_bulkiness = Some(parse_u64(
                        "--max-bulkiness",
                        &flag_val(&arg, "--max-bulkiness", &mut iter)?,
                    )?);
                }
                "--chaos-seed" | "--reserved-allocated" => {
                    let _ignored: String = flag_val(&arg, flag, &mut iter)?;
                }
                other => {
                    if other.starts_with('-') {
                        return Err(CliArgsError::UnknownFlag {
                            flag: other.to_owned(),
                        });
                    }
                    if raw.pattern.is_some() {
                        return Err(CliArgsError::UnexpectedArgument {
                            arg: other.to_owned(),
                        });
                    }
                    raw.pattern = Some(arg);
                }
            }
        }

        raw.into_search_params()
    }
}

// ── Raw CLI args holder ────────────────────────────────────────────────

/// Transient container for raw CLI values before sugar expansion.
///
/// Fields mirror CLI flags 1:1; documented at the flag-parsing level.
#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors CLI flags")]
#[cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "fields mirror CLI flags — documented at the parser level"
    )
)]
struct RawCliArgs {
    pattern: Option<String>,
    drive: Option<uffs_mft::platform::DriveLetter>,
    drives: Option<Vec<uffs_mft::platform::DriveLetter>>,
    files_only: bool,
    dirs_only: bool,
    hide_system: bool,
    hide_ads: bool,
    normalize_malformed: bool,
    /// Diagnostic-only flag (see `--resolve-lcn-order` above).
    resolve_lcn_order: bool,
    /// WI-4.4: `Some(true)` from `--malformed`, `Some(false)` from
    /// `--well-formed`, `None` if neither (no filter).
    malformed: Option<bool>,
    /// WI-4.4: `Some(true)` from `--malformed-path`.
    malformed_path: Option<bool>,
    /// Snapshot-diff baseline path from `--diff <BASELINE>`: turns the query
    /// into a search over the deleted set of that baseline vs the live index.
    diff_baseline: Option<String>,
    profile: bool,
    benchmark: bool,
    no_cache: bool,
    case: bool,
    smart_case: bool,
    word: bool,
    name_only: bool,
    sort_desc: bool,
    parity_compat: bool,
    count: bool,
    rows: bool,
    no_output: bool,
    sort: Option<String>,
    ext: Option<String>,
    attr: Option<String>,
    newer: Option<String>,
    older: Option<String>,
    newer_created: Option<String>,
    older_created: Option<String>,
    newer_accessed: Option<String>,
    older_accessed: Option<String>,
    exclude: Option<String>,
    in_path: Option<String>,
    path_excludes: Option<String>,
    type_filter: Option<String>,
    month: Option<String>,
    between: Option<String>,
    begins_with: Option<String>,
    ends_with: Option<String>,
    contains: Option<String>,
    not_contains: Option<String>,
    limit: u32,
    min_size: Option<u64>,
    max_size: Option<u64>,
    exact_size: Option<u64>,
    min_size_on_disk: Option<u64>,
    max_size_on_disk: Option<u64>,
    exact_size_on_disk: Option<u64>,
    min_treesize: Option<u64>,
    max_treesize: Option<u64>,
    min_tree_allocated: Option<u64>,
    max_tree_allocated: Option<u64>,
    min_descendants: Option<u32>,
    max_descendants: Option<u32>,
    exact_descendants: Option<u32>,
    min_name_length: Option<u16>,
    max_name_length: Option<u16>,
    min_path_length: Option<u16>,
    max_path_length: Option<u16>,
    min_bulkiness: Option<u64>,
    max_bulkiness: Option<u64>,
    format: String,
    out: String,
    columns: String,
    sep: String,
    quotes: String,
    header: Option<bool>,
    pos: String,
    neg: String,
    query_mode: String,
    tz_offset: Option<i32>,
    agg: Vec<String>,
    facet: Vec<String>,
    stats: Vec<String>,
    histogram: Vec<String>,
    agg_cursor: Option<String>,
    agg_page_size: Option<u16>,
    mft_file: Vec<String>,
    data_dir: Option<String>,
}

impl RawCliArgs {
    /// Convert raw CLI values into a fully-populated [`SearchParams`],
    /// performing all sugar expansion.
    #[expect(clippy::too_many_lines, reason = "sugar expansion for 60+ flags")]
    #[expect(
        clippy::cognitive_complexity,
        reason = "three composable sugar rewrites (drive-prefix, ext-glob, regex-ext) plus filter normalisation; splitting further would fragment the parse pipeline"
    )]
    fn into_search_params(mut self) -> Result<SearchParams, CliArgsError> {
        // ── Pattern sugar: --begins-with / --ends-with / --contains ─
        let raw_pattern = self
            .pattern
            .take()
            .or_else(|| self.begins_with.take().map(|prefix| format!("{prefix}*")))
            .or_else(|| self.ends_with.take().map(|suffix| format!("*{suffix}")))
            .or_else(|| self.contains.take().map(|needle| format!("*{needle}*")))
            .unwrap_or_else(|| "*".to_owned());

        // ── --not-contains → merge into --exclude ──────────────────
        let exclude = match (self.exclude.take(), self.not_contains.take()) {
            (Some(ex), Some(nc)) => Some(format!("{ex},*{nc}*")),
            (Some(ex), None) => Some(ex),
            (None, Some(nc)) => Some(format!("*{nc}*")),
            (None, None) => None,
        };

        // ── Scope prefixes: path:/dir:/file:/<letter>: ─────────────
        //
        // `pattern` is `mut` so the ext-pattern sugar block below can
        // rewrite `*.<ext>` → `"*"` in-place without introducing a
        // shadow binding (see the `shadow_reuse` workspace lint).
        //
        // The `<letter>:` branch strips a bare drive prefix like
        // `C:*.dll` into `drive=C` + `pattern="*.dll"`, which then
        // composes with the ext-glob promotion further down to yield
        // `drive=C` + `pattern="*"` + `ext=Some("dll")`.  Path-anchored
        // patterns like `C:\*.dll` keep the `\` and skip this branch —
        // they must stay on the tree walker.  An explicit `--drive` /
        // `--drives` flag always wins over the inferred prefix.
        let (match_path, mut pattern) = if let Some(rest) = raw_pattern.strip_prefix("path:") {
            (true, rest.to_owned())
        } else if let Some(rest) = raw_pattern.strip_prefix("dir:") {
            self.dirs_only = true;
            (false, rest.to_owned())
        } else if let Some(rest) = raw_pattern.strip_prefix("file:") {
            self.files_only = true;
            (false, rest.to_owned())
        } else if let Some((letter, rest)) = uffs_mft::platform::split_drive_prefix(&raw_pattern) {
            if self.drive.is_none() && self.drives.is_none() {
                self.drive = Some(letter);
            }
            (false, rest.to_owned())
        } else {
            (false, raw_pattern)
        };

        // ── --name-only validation ─────────────────────────────────
        if self.name_only
            && (pattern.contains('\\') || pattern.contains('/'))
            && !pattern.starts_with('>')
        {
            return Err(CliArgsError::NameOnlyWithPathPattern);
        }

        // ── --exact-size / --exact-descendants ─────────────────────
        let min_size = self.exact_size.or(self.min_size);
        let max_size = self.exact_size.or(self.max_size);
        let min_desc = self.exact_descendants.or(self.min_descendants);
        let max_desc = self.exact_descendants.or(self.max_descendants);
        let min_sod = self.exact_size_on_disk.or(self.min_size_on_disk);
        let max_sod = self.exact_size_on_disk.or(self.max_size_on_disk);

        // ── --between START,END → newer + older ────────────────────
        let (between_newer, between_older) = self.between.as_ref().map_or((None, None), |bv| {
            let mut parts = bv.splitn(2, ',');
            (
                parts.next().map(String::from),
                parts.next().map(String::from),
            )
        });
        let newer = self.newer.or(between_newer);
        let older = self.older.or(between_older);

        // ── Month spec → Vec<u32> ──────────────────────────────────
        let allowed_months: Vec<u32> = self
            .month
            .as_deref()
            .map_or_default(crate::format::parse_month_spec);

        // ── Smart case ─────────────────────────────────────────────
        let case_sensitive = if self.case {
            true
        } else if self.smart_case {
            pattern.chars().any(char::is_uppercase)
        } else {
            false
        };

        // ── Ext-pattern sugar: `*.<ext>` → pattern="*" + ext=<ext> ──
        //
        // Restores the fast path that the old fat-CLI used: a bare
        // `*.<ext>` query becomes semantically identical to
        // `* --ext <ext>`, which routes through the daemon's
        // `ExtensionIndex` (O(K) iteration over matching records)
        // instead of the trigram+glob path (O(candidates) with
        // per-record glob match and name-fold).
        //
        // Guards:
        // - `match_path` off: `path:*.dll` scans full paths, not names.
        // - `case_sensitive` off: `--case *.DLL` would return zero results
        //   today (no files have literal uppercase extensions on NTFS), but the
        //   ext index is case-folded and would return all .dll files.  Preserve
        //   the stricter semantic.
        // - `self.ext` is `None`: don't clobber an explicit `--ext`.
        // - `is_pure_ext_glob`: only `*.<alnum+_>+` shapes are safe to promote;
        //   `*.tar.gz`, `*.[ch]`, etc. stay on trigram.
        //
        // Mirrored at dispatch time by
        // `uffs_core::search::backend::search_index` as a safety net for
        // direct JSON-RPC `search` callers that skip this parser.
        if !match_path && !case_sensitive && self.ext.is_none() && is_pure_ext_glob(&pattern) {
            let ext = pattern
                .strip_prefix("*.")
                .unwrap_or_default()
                .to_ascii_lowercase();
            self.ext = Some(ext);
            // Reuse the existing `String` allocation instead of
            // `pattern = "*".to_owned()` (which would heap-allocate a
            // fresh `String`).  Satisfies `clippy::assigning_clones`.
            pattern.clear();
            pattern.push('*');
        }

        // ── Regex ext-alternation sugar: `>.*\.(a|b|c)$` → `*` + ext=a,b,c ─
        //
        // Mirror of the ext-glob promotion above for the regex shape.
        // Routes patterns like `>.*\.(jpg|png|heic)$` through the
        // `ExtensionIndex` fast path instead of a full-scan regex
        // compile → per-record match.  On a 3.5 M-record C: drive
        // this drops `>.*\.(jpg|png|heic)$` from ~298 ms to ~95 ms —
        // matching the equivalent `--ext jpg,png,heic` glob path.
        //
        // Guards match the ext-glob rule: not `match_path`, not
        // `case_sensitive`, and `--ext` not set by the user.  See
        // `extract_extensions_from_regex` for the acceptance matrix —
        // it **requires** a trailing `$` anchor so the rewrite is
        // semantically lossless (without `$` the regex matches
        // `.jpg` anywhere in the name, which the ext-index cannot
        // replicate).
        //
        // Mirrored at dispatch time by
        // `uffs_core::search::dispatch::apply_dispatch_safety_nets`
        // (rewrite #3) as a safety net for direct JSON-RPC `search`
        // callers that skip this parser.
        if !match_path
            && !case_sensitive
            && self.ext.is_none()
            && let Some(exts) = extract_extensions_from_regex(&pattern)
        {
            // `--ext` takes the normalised CSV form expected by
            // `SearchFilters::from_params` (see `ext_filter.split(',')`).
            self.ext = Some(exts.join(","));
            pattern.clear();
            pattern.push('*');
        }

        // ── Aggregate sugar ────────────────────────────────────────
        let mut agg_specs = self.agg;
        if self.count && !agg_specs.iter().any(|spec| spec == "count") {
            agg_specs.push("count".to_owned());
        }
        for facet in &self.facet {
            if let Some((field, top)) = facet.split_once(':') {
                agg_specs.push(format!("terms:{field},top={top}"));
            } else {
                agg_specs.push(format!("terms:{facet},top=20"));
            }
        }
        for stat in &self.stats {
            agg_specs.push(format!("stats:{stat}"));
        }
        for hist in &self.histogram {
            if let Some((field, interval)) = hist.split_once(':') {
                agg_specs.push(format!("hist:{field},interval={interval}"));
            } else {
                agg_specs.push(format!("hist:{hist}"));
            }
        }
        let force_rows = self.rows;
        let agg_only = !agg_specs.is_empty() && !force_rows;

        // ── Drives ─────────────────────────────────────────────────
        let drives: Vec<uffs_mft::platform::DriveLetter> = self
            .drives
            .or_else(|| self.drive.map(|ch| vec![ch]))
            .unwrap_or_default();

        // ── Filter mode ────────────────────────────────────────────
        let filter_mode = if self.files_only {
            Some(SearchFilterMode::Files)
        } else if self.dirs_only {
            Some(SearchFilterMode::Dirs)
        } else {
            Some(SearchFilterMode::All)
        };
        let filter = if self.files_only {
            Some("files".to_owned())
        } else if self.dirs_only {
            Some("dirs".to_owned())
        } else {
            None
        };

        // ── Limit ──────────────────────────────────────────────────
        let limit = if agg_only {
            Some(0)
        } else {
            (self.limit > 0).then_some(self.limit)
        };

        // ── Sort ───────────────────────────────────────────────────
        let sorts = self.sort.as_deref().map_or_else(Vec::new, |sort_str| {
            SearchParams::canonicalize_legacy_sort(sort_str, self.sort_desc)
        });

        // ── Columns / projection ───────────────────────────────────
        let columns = if self.parity_compat {
            "parity".to_owned()
        } else {
            self.columns
        };
        let projection: Vec<String> = if columns.is_empty() {
            Vec::new()
        } else {
            columns
                .split(',')
                .map(|col| col.trim().to_owned())
                .collect()
        };

        // ── Output config ──────────────────────────────────────────
        let output_file = if self.out.is_empty() || self.out == "console" {
            None
        } else {
            let path = std::path::Path::new(&self.out);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
            Some(abs.to_string_lossy().into_owned())
        };

        // ── Aggregation wire specs ─────────────────────────────────
        let aggregations = agg_specs
            .iter()
            .map(|spec| {
                let is_preset = crate::format::is_aggregate_preset(spec);
                super::AggregateSpecWire {
                    kind: if spec == "count" {
                        "count".to_owned()
                    } else if is_preset {
                        "preset".to_owned()
                    } else {
                        "raw".to_owned()
                    },
                    label: (!is_preset && spec != "count").then(|| spec.clone()),
                    preset: is_preset.then(|| spec.clone()),
                    ..super::AggregateSpecWire::default()
                }
            })
            .collect();

        // ── Assemble ───────────────────────────────────────────────
        let mut params = SearchParams {
            // Core
            pattern,
            case_sensitive,
            whole_word: false,
            match_path,
            // Sort
            sort: self.sort,
            sorts,
            sort_desc: self.sort_desc,
            // Limit
            limit,
            // Filter mode
            filter,
            filter_mode,
            predicates: Vec::new(),
            drives: drives.clone(),
            projection,
            response_mode: Some(SearchResponseMode::Rows),
            // Size
            min_size,
            max_size,
            // Descendants
            min_descendants: min_desc,
            max_descendants: max_desc,
            // Time
            newer,
            older,
            newer_created: self.newer_created,
            older_created: self.older_created,
            newer_accessed: self.newer_accessed,
            older_accessed: self.older_accessed,
            // Attribute / extension / exclude
            attr: self.attr,
            ext: self.ext,
            exclude,
            path_contains: self.in_path,
            path_excludes: self.path_excludes,
            type_filter: self.type_filter,
            min_bulkiness: self.min_bulkiness,
            max_bulkiness: self.max_bulkiness,
            // Length
            min_name_len: self.min_name_length,
            max_name_len: self.max_name_length,
            min_path_len: self.min_path_length,
            max_path_len: self.max_path_length,
            // Size-on-disk
            min_allocated: min_sod,
            max_allocated: max_sod,
            // Tree metrics
            min_treesize: self.min_treesize,
            max_treesize: self.max_treesize,
            min_tree_allocated: self.min_tree_allocated,
            max_tree_allocated: self.max_tree_allocated,
            // Month
            allowed_months,
            // WI-4.4 malformed-name filters
            malformed: self.malformed,
            malformed_path: self.malformed_path,
            // Misc
            hide_system: self.hide_system,
            hide_ads: self.hide_ads,
            normalize_malformed: self.normalize_malformed,
            // Profiling
            profile: self.profile || self.benchmark,
            aggregations,
            // Row precedence (high → low): --rows (on) > agg (off) >
            // --no-output (off) > default (on).
            include_rows: force_rows || (agg_specs.is_empty() && !self.no_output),
            resolve_lcn_order: self.resolve_lcn_order,
            agg_cursor: self.agg_cursor,
            agg_page_size: self.agg_page_size,
            // Direct file output
            output_file,
            output_separator: non_empty(self.sep),
            output_quote: non_empty(self.quotes),
            // Forward `None` when the user did not pass `--header` so
            // the daemon's `uffs_format::OutputConfig` default
            // (`header = true`) wins.  Passing `Some(false)` here
            // unconditionally (as we used to) stripped the header
            // line from every default CSV query — see the comment
            // on the `"--header" =>` arm above.
            output_header: self.header,
            output_pos: non_empty(self.pos),
            output_neg: non_empty(self.neg),
            output_columns: non_empty(columns),
            output_parity_compat: self.parity_compat.then_some(true),
            output_tz_offset_hours: self.tz_offset,
            // Forward the CLI's `--format` value so the daemon can
            // gate its `try_pack_csv_blob` pre-format fast path on it.
            // Phase 3: `"csv"` and `"custom"` both take the fast path;
            // `"json"` / `"table"` stay on the CLI's local formatter.
            // See `SearchParams::output_format` for the full rationale.
            //
            // Always populated (defaulting to `"csv"` when the user did
            // not pass `--format`) so the daemon's blob fast paths can
            // treat an absent `output_format` as an *explicit* opt-out,
            // which is how non-CLI callers (e.g. `uffs-mcp`) signal
            // that they want structured `InlineRows` back, not a
            // pre-rendered CSV blob they can't re-parse.  See the gate
            // in `uffs_daemon::handler::RequestHandler::is_csv_blob_eligible`.
            output_format: Some(non_empty(self.format.clone()).unwrap_or_else(|| "csv".to_owned())),
            // Drives to echo into the legacy drive footer when
            // `--format custom`.  Same letters as `drives` above for
            // the main CLI path (which populates `drives` from
            // `--drive` / `--drives`); the thin-client passthrough in
            // `commands::search::dispatch` handles `--mft-file`
            // separately and overrides this field directly on the
            // passthrough `SearchParams`.
            output_drive_targets: drives,
            // Snapshot-diff baseline (set by the `--diff` command path, which
            // parses the remaining flags as a normal search); a plain search
            // leaves it unset.
            diff_baseline: self.diff_baseline.clone(),
        };
        // A diff with no explicit pattern lists every deleted file.
        if params.diff_baseline.is_some() && params.pattern.is_empty() {
            "*".clone_into(&mut params.pattern);
        }
        params.populate_canonical_fields();
        Ok(params)
    }
}
