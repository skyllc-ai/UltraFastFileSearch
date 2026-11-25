// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for output helpers.
//!
//! All tests exercise the unified `write_native_results` path using
//! `serde_json::Value` inputs — no polars, no typed protocol structs.

use core::time::Duration;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::json;

use super::{
    ConsoleWriteStrategy, MULTICOL_AVG_BYTES_PER_ROW, MULTICOL_BUFFER_CAP_BYTES,
    choose_console_strategy, write_native_results,
};

type TestResult = Result<()>;

fn temp_output_path(extension: &str) -> PathBuf {
    use core::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "uffs-cli-output-contract-{}-{nanos}-{seq}.{extension}",
        std::process::id()
    ))
}

/// A single row as JSON Value matching the old `sample_df()` content.
fn sample_rows() -> Vec<serde_json::Value> {
    vec![json!({
        "drive": "C",
        "path": "C:\\Temp\\file.txt",
        "name": "file.txt",
        "size": 123,
        "is_directory": false,
        "modified": 1_700_001_100_000_000_i64,
        "created": 1_700_001_000_000_000_i64,
        "accessed": 1_700_001_200_000_000_i64,
        "flags": 0,
        "allocated": 128,
        "descendants": 0,
        "treesize": 0,
        "tree_allocated": 0,
    })]
}

/// 20 000+ rows for testing the slow-scan footer guard.
fn large_sample_rows() -> Vec<serde_json::Value> {
    (0..20_000_u64)
        .map(|idx| {
            json!({
                "drive": "C",
                "path": format!("C:\\Temp\\file{idx}.txt"),
                "name": format!("file{idx}.txt"),
                "size": 100_u64,
                "is_directory": false,
                "modified": 0_i64,
                "created": 0_i64,
                "accessed": 0_i64,
                "flags": 0_u32,
                "allocated": 128_u64,
                "descendants": 0_u32,
                "treesize": 0_u64,
                "tree_allocated": 0_u64,
            })
        })
        .collect()
}

// ===================================================================
// write_native_results contract tests
// ===================================================================

#[test]
fn write_native_csv_uses_columns_without_legacy_footer() -> TestResult {
    let path = temp_output_path("csv");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "csv",
        &path.to_string_lossy(),
        "path,name",
        ";",
        "'",
        false,
        "1",
        "0",
        None,
        &[
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D,
        ],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert_eq!(written, "'C:\\Temp\\file.txt';'file.txt'\n");
    Ok(())
}

#[test]
fn write_native_custom_file_appends_legacy_drive_footer() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &[
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D,
        ],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    // With glob pattern "*.txt", few results is expected — no MMMmmm warning.
    assert_eq!(
        written,
        concat!(
            "\"C:\\Temp\\file.txt\",\"file.txt\"\n",
            "\r\n",
            "\r\n",
            "Drives? \t2\tC:|D:\r\n",
            "\r\n",
        )
    );
    Ok(())
}

#[test]
fn write_native_json_file_has_no_footer() -> TestResult {
    let path = temp_output_path("json");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "json",
        &path.to_string_lossy(),
        "path,name",
        "\t",
        "",
        false,
        "1",
        "0",
        None,
        &[
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D,
        ],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(!written.contains("Drives?"));
    assert!(written.contains("C:\\\\Temp\\\\file.txt"));
    Ok(())
}

// ===================================================================
// Legacy footer tests (via write_native_results in custom format)
// ===================================================================

#[test]
fn legacy_footer_includes_fast_scan_message_for_full_scan_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &[uffs_mft::platform::DriveLetter::G],
        Duration::from_millis(999),
        "*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(written.contains("Drives? \t1\tG:"));
    assert!(written.contains("MMMmmm that was FAST"));
    Ok(())
}

#[test]
fn legacy_footer_includes_fast_scan_for_transformed_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &[uffs_mft::platform::DriveLetter::G],
        Duration::from_millis(999),
        ">G:.*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(written.contains("Drives? \t1\tG:"));
    assert!(written.contains("MMMmmm that was FAST"));
    Ok(())
}

