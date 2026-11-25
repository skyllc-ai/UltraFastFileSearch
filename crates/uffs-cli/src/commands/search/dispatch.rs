// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Thin-client output dispatch for `search_cli` responses.
//!
//! Extracts format/column/separator settings from raw CLI args and
//! delegates to the output module for formatting.

use std::io::Write;

use anyhow::Result;
use uffs_client::format::extract_drive_letter;

use super::super::output::{render_native_results_into, write_native_results};

// ── Thin-client output helpers ─────────────────────────────────────────
//
// Used by the passthrough `search_cli` path where no `SearchConfig` exists.

/// Extract a flag value from raw CLI args (options only: a token after `--`
/// is the pattern, never a flag).
fn arg_val<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let eq_prefix = format!("{flag}=");
    let mut iter = super::args::options(args).iter();
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix(eq_prefix.as_str()) {
            return Some(rest);
        }
        if arg == flag {
            return iter.next().map(String::as_str);
        }
    }
    None
}

/// Default `--format` when the user didn't pass one.
///
/// `table` when stdout is an interactive terminal (a human is reading),
/// `csv` when stdout is piped/redirected or the result goes to a file
/// via `--out` (a machine is reading) — the convention of modern CLIs
/// (ripgrep, fd, bat): pretty for eyes, structured for pipes, and no
/// existing script breaks because pipes keep the CSV default.
///
/// `to_terminal` is true when the output goes to the console (no `--out`)
/// and stdout is an interactive terminal; the search path decides it once
/// and names the result for the daemon (`args::with_default_format`).
pub(crate) const fn default_format(to_terminal: bool) -> &'static str {
    if to_terminal { "table" } else { "csv" }
}

/// Whether stdout is an interactive terminal.
pub(crate) fn stdout_is_terminal() -> bool {
    uffs_client::stdout_kind::StdoutKind::detect().is_terminal()
}

/// Output settings resolved from the raw CLI args — the one place
/// `--format` / `--columns` / `--sep` / drive targets are read, shared
/// by the console path ([`write_rows`]) and the benchmark sink
/// ([`write_rows_into`]) so both render byte-for-byte the same output.
struct OutputSettings<'a> {
    /// `--out` destination (`console` when absent).
    out: &'a str,
    /// Resolved `--format`.
    format: &'a str,
    /// `--columns`, or `parity` when `--parity-compat` is set.
    columns: &'a str,
    /// `--sep`.
    sep: &'a str,
    /// `--quotes`.
    quotes: &'a str,
    /// `--header` (default on).
    header: bool,
    /// `--pos` parity boolean string.
    pos: &'a str,
    /// `--neg` parity boolean string.
    neg: &'a str,
    /// `--tz-offset` in hours.
    tz_offset: Option<i32>,
    /// Drive letters for the footer.
    targets: Vec<uffs_mft::platform::DriveLetter>,
    /// The search pattern (first positional).
    pattern: &'a str,
}

