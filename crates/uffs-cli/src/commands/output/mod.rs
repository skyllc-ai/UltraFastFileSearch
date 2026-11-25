// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Output helpers for CLI search commands.
//!
//! Formats `SearchRow` (from the daemon protocol) directly — no polars,
//! no `DisplayRow`, no `DataFrame`.  This is the thin-client output path.

mod parity;

mod render_into;
use core::time::Duration;
use std::fs::File;
use std::io::{BufWriter, Write};

use anyhow::{Context as _, Result};
use parity::{write_legacy_drive_footer, write_parity};
pub use render_into::render_native_results_into;
use serde_json::Value;

// ── Value extraction helpers ───────────────────────────────────────────

/// Get string field.
fn vs(row: &Value, key: &str) -> String {
    row[key].as_str().unwrap_or("").to_owned()
}

/// Get u64 field.
fn vu(row: &Value, key: &str) -> u64 {
    row[key].as_u64().unwrap_or(0)
}

/// Get i64 field.
fn vi(row: &Value, key: &str) -> i64 {
    row[key].as_i64().unwrap_or(0)
}

/// Get u32 field (clamped to `u32::MAX` on overflow).
///
/// NTFS file attributes (`FILE_ATTRIBUTE_*`) are documented as `u32` by
/// Microsoft, so the saturating `try_from` fallback is unreachable for
/// well-formed daemon responses; it nevertheless replaces the previous
/// truncating `as u32` to keep the conversion idiomatic and lint-free.
fn vu32(row: &Value, key: &str) -> u32 {
    let raw = row.get(key).and_then(Value::as_u64).unwrap_or(0);
    u32::try_from(raw).unwrap_or(u32::MAX)
}

/// Get bool field.
fn vb(row: &Value, key: &str) -> bool {
    row[key].as_bool().unwrap_or(false)
}

/// Context for legacy baseline-compatible footer formatting.
pub(crate) struct CppFooterContext<'a> {
    /// Drive letters to include in the footer (e.g.
    /// `[DriveLetter::C, DriveLetter::D]`).
    pub output_targets: &'a [uffs_mft::platform::DriveLetter],
    /// Original search pattern string.
    pub pattern: &'a str,
    /// Total result row count for fast-scan heuristic.
    pub row_count: usize,
}

/// Write `SearchRow` search results to console or file.
///
/// For `json` format: serialises with `serde_json` (no polars).
/// For `csv`/`custom`: writes columnar text directly from `SearchRow` fields.
/// For `table`: formats a fixed-width text table.
///
/// # Errors
///
/// Returns an error if the operation fails.
#[expect(clippy::too_many_arguments, reason = "output config forwarding")]
pub fn write_native_results(
    rows: &[Value],
    format: &str,
    out: &str,
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    pos: &str,
    neg: &str,
    tz_offset: Option<i32>,
    output_targets: &[uffs_mft::platform::DriveLetter],
    _elapsed: Duration,
    pattern: &str,
) -> Result<()> {
    let is_console = out.is_empty()
        || matches!(
            out.to_lowercase().as_str(),
            "console" | "con" | "term" | "terminal"
        );

    let footer_ctx = CppFooterContext {
        output_targets,
        pattern,
        row_count: rows.len(),
    };

    let parity_ctx = ParityContext::new(pos, neg, tz_offset);

    if is_console {
        write_to_stdout(
            rows,
            format,
            columns,
            separator,
            quote,
            header,
            &footer_ctx,
            &parity_ctx,
        )
    } else {
        let file =
            File::create(out).with_context(|| format!("Failed to create output file: {out}"))?;
        let mut writer = BufWriter::new(file);
        write_formatted(
            &mut writer,
            rows,
            format,
            columns,
            separator,
            quote,
            header,
            &footer_ctx,
            &parity_ctx,
        )?;
        writer.flush()?;
        Ok(())
    }
}

