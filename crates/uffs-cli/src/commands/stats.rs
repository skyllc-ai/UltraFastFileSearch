// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Stats command implementation.
//!
//! **Daemon mode** (no path): `run_stats` synthesises an aggregate-only
//! `overview` search and re-enters through
//! [`crate::commands::search::run_search`] — reusing the full search
//! daemon lifecycle (auto-start, await_ready, data-dir forwarding).
//!
//! Legacy parquet-mode stats have been removed from the thin CLI.
//! Use `uffsd` directly or `uffs --stats` (daemon mode) instead.

use anyhow::{Context as _, Result, bail};

use crate::args;
use crate::commands::search::run_search;

/// Handle `uffs --stats [--data-dir DIR] [--mft-file PATH]`: synthesise an
/// aggregate-only `overview` search and run it.
///
/// `--data-dir` and `--mft-file` (repeatable) are passed on to the daemon;
/// any other argument is refused.  (`--stats` used to take a `PATH` and
/// `--top N` that did nothing; they are gone.)
///
/// # Errors
///
/// A flag `--stats` does not take, a flag without its value, or a failure
/// of the synthesised overview search.
pub(crate) fn run_stats(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        args::print_stats_help();
        return Ok(());
    }
    let mut synth_args: Vec<String> = [
        "*", "--agg", "overview", "--format", "table", "--limit", "0",
    ]
    .map(str::to_owned)
    .to_vec();
    synth_args.extend(source_flags(args)?);
    run_search(&synth_args)
}

/// The data-source flags of `uffs --stats`, ready to pass on to the search.
///
/// # Errors
///
/// A flag `--stats` does not take, or a flag without its value.
fn source_flags(args: &[String]) -> Result<Vec<String>> {
    let mut flags = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--data-dir" | "--mft-file" => {
                let val = iter
                    .next()
                    .with_context(|| format!("`uffs --stats {arg}` needs a value"))?;
                flags.extend([arg.clone(), val.clone()]);
            }
            other => bail!(
                "`uffs --stats` does not take `{other}` (it takes --data-dir and --mft-file; \
                 see `uffs --stats --help`)"
            ),
        }
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::source_flags;

    /// Owned argument vector from string slices.
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().copied().map(String::from).collect()
    }

    #[test]
    fn stats_passes_data_sources_on() {
        assert_eq!(
            source_flags(&args(&["--data-dir", "d", "--mft-file", "a.bin,b.bin"]))
                .expect("valid flags"),
            ["--data-dir", "d", "--mft-file", "a.bin,b.bin"]
        );
    }

    #[test]
    fn stats_refuses_the_removed_path_and_top() {
        source_flags(&args(&["C:\\index"])).expect_err("a PATH is refused");
        source_flags(&args(&["--top", "5"])).expect_err("--top is refused");
        source_flags(&args(&["--mft-file"])).expect_err("a missing value is refused");
    }
}