#[test]
fn legacy_footer_omits_fast_scan_for_real_regex_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &[uffs_mft::platform::DriveLetter::G],
        Duration::from_millis(999),
        r">G:.*\.(jpg|png)",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(written.contains("Drives? \t1\tG:"));
    assert!(!written.contains("MMMmmm"));
    Ok(())
}

#[test]
fn legacy_footer_omits_fast_scan_message_when_many_results() -> TestResult {
    let path = temp_output_path("txt");
    let rows = large_sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &[uffs_mft::platform::DriveLetter::G],
        Duration::from_secs(2),
        ">G:.*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    // Should NOT contain fast-scan message (row_count >= 20,000)
    assert!(written.contains("Drives? \t1\tG:"));
    assert!(!written.contains("MMMmmm"));
    Ok(())
}

// ── Phase 3.2: single-buffer multi-column console render ────────────

/// Tiny row counts fit comfortably under the cap → `SingleBuffer`.
/// This is the common case — any interactive query picks this branch.
#[test]
fn choose_console_strategy_small_result_uses_single_buffer() {
    // 100 rows × 256 B = 25 KB — well under the 50 MB cap.
    assert_eq!(
        choose_console_strategy(100, MULTICOL_BUFFER_CAP_BYTES, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );
}

/// The 45K-row benchmark baseline stays on the fast path — locks in
/// the expected behaviour for the Phase 2 Run 11 workload.
#[test]
fn choose_console_strategy_benchmark_row_count_uses_single_buffer() {
    // 45_000 × 256 B ≈ 11 MB — still well under 50 MB.
    assert_eq!(
        choose_console_strategy(
            45_000,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::SingleBuffer
    );
}

/// Pathologically large result sets flip to streaming so peak RSS
/// stays bounded.  With the production constants, the threshold is
/// roughly 200K rows (50 MB / 256 B).
#[test]
fn choose_console_strategy_huge_result_falls_back_to_streaming() {
    // 1_000_000 × 256 B = 256 MB — 5× over the cap.
    assert_eq!(
        choose_console_strategy(
            1_000_000,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::Streaming
    );
}

/// Boundary case: `row_count × est == cap` is inclusive of the buffer
/// path (`<=`).  Exactly-at-cap inputs render in one buffer.
#[test]
fn choose_console_strategy_exactly_at_cap_uses_single_buffer() {
    // row_count chosen so row_count × 256 == 50 MB exactly.
    let exact = MULTICOL_BUFFER_CAP_BYTES / MULTICOL_AVG_BYTES_PER_ROW;
    assert_eq!(
        choose_console_strategy(exact, MULTICOL_BUFFER_CAP_BYTES, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );

    // One more row tips over — streaming.
    assert_eq!(
        choose_console_strategy(
            exact + 1,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::Streaming
    );
}

/// Overflow guard: `usize::MAX × 256` saturates, so the decision must
/// not silently wrap and misclassify as `SingleBuffer`.  A regression
/// here would mean catastrophic allocation attempts for attacker-
/// controlled pagination cursors.
#[test]
fn choose_console_strategy_saturates_on_overflow() {
    assert_eq!(
        choose_console_strategy(
            usize::MAX,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::Streaming
    );
}

/// A zero-byte cap forces every non-empty result onto the streaming
/// path — useful for tests that want to exercise the fallback without
/// generating millions of synthetic rows.
#[test]
fn choose_console_strategy_tiny_cap_forces_streaming() {
    // 1 row × 256 B > 0 B → Streaming.
    assert_eq!(
        choose_console_strategy(1, 0, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::Streaming
    );
    // 0 rows stays on SingleBuffer even with a zero cap — `0 <= 0`.
    assert_eq!(
        choose_console_strategy(0, 0, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );
}

// ============================================================================
// CLI ↔ uffs_format byte-parity regression tests
// ============================================================================
//
// Phase 3 (v0.5.64) lifts the `try_pack_csv_blob` gate to cover
// `--columns parity` / `--parity-compat` and `--format custom`.  For
// that lift to be safe, the CLI's hand-rolled formatters
// (`write_parity`, `write_columnar`) must produce **byte-identical**
// output to the daemon's `uffs_format::write_rows` path for every
// supported configuration — otherwise the same query could emit
// different bytes depending on whether pre-format kicked in.
//
// Each test below builds a `SearchRow` fixture, serialises it to
// `serde_json::Value` for the CLI path, pushes both sides through
// their respective writers, and asserts byte equality.  If the two
// ever drift the failure message prints the diffed text so the
// regression is easy to localise.

/// Build a `SearchRow` fixture the byte-parity tests can feed to both
/// pipelines.  Kept alongside the tests (not in a shared helper) so
/// each test can tweak individual fields without a builder-knob API.
fn parity_row(
    path: &str,
    name: &str,
    is_directory: bool,
    flags: u32,
    modified_filetime: i64,
) -> uffs_client::protocol::response::SearchRow {
    uffs_client::protocol::response::SearchRow {
        drive: uffs_mft::platform::DriveLetter::C,
        path: path.to_owned(),
        name: name.to_owned(),
        size: 4321,
        is_directory,
        modified: modified_filetime,
        created: modified_filetime,
        accessed: modified_filetime,
        flags,
        allocated: 8192,
        descendants: 0,
        treesize: 0,
        tree_allocated: 0,
        malformed: false,
        malformed_path: false,
        name_hex: None,
        file_reference: 0,
    }
}

/// Serialise a slice of `SearchRow` to the JSON-value shape the CLI's
/// `write_parity` / `write_columnar` consume.  Uses
/// `serde_json::to_value` so every wire field round-trips identically
/// to what a live daemon response carries.
fn rows_to_values(rows: &[uffs_client::protocol::response::SearchRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| serde_json::to_value(row).expect("SearchRow must serialise"))
        .collect()
}

/// Helper: run the CLI `write_parity` and `uffs_format::write_rows`
/// (parity column order + `parity_compat=true`) on the same fixture
/// and assert byte equality.  Returns the shared bytes so callers can
/// pin further invariants (header length, row count, etc.).
fn assert_parity_bytes_match(
    rows: &[uffs_client::protocol::response::SearchRow],
    tz_offset_secs: i32,
) -> Vec<u8> {
    // ── CLI path ──────────────────────────────────────────────
    let json_rows = rows_to_values(rows);
    let parity_ctx = super::ParityContext {
        pos: "1",
        neg: "0",
        tz_offset_secs,
    };
    let mut cli_bytes = Vec::new();
    super::parity::write_parity(&mut cli_bytes, &json_rows, ",", "\"", &parity_ctx)
        .expect("CLI write_parity must succeed");

    // ── uffs_format path ──────────────────────────────────────
    let tz_hours = tz_offset_secs / 3_600_i32;
    let fmt_cfg = uffs_format::OutputConfig::new()
        .with_columns("parity")
        .with_header(true)
        .with_separator(",")
        .with_quote("\"")
        .with_pos("1")
        .with_neg("0")
        .with_tz_offset_hours(tz_hours)
        .with_parity_compat(true);
    let mut fmt_bytes = Vec::new();
    uffs_format::write_rows(&fmt_cfg, rows, &mut fmt_bytes)
        .expect("uffs_format::write_rows must succeed");

    assert_eq!(
        cli_bytes,
        fmt_bytes,
        "CLI write_parity and uffs_format::write_rows(parity_cfg) must emit \
         byte-identical output — a drift here means the daemon's \
         try_pack_csv_blob fast path would produce different bytes than \
         the CLI's local formatter.\n\nCLI bytes:\n{cli_text}\n\n\
         uffs_format bytes:\n{fmt_text}",
        cli_text = String::from_utf8_lossy(&cli_bytes),
        fmt_text = String::from_utf8_lossy(&fmt_bytes),
    );
    cli_bytes
}

/// Parity basic: single file row with zero filetimes.  Pins the
/// datetime-sentinel alignment between `append_datetime_tz` (CLI)
/// and `append_datetime_native` (uffs-format) — both must emit
/// `"0000-00-00 00:00:00"` when `filetime_to_calendar` returns `None`.
#[test]
fn parity_byte_parity_basic_file_zero_filetime() {
    let rows = vec![parity_row(
        "C:\\Temp\\readme.txt",
        "readme.txt",
        false,
        0x0022, // HIDDEN | ARCHIVE
        0,
    )];
    let bytes = assert_parity_bytes_match(&rows, 0);
    // Sanity: the canonical parity header is 25 columns + trailing
    // blank separator line, so byte count is well above zero.
    assert!(!bytes.is_empty(), "expected a non-empty parity blob");
    assert!(
        String::from_utf8_lossy(&bytes).contains("0000-00-00 00:00:00"),
        "zero-filetime must render as the zero sentinel"
    );
}

/// Parity directory: exercises the `parity_dir` branch that rewrites
/// `Path` (trailing `\`), empties `Name`, moves the path into
/// `PathOnly`, and swaps `Size`/`SizeOnDisk` to `treesize`/
/// `tree_allocated`.
#[test]
fn parity_byte_parity_directory_rewrite() {
    let mut row = parity_row(
        "D:\\Users\\alice\\Docs",
        "Docs",
        true,
        0x0010,                      // DIRECTORY
        133_485_408_000_000_000_i64, // 2024-01-01 UTC
    );
    row.drive = uffs_mft::platform::DriveLetter::D;
    row.size = 0;
    row.allocated = 0;
    row.treesize = 65_536;
    row.tree_allocated = 73_728;
    row.descendants = 12;
    assert_parity_bytes_match(&[row], 0);
}

/// Parity mixed flags: every bit of `PARITY_MASK` toggled on, plus
/// a non-zero filetime — pins the 15-column flag dispatch and the
/// `ParityAttributes` final column both render identically.
#[test]
fn parity_byte_parity_all_flag_bits() {
    // All 15 parity-mask bits set: READONLY|HIDDEN|SYSTEM|DIRECTORY|
    // ARCHIVE|SPARSE|REPARSE|COMPRESSED|OFFLINE|NOT_INDEXED|
    // ENCRYPTED|INTEGRITY|NO_SCRUB|PINNED|UNPINNED.
    let flags: u32 = 0x0001
        | 0x0002
        | 0x0004
        | 0x0010
        | 0x0020
        | 0x0200
        | 0x0400
        | 0x0800
        | 0x1000
        | 0x2000
        | 0x4000
        | 0x8000
        | 0x0002_0000
        | 0x0008_0000
        | 0x0010_0000;
    let rows = vec![parity_row(
        "C:\\System\\strange.bin",
        "strange.bin",
        false,
        flags,
        133_485_408_000_000_000_i64,
    )];
    assert_parity_bytes_match(&rows, -8 * 3600); // PST
}

/// Parity multi-row: two files with distinct paths.  Pins row
/// ordering + the separator line between header and data.
#[test]
fn parity_byte_parity_multi_row() {
    let rows = vec![
        parity_row("C:\\a.txt", "a.txt", false, 0x0020, 0),
        parity_row("C:\\b.log", "b.log", false, 0x0022, 0),
    ];
    assert_parity_bytes_match(&rows, 0);
}

/// Helper: run CLI `write_columnar` and `uffs_format::write_rows` on
/// the same fixture and assert byte equality.  Mirrors
/// [`assert_parity_bytes_match`] but for the non-parity multi-column
/// projection path — pins that
/// 1. the datetime-sentinel alignment works end-to-end,
/// 2. `write_columnar`'s quote policy (string columns only) matches
///    `uffs_format`, and
/// 3. both paths terminate the header with `\n\n` (legacy baseline).
fn assert_columnar_bytes_match(
    rows: &[uffs_client::protocol::response::SearchRow],
    columns: &str,
    tz_offset_hours: i32,
) -> Vec<u8> {
    // ── CLI path ──────────────────────────────────────────────
    let json_rows = rows_to_values(rows);
    let parity_ctx = super::ParityContext {
        pos: "1",
        neg: "0",
        tz_offset_secs: tz_offset_hours.saturating_mul(3_600_i32),
    };
    let mut cli_bytes = Vec::new();
    super::write_columnar(
        &mut cli_bytes,
        &json_rows,
        columns,
        ",",
        "\"",
        true,
        &parity_ctx,
    )
    .expect("CLI write_columnar must succeed");

    // ── uffs_format path ──────────────────────────────────────
    let fmt_cfg = uffs_format::OutputConfig::new()
        .with_columns(columns)
        .with_header(true)
        .with_separator(",")
        .with_quote("\"")
        .with_pos("1")
        .with_neg("0")
        .with_tz_offset_hours(tz_offset_hours);
    let mut fmt_bytes = Vec::new();
    uffs_format::write_rows(&fmt_cfg, rows, &mut fmt_bytes)
        .expect("uffs_format::write_rows must succeed");

    assert_eq!(
        cli_bytes,
        fmt_bytes,
        "CLI write_columnar and uffs_format::write_rows must emit \
         byte-identical output — a drift here means the CLI fallback \
         path would show different bytes than the daemon's pre-formatted \
         blob on the same query.\n\nCLI bytes:\n{cli_text}\n\n\
         uffs_format bytes:\n{fmt_text}",
        cli_text = String::from_utf8_lossy(&cli_bytes),
        fmt_text = String::from_utf8_lossy(&fmt_bytes),
    );
    fmt_bytes
}

/// Columnar zero-filetime: pins the datetime-sentinel fix — both
/// paths must emit `"0000-00-00 00:00:00"` for a zero FILETIME now
/// that `format_filetime_local` in the CLI matches
/// `append_datetime_native` in uffs-format.
#[test]
fn columnar_byte_parity_zero_filetime_date_columns() {
    let rows = vec![parity_row(
        "C:\\data\\empty_time.txt",
        "empty_time.txt",
        false,
        0x0020,
        0,
    )];
    let fmt_bytes = assert_columnar_bytes_match(&rows, "name,modified,created,accessed", 0);
    assert!(
        String::from_utf8_lossy(&fmt_bytes).contains("0000-00-00 00:00:00"),
        "zero-filetime must render as the zero sentinel in uffs_format output too"
    );
}

/// Columnar non-zero filetime: pins normal date formatting agreement
/// between the CLI and uffs-format paths.
#[test]
fn columnar_byte_parity_nonzero_filetime() {
    let rows = vec![parity_row(
        "C:\\data\\real.txt",
        "real.txt",
        false,
        0x0020,
        133_485_408_000_000_000_i64, // 2024-01-01 UTC
    )];
    assert_columnar_bytes_match(&rows, "name,modified", 0);
}

/// Without `--columns` the CLI's CSV writer emits every standard column, in
/// the order the daemon's CSV writer uses (`uffs_format`'s
/// `BASELINE_COLUMN_ORDER`), so a CSV has one shape whichever side wrote it.
#[test]
fn default_columns_are_the_daemon_baseline() {
    let expected: Vec<&'static str> = uffs_format::BASELINE_COLUMN_ORDER
        .iter()
        .map(|column| {
            super::resolve_col_name(column.canonical_name()).unwrap_or_else(|| {
                panic!(
                    "baseline column {} has no CLI name",
                    column.canonical_name()
                )
            })
        })
        .collect();
    assert_eq!(super::resolve_columns(""), expected);
    assert_eq!(super::resolve_columns("all"), expected);
}