/// Maximum in-memory buffer size for the single-`write_all` console fast path.
///
/// Results rendered below this threshold are built in a `Vec<u8>` and
/// flushed with one `stdout.lock().write_all` call — no `BufWriter`
/// flush storms, one syscall instead of `N / 64 KB`.  Larger results
/// fall back to the streaming `BufWriter` path so peak RSS stays
/// bounded on pathological queries.
const MULTICOL_BUFFER_CAP_BYTES: usize = 50 * 1024 * 1024;

/// Generous per-row size estimate for the up-front cap check.
///
/// 256 B comfortably covers a CSV row with a ~150 B path, seven
/// numeric/boolean columns, and quote overhead.  JSON is slightly
/// denser (~200-300 B per row); the estimate is conservative on the
/// high side so the cap fires only on truly huge result sets.
const MULTICOL_AVG_BYTES_PER_ROW: usize = 256;

/// Decision: render into a single buffer, or stream via `BufWriter`?
///
/// Extracted into a pure function so tests can pin each branch without
/// needing a live stdout handle or a hand-crafted 50 MB fixture.
#[derive(Debug, PartialEq, Eq)]
enum ConsoleWriteStrategy {
    /// Render into a `Vec<u8>` and emit one `write_all` — the fast path.
    SingleBuffer,
    /// Stream through a `BufWriter` — the memory-safe fallback for
    /// result sets whose estimated byte count exceeds the cap.
    Streaming,
}

/// Pick the console write strategy for a given row count.
///
/// `cap_bytes` and `est_bytes_per_row` are parameters (not constants)
/// so tests can drive the decision with a small synthetic cap.
const fn choose_console_strategy(
    row_count: usize,
    cap_bytes: usize,
    est_bytes_per_row: usize,
) -> ConsoleWriteStrategy {
    // Use saturating arithmetic so a pathological `row_count` close to
    // `usize::MAX` cannot silently wrap and misclassify as SingleBuffer.
    if row_count.saturating_mul(est_bytes_per_row) <= cap_bytes {
        ConsoleWriteStrategy::SingleBuffer
    } else {
        ConsoleWriteStrategy::Streaming
    }
}

/// Write formatted rows to stdout, choosing between single-buffer and
/// streaming paths based on [`choose_console_strategy`].
#[expect(clippy::too_many_arguments, reason = "output config forwarding")]
fn write_to_stdout(
    rows: &[Value],
    format: &str,
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    footer_ctx: &CppFooterContext<'_>,
    parity_ctx: &ParityContext<'_>,
) -> Result<()> {
    match choose_console_strategy(
        rows.len(),
        MULTICOL_BUFFER_CAP_BYTES,
        MULTICOL_AVG_BYTES_PER_ROW,
    ) {
        ConsoleWriteStrategy::SingleBuffer => {
            let estimated = rows.len().saturating_mul(MULTICOL_AVG_BYTES_PER_ROW);
            let mut buf: Vec<u8> = Vec::with_capacity(estimated);
            write_formatted(
                &mut buf, rows, format, columns, separator, quote, header, footer_ctx, parity_ctx,
            )?;
            // Phase 3.3: platform-aware write.  On a Windows real
            // console this dispatches to `WriteConsoleW` (single
            // UTF-8 → UTF-16 transcode, then one chunked call) to
            // bypass narrow-CRT codepage translation.  Everywhere else
            // this collapses to `stdout.lock().write_all(&buf)`.
            uffs_client::stdout_kind::write_stdout_buffer(&buf)
                .with_context(|| "Failed to write formatted output to stdout")?;
            Ok(())
        }
        ConsoleWriteStrategy::Streaming => {
            let stdout_handle = std::io::stdout();
            let mut stdout = BufWriter::with_capacity(64 * 1024, stdout_handle.lock());
            write_formatted(
                &mut stdout,
                rows,
                format,
                columns,
                separator,
                quote,
                header,
                footer_ctx,
                parity_ctx,
            )?;
            stdout.flush()?;
            Ok(())
        }
    }
}