impl<'a> OutputSettings<'a> {
    /// Read every output-affecting flag from `args`.
    fn from_args(args: &'a [String]) -> Self {
        let out = arg_val(args, "--out").unwrap_or("console");
        let format = arg_val(args, "--format")
            .or_else(|| arg_val(args, "-f"))
            .unwrap_or_else(|| default_format(out == "console" && stdout_is_terminal()));
        // --parity-compat implies --columns parity (matches legacy OutputConfig
        // behaviour).
        let (opts, operands) = uffs_client::protocol::cli_args::split_end_of_options(args);
        let parity_compat = opts.iter().any(|arg| arg == "--parity-compat");
        let columns = if parity_compat {
            "parity"
        } else {
            arg_val(args, "--columns").unwrap_or("")
        };
        let sep = arg_val(args, "--sep").unwrap_or(",");
        let quotes = arg_val(args, "--quotes").unwrap_or("\"");
        let header = arg_val(args, "--header").is_none_or(|val| val != "false" && val != "0");
        let pos = arg_val(args, "--pos").unwrap_or("1");
        let neg = arg_val(args, "--neg").unwrap_or("0");
        let tz_offset = arg_val(args, "--tz-offset").and_then(|val| val.parse::<i32>().ok());

        // Extract drive targets for footer.
        let drive = arg_val(args, "--drive").or_else(|| arg_val(args, "-d"));
        let drives_str = arg_val(args, "--drives");
        let mft_str = arg_val(args, "--mft-file");
        let mut targets: Vec<uffs_mft::platform::DriveLetter> = Vec::new();
        if let Some(drive_val) = drive {
            if let Some(letter) = drive_val
                .chars()
                .next()
                .and_then(|ch| uffs_mft::platform::DriveLetter::parse(ch).ok())
            {
                targets.push(letter);
            }
        } else if let Some(drives_val) = drives_str {
            for part in drives_val.split(',') {
                let trimmed = part.trim();
                let stripped = trimmed.strip_suffix(':').unwrap_or(trimmed);
                if let Some(letter) = stripped
                    .chars()
                    .next()
                    .and_then(|ch| uffs_mft::platform::DriveLetter::parse(ch).ok())
                {
                    targets.push(letter);
                }
            }
        } else if let Some(mft_val) = mft_str {
            for part in mft_val.split(',') {
                if let Some(letter) = extract_drive_letter(part.trim()) {
                    targets.push(letter);
                }
            }
        }

        let pattern = operands
            .first()
            .or_else(|| opts.first())
            .map_or("*", String::as_str);

        Self {
            out,
            format,
            columns,
            sep,
            quotes,
            header,
            pos,
            neg,
            tz_offset,
            targets,
            pattern,
        }
    }
}

/// Write search result rows to console using format extracted from raw
/// CLI args.
///
/// The daemon already writes to file when `--out` is set (OPT-4),
/// so this only handles console output.
///
/// # Errors
///
/// Returns an error if writing fails.
pub fn write_rows(rows: &[serde_json::Value], args: &[String]) -> Result<()> {
    let cfg = OutputSettings::from_args(args);
    write_native_results(
        rows,
        cfg.format,
        cfg.out,
        cfg.columns,
        cfg.sep,
        cfg.quotes,
        cfg.header,
        cfg.pos,
        cfg.neg,
        cfg.tz_offset,
        &cfg.targets,
        core::time::Duration::ZERO,
        cfg.pattern,
    )
}

/// Render search result rows with the same settings as [`write_rows`],
/// but into `writer` instead of the console — the `--benchmark` sink.
///
/// # Errors
///
/// Returns an error if formatting or the write fails.
pub(crate) fn write_rows_into<W: Write>(
    writer: &mut W,
    rows: &[serde_json::Value],
    args: &[String],
) -> Result<()> {
    let cfg = OutputSettings::from_args(args);
    render_native_results_into(
        writer,
        rows,
        cfg.format,
        cfg.columns,
        cfg.sep,
        cfg.quotes,
        cfg.header,
        cfg.pos,
        cfg.neg,
        cfg.tz_offset,
        &cfg.targets,
        cfg.pattern,
    )
}

/// Write aggregate results to console.
///
/// # Errors
///
/// Returns an error if writing fails.
pub(crate) fn write_aggregations(
    aggregations: &[serde_json::Value],
    args: &[String],
) -> Result<()> {
    let format = arg_val(args, "--format")
        .or_else(|| arg_val(args, "-f"))
        .unwrap_or_else(|| default_format(stdout_is_terminal()));
    match format {
        "json" => {
            let json = serde_json::to_string_pretty(aggregations)?;
            writeln!(std::io::stdout(), "{json}")?;
        }
        "csv" | "tsv" => {
            crate::commands::aggregate::print_csv_results_raw(aggregations, format == "tsv")?;
        }
        _ => {
            crate::commands::aggregate::print_table_results_raw(aggregations)?;
        }
    }
    Ok(())
}
