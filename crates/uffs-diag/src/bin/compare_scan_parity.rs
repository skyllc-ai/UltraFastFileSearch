// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Comprehensive reference-output vs Rust scan parity comparison tool.
//!
//! Exception: Standalone diagnostic CLI binary with complex parity output
//! formatting. Not library code - splitting would reduce tool readability.
//!
//! This tool performs deep comparison of UFFS scan outputs to verify that
//! the Rust implementation produces identical results to the reference output.
//!
//! # Usage
//!
//! ```bash
//! # Compare trial_run.ps1 outputs (Windows)
//! uffs-compare-scan-parity baseline_c.txt rust_reference_full_c.txt
//!
//! # Compare with report output
//! uffs-compare-scan-parity baseline_c.txt rust_reference_full_c.txt --report parity_report.md
//! ```

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::use_debug,
    reason = "diagnostic tool — stdout/stderr/debug output is intentional"
)]
#![expect(
    clippy::too_many_lines,
    reason = "diagnostic analysis functions are inherently long sequential pipelines"
)]
#![expect(
    clippy::float_arithmetic,
    clippy::default_numeric_fallback,
    reason = "statistical comparison tool — arithmetic casts are pervasive and reviewed"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "indices are validated by HashMap lookup or bounds-checked iterators"
)]
#![expect(
    clippy::single_call_fn,
    reason = "functions factored for readability in this analysis tool"
)]
#![expect(
    clippy::shadow_reuse,
    reason = "idiomatic DataFrame transformation pipeline"
)]
#![expect(
    clippy::str_to_string,
    reason = "string operations on normalized ASCII paths are safe"
)]
#![expect(
    clippy::min_ident_chars,
    reason = "short names (f, c, r, i) are conventional in statistical code"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "iteration order is irrelevant for statistical aggregation"
)]
#![expect(
    clippy::option_if_let_else,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args,
    reason = "style lints — current form is clearer in this context"
)]

// This target links its package's full dependency list but uses only
// part of it.  Naming the rest keeps `unused_crate_dependencies` exact on
// every toolchain (newer nightlies stopped reporting it for bench, bin and
// test targets; the pinned one still does), with no lint suppression.
use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use hex as _;
use rayon::prelude::*;
use sha2 as _;
use uffs_diag::parity::stats::{ComparisonResults, FieldStats};
use uffs_polars::{CsvReadOptions, DataFrame, SerReader as _, StringChunked};

// ============================================================================
// Column Name Mappings (reference output <-> Rust)
// ============================================================================

/// Map reference column names to normalized internal names
#[expect(dead_code, reason = "utility for future column normalization")]
fn normalize_column_name(name: &str) -> &'static str {
    match name.to_lowercase().replace(' ', "_").as_str() {
        "path" => "path",
        "name" => "name",
        "path_only" | "pathonly" => "path_only",
        "size" => "size",
        "size_on_disk" | "sizeondisk" | "allocated_size" => "allocated_size",
        "created" => "created",
        "last_written" | "written" | "modified" => "modified",
        "last_accessed" | "accessed" => "accessed",
        "descendants" | "decendents" => "descendants", // legacy typo
        "treesize" | "tree_size" => "treesize",
        "tree_allocated" | "treeallocated" => "tree_allocated",
        "directory_flag" | "directoryflag" | "is_directory" => "is_directory",
        "hidden" | "is_hidden" => "is_hidden",
        "system" | "is_system" => "is_system",
        "archive" | "is_archive" => "is_archive",
        "read-only" | "readonly" | "is_readonly" => "is_readonly",
        "compressed" | "is_compressed" => "is_compressed",
        "encrypted" | "is_encrypted" => "is_encrypted",
        "sparse" | "is_sparse" => "is_sparse",
        "reparse" | "is_reparse" => "is_reparse",
        "offline" | "is_offline" => "is_offline",
        "attributes" | "flags" => "flags",
        "type" => "type",
        "frs" => "frs",
        "parent_frs" => "parent_frs",
        _ => "unknown",
    }
}

// ============================================================================
// CSV Loading
// ============================================================================