/// Parity formatting context (timezone, boolean flags).
struct ParityContext<'a> {
    /// Positive boolean string (e.g., `"1"`).
    pos: &'a str,
    /// Negative boolean string (e.g., `"0"`).
    neg: &'a str,
    /// Timezone offset in seconds from UTC.
    tz_offset_secs: i32,
}

impl<'a> ParityContext<'a> {
    /// Resolve the parity context from the CLI's `--pos` / `--neg` /
    /// `--tz-offset` (hours) settings; an absent offset uses the local
    /// zone.
    fn new(pos: &'a str, neg: &'a str, tz_offset: Option<i32>) -> Self {
        Self {
            pos,
            neg,
            tz_offset_secs: tz_offset.map_or_else(
                || *LOCAL_TZ_OFFSET_SECS,
                |hours| hours.saturating_mul(3_600_i32),
            ),
        }
    }
}

/// Dispatch to the appropriate formatter.
#[expect(clippy::too_many_arguments, reason = "output config forwarding")]
fn write_formatted<W: Write>(
    writer: &mut W,
    rows: &[Value],
    format: &str,
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    footer_ctx: &CppFooterContext<'_>,
    parity_ctx: &ParityContext<'_>,
) -> Result<()> {
    let is_parity = columns.eq_ignore_ascii_case("parity");
    match format {
        "json" => write_json(writer, rows),
        "custom" => {
            if is_parity {
                write_parity(writer, rows, separator, quote, parity_ctx)?;
            } else {
                write_columnar(writer, rows, columns, separator, quote, header, parity_ctx)?;
            }
            write_legacy_drive_footer(writer, footer_ctx)
        }
        "table" => write_table(writer, rows),
        _ => {
            if is_parity {
                write_parity(writer, rows, separator, quote, parity_ctx)
            } else {
                write_columnar(writer, rows, columns, separator, quote, header, parity_ctx)
            }
        }
    }
}

/// Serialise rows as NDJSON (one JSON object per line).
fn write_json<W: Write>(writer: &mut W, rows: &[Value]) -> Result<()> {
    for row in rows {
        serde_json::to_writer(&mut *writer, row)?;
        writeln!(writer)?;
    }
    Ok(())
}

/// Write a simple aligned text table (name, size, modified, path).
fn write_table<W: Write>(writer: &mut W, rows: &[Value]) -> Result<()> {
    // Header
    writeln!(
        writer,
        "{:<50} {:>12} {:>19} Path",
        "Name", "Size", "Modified"
    )?;
    writeln!(writer, "{}", "─".repeat(120))?;

    for row in rows {
        let size_str = uffs_client::format::format_bytes(vu(row, "size"));
        let time_str = format_filetime_local(vi(row, "modified"));
        writeln!(
            writer,
            "{:<50} {:>12} {:>19} {}",
            vs(row, "name"),
            size_str,
            time_str,
            vs(row, "path")
        )?;
    }
    Ok(())
}

// ── Column definition table ─────────────────────────────────────────
//
// Inlined from `uffs-core::FieldId` / `field_metadata` so the CLI stays
// dependency-free (thin-client design).  Keep in sync with FieldId.

/// A column definition: `(canonical_name, &[aliases], display_name)`.
type ColDef = (&'static str, &'static [&'static str], &'static str);

/// Lookup table: canonical name + aliases → display name.
static COL_TABLE: &[ColDef] = &[
    ("name", &[], "Name"),
    ("path", &[], "Path"),
    ("path_only", &["pathonly", "path only"], "Path Only"),
    ("size", &[], "Size"),
    (
        "size_on_disk",
        &["allocated", "allocated_size", "sod"],
        "Size on Disk",
    ),
    ("created", &[], "Created"),
    ("modified", &["written"], "Last Written"),
    ("accessed", &[], "Last Accessed"),
    ("extension", &["ext"], "Extension"),
    ("drive", &["drv"], "Drive"),
    ("type", &["kind"], "Type"),
    ("descendants", &[], "Descendants"),
    ("treesize", &["tree_size"], "Tree Size"),
    ("tree_allocated", &[], "Tree Allocated"),
    ("bulkiness", &[], "Bulkiness"),
    ("name_length", &["namelength", "name length"], "Name Length"),
    ("path_length", &["pathlength", "path length"], "Path Length"),
    // Boolean attribute columns
    ("hidden", &[], "Hidden"),
    ("system", &[], "System"),
    ("archive", &[], "Archive"),
    ("readonly", &["read_only"], "Read-only"),
    ("compressed", &[], "Compressed"),
    ("encrypted", &[], "Encrypted"),
    ("sparse", &[], "Sparse"),
    ("reparse", &[], "Reparse"),
    ("offline", &[], "Offline"),
    (
        "not_indexed",
        &["notindexed", "not indexed"],
        "Not content indexed file",
    ),
    (
        "directory_flag",
        &["directoryflag", "directory flag"],
        "Directory Flag",
    ),
    ("integrity", &[], "Integrity"),
    ("no_scrub", &["noscrub"], "No scrub file"),
    ("pinned", &[], "Pinned"),
    ("unpinned", &[], "Unpinned"),
    ("recall_on_open", &["recallonopen"], "Recall on open"),
    (
        "recall_on_data_access",
        &["recallondataaccess"],
        "Recall on data access",
    ),
    ("temporary", &[], "Temporary"),
    ("virtual", &[], "Virtual"),
    ("attributes", &["parity_attributes"], "Attributes"),
    ("attribute_value", &[], "AttributeValue"),
    ("flags", &[], "Flags"),
    // WI-4.4 forensic columns (opt-in; never in `--columns all`).
    ("malformed", &["ill_formed", "illformed"], "Malformed"),
    (
        "malformed_path",
        &["malformedpath", "ill_formed_path"],
        "Malformed Path",
    ),
    ("name_hex", &["namehex"], "Name (hex)"),
];

/// Column order used when `--columns all` is specified (matches
/// `uffs-core::output::column::BASELINE_COLUMN_ORDER`).
static ALL_COLUMNS: &[&str] = &[
    "path",
    "name",
    "path_only",
    "size",
    "size_on_disk",
    "created",
    "modified",
    "accessed",
    "descendants",
    "readonly",
    "hidden",
    "system",
    "directory_flag",
    "archive",
    "sparse",
    "reparse",
    "compressed",
    "offline",
    "not_indexed",
    "encrypted",
    "integrity",
    "no_scrub",
    "recall_on_open",
    "pinned",
    "unpinned",
    "recall_on_data_access",
    "attributes",
    "treesize",
    "tree_allocated",
    "bulkiness",
    "type",
    "extension",
    "name_length",
    "path_length",
];

/// Resolve a user column name to its canonical name.
fn resolve_col_name(input: &str) -> Option<&'static str> {
    let lowered = input.to_ascii_lowercase();
    let trimmed = lowered.trim();
    for &(canon, aliases, _display) in COL_TABLE {
        if canon.eq_ignore_ascii_case(trimmed) {
            return Some(canon);
        }
        for &alias in aliases {
            if alias.eq_ignore_ascii_case(trimmed) {
                return Some(canon);
            }
        }
    }
    None
}

/// Get display name for a canonical column name.
fn display_name(canonical: &str) -> &str {
    for &(canon, _, display) in COL_TABLE {
        if canon == canonical {
            return display;
        }
    }
    canonical
}

/// Resolve column specification string to a list of canonical names.
///
/// No `--columns` means every standard column, the same set the daemon's
/// CSV writer emits when no columns are named (`uffs_format`'s
/// `BASELINE_COLUMN_ORDER`), so a CSV has one shape whichever side wrote it.
fn resolve_columns(columns: &str) -> Vec<&'static str> {
    if columns.is_empty() || columns.eq_ignore_ascii_case("all") {
        ALL_COLUMNS.to_vec()
    } else {
        columns
            .split(',')
            .filter_map(|name| resolve_col_name(name.trim()))
            .collect()
    }
}