/// Load CSV with flexible parsing (handles both reference and Rust formats)
fn load_csv(path: &Path, label: &str) -> Result<DataFrame> {
    println!("Loading {label}: {}", path.display());

    let df = CsvReadOptions::default()
        .with_has_header(true)
        .with_infer_schema_length(Some(10000))
        .with_ignore_errors(true)
        .map_parse_options(|opts| opts.with_truncate_ragged_lines(true))
        .try_into_reader_with_file_path(Some(path.into()))?
        .finish()
        .with_context(|| format!("Failed to read CSV: {}", path.display()))?;

    println!("  Loaded {} rows, {} columns", df.height(), df.width());
    println!("  Columns: {:?}", df.get_column_names());
    Ok(df)
}

/// Normalize paths for comparison (lowercase, forward slashes, trim trailing
/// slash)
fn normalize_path(path: &str) -> String {
    path.to_lowercase()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

/// Add normalized path column to `DataFrame`
fn add_normalized_paths(df: &DataFrame) -> Result<DataFrame> {
    let path_col = df.column("Path").or_else(|_| df.column("path"))?;
    let path_str = path_col.str()?;

    let normalized: StringChunked = path_str.iter().map(|opt| opt.map(normalize_path)).collect();

    let mut result = df.clone();
    result.with_column(uffs_polars::Column::new(
        "path_norm".into(),
        normalized.into_series(),
    ))?;
    Ok(result)
}

/// Check if path is an Alternate Data Stream: a `:` after the drive
/// prefix (`C:`).  `get` instead of slicing, so a multi-byte character at
/// byte 2 reads as "no stream" rather than panicking.
fn is_ads_path(path: &str) -> bool {
    path.get(2..).is_some_and(|rest| rest.contains(':'))
}

/// Extract drive letter from path
#[expect(dead_code, reason = "utility for future drive-level analysis")]
fn extract_drive(path: &str) -> Option<char> {
    path.chars().next().filter(char::is_ascii_alphabetic)
}

// ============================================================================
// Comparison Logic
// ============================================================================

/// Build path-to-row-index map for fast lookups
fn build_path_map(df: &DataFrame) -> Result<HashMap<String, usize>> {
    let path_col = df.column("path_norm")?.str()?;
    let mut map = HashMap::with_capacity(df.height());

    for (idx, opt_path) in path_col.iter().enumerate() {
        if let Some(path) = opt_path {
            map.insert(path.to_string(), idx);
        }
    }
    Ok(map)
}

/// Get string value from `DataFrame` at row index
#[expect(dead_code, reason = "utility for future field comparison")]
fn get_str_value(df: &DataFrame, col: &str, idx: usize) -> Option<String> {
    df.column(col)
        .ok()
        .and_then(|c| c.str().ok())
        .and_then(|s| s.get(idx).map(String::from))
}

/// Get `u64` value from `DataFrame` at row index
fn get_u64_value(df: &DataFrame, col: &str, idx: usize) -> Option<u64> {
    df.column(col).ok().and_then(|c| {
        if let Ok(u) = c.u64() {
            u.get(idx)
        } else if let Ok(i) = c.i64() {
            i.get(idx).map(uffs_mft::nonneg_to_u64)
        } else if let Ok(s) = c.str() {
            s.get(idx).and_then(|v| v.parse().ok())
        } else {
            None
        }
    })
}

/// Get `bool` value from `DataFrame` at row index (handles 0/1 and true/false)
fn get_bool_value(df: &DataFrame, col: &str, idx: usize) -> Option<bool> {
    df.column(col).ok().and_then(|c| {
        if let Ok(b) = c.bool() {
            b.get(idx)
        } else if let Ok(i) = c.i64() {
            i.get(idx).map(|v| v != 0)
        } else if let Ok(u) = c.u64() {
            u.get(idx).map(|v| v != 0)
        } else if let Ok(s) = c.str() {
            s.get(idx).map(|v| v == "1" || v.to_lowercase() == "true")
        } else {
            None
        }
    })
}

use uffs_polars::IntoSeries as _;

/// Compare numeric field and update stats
fn compare_numeric_field(
    stats: &mut FieldStats,
    path: &str,
    reference_val: Option<u64>,
    rust_val: Option<u64>,
) {
    match (reference_val, rust_val) {
        (Some(c), Some(r)) => {
            stats.total_compared += 1;
            if c == r {
                stats.exact_matches += 1;
            } else {
                stats.mismatches += 1;
                // `u64::abs_diff` returns the unsigned magnitude
                // `|c - r|` directly, replacing the previous
                // `wrapping_sub(...).unsigned_abs()` chain (which
                // required a `cast_possible_wrap` expect on every
                // `as i64` cast).
                let diff = uffs_mft::u64_to_f64(c.abs_diff(r));
                stats.sum_abs_diff += diff;
                if diff > stats.max_abs_diff {
                    stats.max_abs_diff = diff;
                }
                if stats.diff_samples.len() < 10 {
                    stats
                        .diff_samples
                        .push((path.to_string(), c.to_string(), r.to_string()));
                }
            }
        }
        (Some(_), None) => stats.reference_only += 1,
        (None, Some(_)) => stats.rust_only += 1,
        (None, None) => {}
    }
}

/// Compare boolean field and update stats
fn compare_bool_field(
    stats: &mut FieldStats,
    path: &str,
    reference_val: Option<bool>,
    rust_val: Option<bool>,
) {
    match (reference_val, rust_val) {
        (Some(c), Some(r)) => {
            stats.total_compared += 1;
            if c == r {
                stats.exact_matches += 1;
            } else {
                stats.mismatches += 1;
                if stats.diff_samples.len() < 10 {
                    stats
                        .diff_samples
                        .push((path.to_string(), c.to_string(), r.to_string()));
                }
            }
        }
        (Some(_), None) => stats.reference_only += 1,
        (None, Some(_)) => stats.rust_only += 1,
        (None, None) => {}
    }
}

/// Perform full comparison between the reference and Rust `DataFrame`s
fn compare_dataframes(
    reference_df: &DataFrame,
    rust_df: &DataFrame,
    reference_file: &str,
    rust_file: &str,
) -> Result<ComparisonResults> {
    let mut results = ComparisonResults {
        reference_file: reference_file.to_string(),
        rust_file: rust_file.to_string(),
        reference_total_rows: reference_df.height(),
        rust_total_rows: rust_df.height(),
        ..Default::default()
    };

    // Add normalized paths
    let reference_df = add_normalized_paths(reference_df)?;
    let rust_df = add_normalized_paths(rust_df)?;

    // Build path maps
    let reference_paths = build_path_map(&reference_df)?;
    let rust_paths = build_path_map(&rust_df)?;

    // Path set analysis
    let reference_path_set: HashSet<_> = reference_paths.keys().cloned().collect();
    let rust_path_set: HashSet<_> = rust_paths.keys().cloned().collect();

    let common: HashSet<_> = reference_path_set
        .intersection(&rust_path_set)
        .cloned()
        .collect();
    let reference_only: Vec<_> = reference_path_set
        .difference(&rust_path_set)
        .cloned()
        .collect();
    let rust_only: Vec<_> = rust_path_set
        .difference(&reference_path_set)
        .cloned()
        .collect();

    results.common_paths = common.len();
    results.reference_only_paths = reference_only.len();
    results.rust_only_paths = rust_only.len();
    results.path_match_rate = if reference_path_set.is_empty() {
        0.0
    } else {
        100.0 * uffs_mft::usize_to_f64(common.len())
            / uffs_mft::usize_to_f64(reference_path_set.len())
    };

    // Sample missing paths
    results.sample_reference_only = reference_only.iter().take(20).cloned().collect();
    results.sample_rust_only = rust_only.iter().take(20).cloned().collect();

    // ADS analysis
    results.reference_ads_count = reference_path_set.iter().filter(|p| is_ads_path(p)).count();
    results.rust_ads_count = rust_path_set.iter().filter(|p| is_ads_path(p)).count();

    // Initialize field stats
    let numeric_fields = [
        "size",
        "allocated_size",
        "descendants",
        "treesize",
        "tree_allocated",
    ];
    let bool_fields = [
        "is_directory",
        "is_hidden",
        "is_system",
        "is_archive",
        "is_readonly",
        "is_compressed",
        "is_encrypted",
        "is_sparse",
        "is_reparse",
    ];

    for field in numeric_fields.iter().chain(bool_fields.iter()) {
        results
            .field_stats
            .insert((*field).to_string(), FieldStats::default());
    }

    // Column name mappings for reference output -> internal
    let reference_col_map: HashMap<&str, &str> = [
        ("Size", "size"),
        ("Size on Disk", "allocated_size"),
        ("Descendants", "descendants"),
        ("Directory Flag", "is_directory"),
        ("Hidden", "is_hidden"),
        ("System", "is_system"),
        ("Archive", "is_archive"),
        ("Read-only", "is_readonly"),
        ("Compressed", "is_compressed"),
        ("Encrypted", "is_encrypted"),
        ("Sparse", "is_sparse"),
        ("Reparse", "is_reparse"),
    ]
    .into_iter()
    .collect();

    // Find actual column names in DataFrames (convert PlSmallStr to String for
    // lookup)
    let reference_cols: HashSet<String> = reference_df
        .get_column_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let _rust_cols: HashSet<String> = rust_df
        .get_column_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    // Compare fields for common paths using parallel iteration
    println!(
        "\nComparing {} common paths (using {} threads)...",
        common.len(),
        rayon::current_num_threads()
    );
    let total = common.len();

    // Convert to Vec for parallel iteration
    let common_paths: Vec<_> = common.into_iter().collect();

    // Atomic progress counter for reporting
    let progress = AtomicUsize::new(0);

    // Pre-compute column mappings for each field to avoid repeated lookups
    // Both files use the same display column names (e.g., "Size", "Size on
    // Disk"). Tuple is (internal_field_name, reference_col, rust_col).
    let numeric_col_mappings: Vec<(&str, &str, &str)> = numeric_fields
        .iter()
        .map(|&field| {
            let display_col = reference_col_map
                .iter()
                .find(|(_, v)| **v == field)
                .map(|(k, _)| *k)
                .filter(|c| reference_cols.contains(*c))
                .unwrap_or(field);
            (field, display_col, display_col)
        })
        .collect();

    let bool_col_mappings: Vec<(&str, &str, &str)> = bool_fields
        .iter()
        .map(|&field| {
            let display_col = reference_col_map
                .iter()
                .find(|(_, v)| **v == field)
                .map(|(k, _)| *k)
                .filter(|c| reference_cols.contains(*c))
                .unwrap_or(field);
            (field, display_col, display_col)
        })
        .collect();

    // Parallel comparison with thread-local stats, then reduce
    let all_field_stats: HashMap<String, FieldStats> = common_paths
        .par_iter()
        .fold(
            || {
                // Thread-local accumulator: HashMap of field stats
                let mut local_stats: HashMap<String, FieldStats> = HashMap::new();
                for field in numeric_fields.iter().chain(bool_fields.iter()) {
                    local_stats.insert((*field).to_string(), FieldStats::default());
                }
                local_stats
            },
            |mut local_stats, path| {
                // Progress reporting (atomic, occasional)
                let current = progress.fetch_add(1, Ordering::Relaxed);
                if current.is_multiple_of(500_000) && current > 0 {
                    println!(
                        "  Progress: {}/{} ({:.1}%)",
                        current,
                        total,
                        100.0 * uffs_mft::usize_to_f64(current) / uffs_mft::usize_to_f64(total)
                    );
                }

                let reference_idx = reference_paths[path];
                let rust_idx = rust_paths[path];

                // Compare numeric fields
                for (field, reference_col, rust_col) in &numeric_col_mappings {
                    let reference_val = get_u64_value(&reference_df, reference_col, reference_idx);
                    let rust_val = get_u64_value(&rust_df, rust_col, rust_idx);

                    if let Some(stats) = local_stats.get_mut(*field) {
                        compare_numeric_field(stats, path, reference_val, rust_val);
                    }
                }

                // Compare boolean fields
                for (field, reference_col, rust_col) in &bool_col_mappings {
                    let reference_val = get_bool_value(&reference_df, reference_col, reference_idx);
                    let rust_val = get_bool_value(&rust_df, rust_col, rust_idx);

                    if let Some(stats) = local_stats.get_mut(*field) {
                        compare_bool_field(stats, path, reference_val, rust_val);
                    }
                }

                local_stats
            },
        )
        .reduce(
            || {
                // Identity element for reduce
                let mut stats: HashMap<String, FieldStats> = HashMap::new();
                for field in numeric_fields.iter().chain(bool_fields.iter()) {
                    stats.insert((*field).to_string(), FieldStats::default());
                }
                stats
            },
            |mut acc, local| {
                // Merge thread-local stats into accumulator
                for (field, local_stats) in local {
                    if let Some(acc_stats) = acc.get_mut(&field) {
                        acc_stats.merge(local_stats);
                    }
                }
                acc
            },
        );

    results.field_stats = all_field_stats;
    println!("  Progress: {}/{} (100.0%)", total, total);

    Ok(results)
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> Result<()> {
    uffs_version::handle_version!("uffs-compare-scan-parity");
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!(
            "Usage: uffs-compare-scan-parity <reference_output.txt> <rust_output.txt> [--report <file.md>] [-v]"
        );
        eprintln!();
        eprintln!("Compare reference and Rust UFFS scan outputs for parity verification.");
        eprintln!();
        eprintln!("Arguments:");
        eprintln!(
            "  reference_output.txt  Reference output from trial_run.ps1 (e.g., baseline_c.txt)"
        );
        eprintln!("  rust_output.txt       Rust output (e.g., rust_reference_full_c.txt)");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --report <file>  Write markdown report to file");
        eprintln!("  -v, --verbose    Show all differences (not just samples)");
        std::process::exit(1);
    }

    let reference_path = Path::new(&args[1]);
    let rust_path = Path::new(&args[2]);

    // Parse optional arguments
    let mut report_path: Option<&Path> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--report" if i + 1 < args.len() => {
                report_path = Some(Path::new(&args[i + 1]));
                i += 2;
            }
            "-v" | "--verbose" => {
                // TODO: implement verbose mode
                i += 1;
            }
            _ => i += 1,
        }
    }

    println!("{}", "═".repeat(70));
    println!("UFFS SCAN PARITY COMPARISON");
    println!("{}", "═".repeat(70));

    // Load CSVs
    let reference_df = load_csv(reference_path, "reference")?;
    let rust_df = load_csv(rust_path, "Rust")?;

    // Perform comparison
    let results = compare_dataframes(
        &reference_df,
        &rust_df,
        &reference_path.display().to_string(),
        &rust_path.display().to_string(),
    )?;

    // Print results
    print_results(&results);

    // Write markdown report if requested
    if let Some(report) = report_path {
        write_markdown_report(&results, report)?;
    }

    Ok(())
}