/// Columns that `uffs_format::write_rows` quote-wraps in its CSV
/// output.  Everything else (numeric, datetime, boolean-flag) is
/// emitted raw.  Keep in sync with the match arms in
/// `uffs_format::writer::write_row` — any new quoted column there
/// must be added here so the CLI's `write_columnar` stays
/// byte-identical to the daemon's `try_pack_csv_blob` output.
fn is_quoted_column(canonical: &str) -> bool {
    matches!(
        canonical,
        // `name_hex` is a string column (quoted in uffs_format::writer); the
        // malformed bools render as raw 0/1 like the attribute-flag columns.
        "path" | "name" | "path_only" | "type" | "extension" | "name_hex"
    )
}

/// Write columnar (CSV-style) output from `SearchRow` fields.
///
/// Columns are resolved through the inline column table so display
/// names, flag decomposition, and derived columns (Path Only, Bulkiness,
/// etc.) work correctly.
///
/// Quoting policy mirrors `uffs_format::writer::write_row`: only
/// string-shaped columns (Path / Name / `PathOnly` / Type / Extension)
/// are wrapped in the configured quote character.  Numeric, datetime,
/// and boolean-flag columns are emitted raw.  This keeps the CLI's
/// fallback output byte-identical to the daemon's pre-formatted blob
/// on every column set.
///
/// Datetime formatting honours the timezone offset carried on
/// `parity_ctx` (matching `uffs_format::append_datetime_native`),
/// not the host's local offset.  The caller drives the offset via
/// `--tz-offset`; when absent, `dispatch::write_rows` feeds the
/// host-local value into `parity_ctx.tz_offset_secs`.
fn write_columnar<W: Write>(
    writer: &mut W,
    rows: &[Value],
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    parity_ctx: &ParityContext<'_>,
) -> Result<()> {
    let fields = resolve_columns(columns);

    // Header row — use display_name() for Title-Case headers.
    //
    // The header is terminated by `\n\n` (header line + blank
    // separator line) to match `uffs_format::write_rows`, so the
    // CLI fallback path and the daemon's pre-formatted blob produce
    // byte-identical output.  The blank line is the legacy baseline
    // the `uffs-core::output::tests::format_parity_*` regression
    // tests pin.
    if header {
        for (idx, field) in fields.iter().enumerate() {
            if idx > 0 {
                write!(writer, "{separator}")?;
            }
            let name = display_name(field);
            if quote.is_empty() {
                write!(writer, "{name}")?;
            } else {
                write!(writer, "{quote}{name}{quote}")?;
            }
        }
        writer.write_all(b"\n\n")?;
    }

    for row in rows {
        for (idx, field) in fields.iter().enumerate() {
            if idx > 0 {
                write!(writer, "{separator}")?;
            }
            let value = extract_field(row, field, parity_ctx.tz_offset_secs);
            if !quote.is_empty() && is_quoted_column(field) {
                write!(writer, "{quote}{value}{quote}")?;
            } else {
                write!(writer, "{value}")?;
            }
        }
        writeln!(writer)?;
    }
    Ok(())
}

/// Extract a field value from a JSON row by canonical column name.
///
/// Handles flag decomposition, path derivation, and computed columns.
///
/// `tz_offset_secs` drives the `Created` / `Modified` / `Accessed`
/// column formatting — matches
/// `uffs_format::append_datetime_native` so
/// `RequestHandler::try_pack_csv_blob`'s pre-formatted bytes stay
/// byte-identical with this fallback path.
fn extract_field(row: &Value, field: &str, tz_offset_secs: i32) -> String {
    let flags = vu32(row, "flags");
    match field {
        "name" => vs(row, "name"),
        "path" => vs(row, "path"),
        "path_only" => {
            let path = vs(row, "path");
            if vb(row, "is_directory") {
                path
            } else if let Some(pos) = path.rfind('\\') {
                path.get(..=pos).unwrap_or(&path).to_owned()
            } else {
                path
            }
        }
        "size" => vu(row, "size").to_string(),
        "size_on_disk" => vu(row, "allocated").to_string(),
        "created" => format_filetime_with_tz(vi(row, "created"), tz_offset_secs),
        "modified" => format_filetime_with_tz(vi(row, "modified"), tz_offset_secs),
        "accessed" => format_filetime_with_tz(vi(row, "accessed"), tz_offset_secs),
        "extension" => extract_extension(&vs(row, "name")),
        "drive" => vs(row, "drive"),
        "type" => if vb(row, "is_directory") {
            "dir"
        } else {
            "file"
        }
        .to_owned(),
        "descendants" => vu(row, "descendants").to_string(),
        "treesize" => vu(row, "treesize").to_string(),
        "tree_allocated" => vu(row, "tree_allocated").to_string(),
        "bulkiness" => {
            let is_dir = vb(row, "is_directory");
            let (logical, alloc) = if is_dir {
                (vu(row, "treesize"), vu(row, "tree_allocated"))
            } else {
                (vu(row, "size"), vu(row, "allocated"))
            };
            alloc
                .checked_mul(100)
                .and_then(|numerator| numerator.checked_div(logical))
                .unwrap_or(0)
                .to_string()
        }
        "name_length" => vs(row, "name").len().to_string(),
        "path_length" => vs(row, "path").len().to_string(),
        // Boolean flag columns
        "hidden" => flag_bit(flags, parity_flags::HIDDEN),
        "system" => flag_bit(flags, parity_flags::SYSTEM),
        "archive" => flag_bit(flags, parity_flags::ARCHIVE),
        "readonly" => flag_bit(flags, parity_flags::READONLY),
        "compressed" => flag_bit(flags, parity_flags::COMPRESSED),
        "encrypted" => flag_bit(flags, parity_flags::ENCRYPTED),
        "sparse" => flag_bit(flags, parity_flags::SPARSE),
        "reparse" => flag_bit(flags, parity_flags::REPARSE),
        "offline" => flag_bit(flags, parity_flags::OFFLINE),
        "not_indexed" => flag_bit(flags, parity_flags::NOT_INDEXED),
        "directory_flag" => flag_bit(flags, parity_flags::DIRECTORY),
        "integrity" => flag_bit(flags, parity_flags::INTEGRITY),
        "no_scrub" => flag_bit(flags, parity_flags::NO_SCRUB),
        "pinned" => flag_bit(flags, parity_flags::PINNED),
        "unpinned" => flag_bit(flags, parity_flags::UNPINNED),
        "recall_on_open" => flag_bit(flags, 0x0004_0000),
        "recall_on_data_access" => flag_bit(flags, 0x0040_0000),
        "temporary" => flag_bit(flags, 0x0100),
        "virtual" => flag_bit(flags, 0x0001_0000),
        "attributes" | "parity_attributes" => (flags & parity_flags::PARITY_MASK).to_string(),
        "attribute_value" | "flags" => flags.to_string(),
        // WI-4.4 forensic columns. The booleans render 0/1 in their own
        // column (like the attribute flags), read from the daemon-projected
        // JSON — never recomputed from `name`/`path` (those are the already-
        // lossy view, always valid UTF-8, which would always read 0/empty).
        "malformed" => if vb(row, "malformed") { "1" } else { "0" }.to_owned(),
        "malformed_path" => if vb(row, "malformed_path") { "1" } else { "0" }.to_owned(),
        "name_hex" => vs(row, "name_hex"),
        _ => String::new(),
    }
}

/// Format a flag bit as "1" or "0".
fn flag_bit(flags: u32, bit: u32) -> String {
    if flags & bit != 0 { "1" } else { "0" }.to_owned()
}

/// Extract file extension from a filename.
fn extract_extension(name: &str) -> String {
    name.rsplit_once('.')
        .map_or_else(String::new, |(_, ext)| ext.to_owned())
}

// ── Parity-compat output ──────────────────────────────────────────────