// ============================================================================
// Report Generation
// ============================================================================

/// Format a number with comma separators for display.
fn format_num(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i.is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Print comparison results to stdout.
fn print_results(results: &ComparisonResults) {
    println!("\n{}", "═".repeat(70));
    println!("UFFS SCAN PARITY COMPARISON REPORT");
    println!("{}", "═".repeat(70));

    println!("\n📁 FILES COMPARED");
    println!("  Reference: {}", results.reference_file);
    println!("  Rust: {}", results.rust_file);

    println!("\n📊 ROW COUNTS");
    println!(
        "  Reference rows: {:>8}",
        format_num(results.reference_total_rows)
    );
    println!("  Rust rows:  {:>12}", format_num(results.rust_total_rows));
    // Row counts are well within `i64::MAX` for any practical MFT;
    // `i64::try_from` is the lint-free way to compute the signed
    // difference without a `cast_possible_wrap` expect.
    let rust_i64 = i64::try_from(results.rust_total_rows).unwrap_or(i64::MAX);
    let ref_i64 = i64::try_from(results.reference_total_rows).unwrap_or(i64::MAX);
    let diff = rust_i64.wrapping_sub(ref_i64);
    println!("  Difference: {:>+12}", diff);

    println!("\n🔗 PATH MATCHING");
    println!(
        "  Common paths:    {:>12}",
        format_num(results.common_paths)
    );
    println!(
        "  Reference only:  {:>12}",
        format_num(results.reference_only_paths)
    );
    println!(
        "  Rust only:       {:>12}",
        format_num(results.rust_only_paths)
    );
    println!("  Match rate:      {:>11.4}%", results.path_match_rate);

    println!("\n📎 ALTERNATE DATA STREAMS (ADS)");
    println!(
        "  Reference ADS:    {:>10}",
        format_num(results.reference_ads_count)
    );
    println!(
        "  Rust ADS entries: {:>10}",
        format_num(results.rust_ads_count)
    );

    println!("\n📈 FIELD-BY-FIELD COMPARISON");
    println!(
        "{:>20} {:>12} {:>12} {:>10} {:>12}",
        "Field", "Compared", "Matches", "Rate", "Max Diff"
    );
    println!("{}", "-".repeat(70));

    let mut fields: Vec<_> = results.field_stats.keys().collect();
    fields.sort();

    for field in &fields {
        let stats = &results.field_stats[*field];
        if stats.total_compared > 0 {
            println!(
                "{:>20} {:>12} {:>12} {:>9.4}% {:>12}",
                field,
                format_num(uffs_mft::frs_to_usize(stats.total_compared)),
                format_num(uffs_mft::frs_to_usize(stats.exact_matches)),
                stats.match_rate(),
                if stats.max_abs_diff > 0.0 {
                    format!("{:.0}", stats.max_abs_diff)
                } else {
                    "-".to_string()
                }
            );
        }
    }

    if !results.sample_reference_only.is_empty() {
        println!("\n❌ SAMPLE PATHS IN REFERENCE OUTPUT BUT NOT IN RUST (first 20):");
        for path in &results.sample_reference_only {
            println!("  {path}");
        }
        if results.reference_only_paths > 20 {
            println!("  ... and {} more", results.reference_only_paths - 20);
        }
    }

    if !results.sample_rust_only.is_empty() {
        println!("\n❌ SAMPLE PATHS IN RUST BUT NOT IN REFERENCE OUTPUT (first 20):");
        for path in &results.sample_rust_only {
            println!("  {path}");
        }
        if results.rust_only_paths > 20 {
            println!("  ... and {} more", results.rust_only_paths - 20);
        }
    }

    for (field, stats) in &results.field_stats {
        if !stats.diff_samples.is_empty() {
            println!("\n⚠️  SAMPLE {field} DIFFERENCES:");
            for (path, ref_val, rust_val) in &stats.diff_samples {
                println!("  {path}");
                println!("    REF: {ref_val}  Rust: {rust_val}");
            }
        }
    }

    println!("\n{}", "═".repeat(70));
    println!("SUMMARY");
    println!("{}", "═".repeat(70));

    let all_match = results.reference_only_paths == 0
        && results.rust_only_paths == 0
        && results.field_stats.values().all(|s| s.mismatches == 0);

    if all_match {
        println!("\n✅ PERFECT PARITY - All paths and fields match exactly!");
    } else {
        println!("\n⚠️  DIFFERENCES DETECTED:");
        if results.reference_only_paths > 0 {
            println!(
                "  - {} paths missing from Rust",
                results.reference_only_paths
            );
        }
        if results.rust_only_paths > 0 {
            println!("  - {} extra paths in Rust", results.rust_only_paths);
        }
        for (field, stats) in &results.field_stats {
            if stats.mismatches > 0 {
                println!("  - {} mismatches in '{field}' field", stats.mismatches);
            }
        }
    }
}

/// Write markdown report to file.
fn write_markdown_report(results: &ComparisonResults, path: &Path) -> Result<()> {
    let mut f = File::create(path)?;

    writeln!(f, "# UFFS Scan Parity Report")?;
    writeln!(f)?;
    writeln!(
        f,
        "Generated: {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    )?;
    writeln!(f)?;

    writeln!(f, "## Files Compared")?;
    writeln!(f)?;
    writeln!(f, "| Source | File |")?;
    writeln!(f, "|--------|------|")?;
    writeln!(f, "| Reference | `{}` |", results.reference_file)?;
    writeln!(f, "| Rust | `{}` |", results.rust_file)?;
    writeln!(f)?;

    writeln!(f, "## Row Counts")?;
    writeln!(f)?;
    writeln!(f, "| Metric | Count |")?;
    writeln!(f, "|--------|------:|")?;
    writeln!(
        f,
        "| Reference rows | {} |",
        format_num(results.reference_total_rows)
    )?;
    writeln!(f, "| Rust rows | {} |", format_num(results.rust_total_rows))?;
    // Row counts within `i64::MAX` — see `print_summary` for the
    // identical `i64::try_from` rationale.
    let rust_i64 = i64::try_from(results.rust_total_rows).unwrap_or(i64::MAX);
    let ref_i64 = i64::try_from(results.reference_total_rows).unwrap_or(i64::MAX);
    let diff = rust_i64.wrapping_sub(ref_i64);
    writeln!(f, "| Difference | {:+} |", diff)?;
    writeln!(f)?;

    writeln!(f, "## Path Matching")?;
    writeln!(f)?;
    writeln!(f, "| Metric | Count |")?;
    writeln!(f, "|--------|------:|")?;
    writeln!(f, "| Common paths | {} |", format_num(results.common_paths))?;
    writeln!(
        f,
        "| Reference only | {} |",
        format_num(results.reference_only_paths)
    )?;
    writeln!(f, "| Rust only | {} |", format_num(results.rust_only_paths))?;
    writeln!(
        f,
        "| **Match rate** | **{:.4}%** |",
        results.path_match_rate
    )?;
    writeln!(f)?;

    writeln!(f, "## Alternate Data Streams (ADS)")?;
    writeln!(f)?;
    writeln!(f, "| Source | ADS Count |")?;
    writeln!(f, "|--------|----------:|")?;
    writeln!(
        f,
        "| Reference | {} |",
        format_num(results.reference_ads_count)
    )?;
    writeln!(f, "| Rust | {} |", format_num(results.rust_ads_count))?;
    writeln!(f)?;

    writeln!(f, "## Field-by-Field Comparison")?;
    writeln!(f)?;
    writeln!(f, "| Field | Compared | Matches | Match Rate | Max Diff |")?;
    writeln!(f, "|-------|----------|---------|------------|----------|")?;

    let mut fields: Vec<_> = results.field_stats.keys().collect();
    fields.sort();

    for field in &fields {
        let stats = &results.field_stats[*field];
        if stats.total_compared > 0 {
            writeln!(
                f,
                "| {} | {} | {} | {:.4}% | {} |",
                field,
                format_num(uffs_mft::frs_to_usize(stats.total_compared)),
                format_num(uffs_mft::frs_to_usize(stats.exact_matches)),
                stats.match_rate(),
                if stats.max_abs_diff > 0.0 {
                    format!("{:.0}", stats.max_abs_diff)
                } else {
                    "-".to_string()
                }
            )?;
        }
    }
    writeln!(f)?;

    let all_match = results.reference_only_paths == 0
        && results.rust_only_paths == 0
        && results.field_stats.values().all(|s| s.mismatches == 0);

    writeln!(f, "## Summary")?;
    writeln!(f)?;
    if all_match {
        writeln!(
            f,
            "✅ **PERFECT PARITY** - All paths and fields match exactly!"
        )?;
    } else {
        writeln!(f, "⚠️ **DIFFERENCES DETECTED**")?;
        writeln!(f)?;
        if results.reference_only_paths > 0 {
            writeln!(
                f,
                "- {} paths missing from Rust",
                results.reference_only_paths
            )?;
        }
        if results.rust_only_paths > 0 {
            writeln!(f, "- {} extra paths in Rust", results.rust_only_paths)?;
        }
        for field in &fields {
            let stats = &results.field_stats[*field];
            if stats.mismatches > 0 {
                writeln!(f, "- {} mismatches in `{}` field", stats.mismatches, field)?;
            }
        }
    }

    println!("📝 Report written to: {}", path.display());
    Ok(())
}