/// NTFS attribute flag constants (for parity boolean columns).
mod parity_flags {
    /// Read-only attribute.
    pub(super) const READONLY: u32 = 0x0001;
    /// Hidden attribute.
    pub(super) const HIDDEN: u32 = 0x0002;
    /// System attribute.
    pub(super) const SYSTEM: u32 = 0x0004;
    /// Directory attribute.
    pub(super) const DIRECTORY: u32 = 0x0010;
    /// Archive attribute.
    pub(super) const ARCHIVE: u32 = 0x0020;
    /// Sparse file attribute.
    pub(super) const SPARSE: u32 = 0x0200;
    /// Reparse point attribute.
    pub(super) const REPARSE: u32 = 0x0400;
    /// Compressed attribute.
    pub(super) const COMPRESSED: u32 = 0x0800;
    /// Offline attribute.
    pub(super) const OFFLINE: u32 = 0x1000;
    /// Not content-indexed attribute.
    pub(super) const NOT_INDEXED: u32 = 0x2000;
    /// Encrypted attribute.
    pub(super) const ENCRYPTED: u32 = 0x4000;
    /// Integrity stream attribute.
    pub(super) const INTEGRITY: u32 = 0x8000;
    /// No-scrub-data attribute.
    pub(super) const NO_SCRUB: u32 = 0x0002_0000;
    /// Pinned attribute.
    pub(super) const PINNED: u32 = 0x0008_0000;
    /// Unpinned attribute.
    pub(super) const UNPINNED: u32 = 0x0010_0000;
    /// Parity mask — the 15 attribute bits tracked by the legacy baseline.
    pub(super) const PARITY_MASK: u32 = READONLY
        | HIDDEN
        | SYSTEM
        | DIRECTORY
        | ARCHIVE
        | SPARSE
        | REPARSE
        | COMPRESSED
        | OFFLINE
        | NOT_INDEXED
        | ENCRYPTED
        | INTEGRITY
        | NO_SCRUB
        | PINNED
        | UNPINNED;
}

/// Local timezone offset in seconds, computed once at startup.
///
/// Matches C++ behavior where `FileTimeToLocalFileTime()` uses the
/// CURRENT timezone offset for ALL timestamps, ignoring historical
/// DST transitions.
///
/// Uses platform APIs (no chrono dependency) via `uffs-client`.
static LOCAL_TZ_OFFSET_SECS: std::sync::LazyLock<i32> =
    std::sync::LazyLock::new(uffs_client::format::local_utc_offset_secs);

/// Format a raw FILETIME into `YYYY-MM-DD HH:MM:SS` with the supplied
/// timezone offset.
///
/// Mirrors `uffs_format::append_datetime_native` exactly, including
/// the `"0000-00-00 00:00:00"` sentinel for a zero FILETIME (which
/// `filetime_to_calendar` returns `None` for).  `write_columnar`
/// calls this via `extract_field` with the config-supplied TZ so the
/// CLI's fallback path and the daemon's `try_pack_csv_blob`
/// pre-formatted blob produce byte-identical output.
fn format_filetime_with_tz(filetime: i64, tz_offset_secs: i32) -> String {
    let local_ft = uffs_time::filetime_with_tz_bias(filetime, tz_offset_secs);
    match uffs_time::filetime_to_calendar(local_ft) {
        Some(uffs_time::CalendarParts {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }) => format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"),
        None => "0000-00-00 00:00:00".to_owned(),
    }
}

/// Format a raw FILETIME into `YYYY-MM-DD HH:MM:SS` using the host's
/// local timezone.
///
/// Used by [`write_table`] for human-facing display where the user
/// expects their own wall-clock time regardless of how the CSV
/// fallback path was configured.  CSV / parity / custom formatters
/// take their offset from the config (`--tz-offset`) via
/// [`format_filetime_with_tz`] instead.
///
/// `pub(crate)` so the forensic `--deleted` command reuses the exact same
/// wall-clock rendering as the search table.
pub(crate) fn format_filetime_local(filetime: i64) -> String {
    format_filetime_with_tz(filetime, *LOCAL_TZ_OFFSET_SECS)
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
