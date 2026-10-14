// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs [--search] <pattern> [flags...]` — the search entry point.
//!
//! Forwards the raw argument vector to the daemon over the `search_cli`
//! RPC, then writes the typed response back to stdout through whichever
//! transport the daemon picked (shmem blob, inline blob, shmem rows,
//! inline rows).  The `--stats` and `--agg` entry points synthesise an
//! argument vector and re-enter through [`run_search`], so this module is
//! the single place where a search leaves the CLI.

use anyhow::{Context as _, Result};
use uffs_client::error::ClientError;
use uffs_client::protocol::response::SearchPayload;

use super::args::{extract_spawn_args, inject_no_output_for_null_stdout, resolve_out_path};
use super::dispatch::{write_aggregations, write_rows, write_rows_into};
use super::unfinished;
use crate::client_profile::{ClientProfile, OutputCost, PayloadSummary, print_client_profile};
use crate::{args, dispatch, search_retry};

/// Readiness wait used when `UFFS_CLIENT_TIMEOUT_SECS=0` switches the client
/// limit off: one year, which is no limit in practice while keeping
/// `await_ready`'s `Instant` arithmetic (it adds up to five times this) far
/// from overflow.
const NO_LIMIT: core::time::Duration = core::time::Duration::from_hours(365 * 24);

/// Forward raw search args to the daemon via `search_cli` RPC.
///
/// # Errors
///
/// Propagates a daemon connect / readiness / RPC failure, a response the
/// CLI cannot deserialise, or a stdout write failure.  A `--flag` that is
/// a near-miss of a management command is rejected up front with a
/// "did you mean" hint instead of a round trip to the daemon.
pub(crate) fn run_search(args: &[String]) -> Result<()> {
    // No pattern, or an explicit help request as the first token
    // (`uffs --search --help`) → the search-first top-level help.
    if matches!(
        args.first().map(String::as_str),
        None | Some("--help" | "-h")
    ) {
        args::print_help();
        return Ok(());
    }

    // Command-typo hint. If the first token is a `--`-flag that the shared
    // parser rejects AND it is a near-miss of a management command, surface a
    // "did you mean" hint up front instead of spinning up the daemon only for
    // it to return a bare unknown-flag error. The CLI suggests over ITS own
    // command set; flag validation stays in `uffs_client::from_cli_args`, so
    // the daemon never learns CLI commands (design: cli-grammar.md §6).
    if let Some(first) = args.first()
        && first.starts_with("--")
        && dispatch::Command::from_token(first).is_none()
        && let Err(uffs_client::protocol::cli_args::Error::UnknownFlag { flag }) =
            uffs_client::protocol::SearchParams::from_cli_args(args)
        && let Some(command) = dispatch::suggest_command(&flag)
    {
        anyhow::bail!(
            "`{flag}` is not a known search flag.\n\
             Did you mean the command `uffs {command}`?  (run `uffs {command} --help`)"
        );
    }

    // Extract daemon-spawn args (--data-dir, --mft-file, --no-cache)
    // from the raw args so we can auto-start the daemon if needed.
    let spawn_args = extract_spawn_args(args);

    let t_connect = std::time::Instant::now();
    let mut client = uffs_client::connect_sync::UffsClientSync::connect_with_args(&spawn_args)
        .with_context(|| "Failed to connect to UFFS daemon")?;
    let connect_ms = t_connect.elapsed().as_millis();

    let t_ready = std::time::Instant::now();
    wait_until_ready(&mut client)?;
    let ready_ms = t_ready.elapsed().as_millis();

    let t_search = std::time::Instant::now();
    // Resolve relative --out paths to absolute using the CLI's cwd, since the
    // daemon process runs in a different working directory.
    // Phase 3.1 NUL fast path: when stdout is redirected to the null
    // device (e.g. `uffs *.dll > NUL`), inject `--no-output` so the
    // daemon skips row materialisation + `paths_blob` construction
    // + IPC row transfer entirely.  Saves ~20-30 ms on medium result
    // sets that would otherwise push 3.5 MB through the pipe just to
    // discard the bytes client-side.
    let args_owned: Vec<String> = inject_no_output_for_null_stdout(resolve_out_path(args));
    let raw_response = match search_retry::search_cli_with_warm_retry(&mut client, &args_owned) {
        Ok(response) => response,
        Err(ClientError::Timeout) => anyhow::bail!(unfinished::timeout_message(
            unfinished::Stage::Search,
            uffs_client::rpc_deadline(),
            &unfinished::probe_daemon(),
        )),
        Err(err) => return Err(err).with_context(|| "Daemon search_cli failed"),
    };
    let ipc_ms = t_search.elapsed().as_millis();

    // v0.5.62: deserialise the daemon response into the typed
    // `SearchResponse` struct.  The `SearchPayload` enum is
    // self-describing (serde tag = "kind", content = "data") so the
    // CLI no longer needs to probe individual fields like
    // `paths_blob`, `paths_blob_shmem`, `shmem_path`, etc. — the
    // enum's variant is the single source of truth for which
    // transport the daemon picked.
    //
    // Unknown fields on the wire are silently ignored (serde default),
    // so newer daemons that add optional response fields are still
    // forward-compatible with this CLI.
    let response: uffs_client::protocol::response::SearchResponse =
        serde_json::from_value(raw_response)
            .with_context(|| "Failed to deserialize search response from daemon")?;

    let profiling = args
        .iter()
        .any(|arg| arg == "--profile" || arg == "--benchmark");

    // OPT-4: When --out is specified, the daemon writes the file directly
    // and returns `SearchPayload::Empty`.  Don't overwrite the file.
    // Handles both `--out foo.csv` (separate arg) and `--out=foo.csv` (= form).
    let has_out = args
        .iter()
        .any(|arg| arg == "--out" || arg.starts_with("--out="));
    let daemon_wrote_file = has_out && response.payload.is_empty();

    // The profile block is printed after the output pass so it can report
    // what producing the output cost; it goes to stderr, so with `-v` the
    // rows and the timings never interleave on one stream.
    let payload_summary = PayloadSummary::of(&response.payload, response.total_count);
    let payload_bytes = payload_byte_hint(&response.payload);
    let output_mode = OutputMode::from_args(&args_owned);
    let output_cost = match output_mode {
        OutputMode::Skip => None,
        OutputMode::Stdout => {
            let t_out = std::time::Instant::now();
            if !daemon_wrote_file {
                write_search_payload(response.payload, args, &mut OutputTarget::Stdout)?;
            }
            write_aggregation_values(&response.aggregations, args)?;
            Some(OutputCost {
                ms: t_out.elapsed().as_millis(),
                bytes: payload_bytes,
                target: "stdout",
            })
        }
        OutputMode::Sink => {
            let t_out = std::time::Instant::now();
            let mut sink = CountingSink::default();
            if !daemon_wrote_file {
                write_search_payload(response.payload, args, &mut OutputTarget::Sink(&mut sink))?;
            }
            if !response.aggregations.is_empty() {
                // Aggregations render as pretty JSON into the sink — the
                // table / CSV printers are stdout-bound; `-v` shows them.
                let json = serde_json::to_string_pretty(&response.aggregations)?;
                std::io::Write::write_all(&mut sink, json.as_bytes())?;
            }
            Some(OutputCost {
                ms: t_out.elapsed().as_millis(),
                bytes: sink.bytes,
                target: "sink",
            })
        }
    };

    if profiling {
        print_client_profile(&ClientProfile {
            connect_ms,
            ready_ms,
            ipc_ms,
            duration_ms: response.duration_ms,
            promotion_ms: response.promotion_ms.unwrap_or(0),
            payload: payload_summary,
            daemon_profile: response.profile.as_ref(),
            output: output_cost,
        });
    }

    Ok(())
}

/// Where a search's rows go on the client.
///
/// * `Stdout` — the normal path, and `--benchmark -v`.
/// * `Sink` — `--benchmark` without `-v`: the full formatting pipeline runs
///   into a byte-counting writer, so the profile measures everything UFFS does
///   to produce the output and excludes only the terminal.  Before this the
///   benchmark skipped the write entirely (#626 fix) and so never measured
///   output production at all, which is the thing a benchmark of a file-search
///   tool is for.
/// * `Skip` — `--no-output` (explicit, or auto-injected when stdout is a null
///   device): the daemon does not even build rows, so there is nothing to
///   format.
///
/// Pure so the decision is unit-testable without a daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    /// Format and write to the real stdout.
    Stdout,
    /// Format into the counting sink.
    Sink,
    /// No output pass at all.
    Skip,
}

impl OutputMode {
    /// Decide the output mode from the raw search args.
    fn from_args(args: &[String]) -> Self {
        let has = |flag: &str| args.iter().any(|arg| arg == flag);
        if has("--no-output") {
            Self::Skip
        } else if has("--benchmark") && !has("-v") && !has("--verbose") {
            Self::Sink
        } else {
            Self::Stdout
        }
    }
}

/// Byte-counting writer backing [`OutputMode::Sink`].
#[derive(Debug, Default)]
struct CountingSink {
    /// Bytes written so far.
    bytes: u64,
}

impl std::io::Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Destination for [`write_search_payload`].
enum OutputTarget<'a> {
    /// The process's stdout.
    Stdout,
    /// The benchmark sink.
    Sink(&'a mut CountingSink),
}

/// Bytes the payload carries as delivered, for the profile's stdout line
/// (the sink counts exactly; stdout reports what it was handed).
const fn payload_byte_hint(payload: &SearchPayload) -> u64 {
    match payload {
        SearchPayload::InlineBlob(blob) => blob.len() as u64,
        SearchPayload::ShmemBlob(_)
        | SearchPayload::ShmemRows { .. }
        | SearchPayload::InlineRows(_)
        | SearchPayload::Empty => 0,
    }
}

/// Wait for the daemon to finish loading its indexes.
///
/// Uses the same client limit as the search itself (10 minutes unless
/// `UFFS_CLIENT_TIMEOUT_SECS` says otherwise): a daemon still loading is a
/// wait, not an error.  `await_ready` treats it as an idle budget, renewed
/// while the load makes progress.
fn wait_until_ready(client: &mut uffs_client::connect_sync::UffsClientSync) -> Result<()> {
    let limit = uffs_client::rpc_deadline();
    match client.await_ready(limit.unwrap_or(NO_LIMIT)) {
        Ok(()) => Ok(()),
        Err(ClientError::Timeout) => anyhow::bail!(unfinished::timeout_message(
            unfinished::Stage::Ready,
            limit,
            &unfinished::probe_daemon(),
        )),
        Err(err) => Err(err).with_context(|| "Daemon did not become ready"),
    }
}

/// Write the aggregation results (if any) to stdout.
fn write_aggregation_values(
    aggregations: &[uffs_client::protocol::AggregateResultWire],
    args: &[String],
) -> Result<()> {
    if aggregations.is_empty() {
        return Ok(());
    }
    // `write_aggregations` still consumes `&[serde_json::Value]`
    // for format flexibility — re-serialise the typed
    // `AggregateResultWire` list via `to_value` once up front
    // and pass the slice to the helper.  Allocation is one per
    // aggregation bucket, which is trivial compared to the
    // aggregation itself.
    let agg_values: Vec<serde_json::Value> = aggregations
        .iter()
        .filter_map(|agg| serde_json::to_value(agg).ok())
        .collect();
    write_aggregations(&agg_values, args)
}

/// Write the daemon's search payload to stdout, picking the fastest
/// transport the daemon selected for this response.
///
/// Priority order matches the [`SearchPayload`] variant dispatch:
///
/// 1. [`SearchPayload::ShmemBlob`] → mmap the raw-bytes file and stream
///    directly to stdout via [`uffs_client::shmem::stream_paths_blob_into`].
///    Zero-copy, zero JSON decode, zero UTF-8 re-validation.  Used for blobs
///    above [`uffs_client::shmem::PATHS_BLOB_SHMEM_THRESHOLD`].
/// 2. [`SearchPayload::InlineBlob`] → single `write_all` of the inline UTF-8
///    buffer.  Skips per-row formatting but still paid ~40 ms of JSON decode on
///    the way in.
/// 3. [`SearchPayload::ShmemRows`] → read the shmem file into a
///    `Vec<SearchRow>` (client's `connect_sync` shim doesn't do transparent
///    resolution for `search_cli`), then fall through to per-row format
///    dispatch.
/// 4. [`SearchPayload::InlineRows`] → traditional per-row format + write
///    dispatch in [`write_rows`].
/// 5. [`SearchPayload::Empty`] → nothing to write.
///
/// Extracted from [`run_search`] to keep that function under the
/// `clippy::too_many_lines` cap.  `target` picks the real stdout or the
/// `--benchmark` sink; every variant does the same work either way.
fn write_search_payload(
    payload: SearchPayload,
    args: &[String],
    target: &mut OutputTarget<'_>,
) -> Result<()> {
    match payload {
        SearchPayload::Empty => {
            // Nothing to write — no-match query, `--no-output`
            // injection, or `--out=file` (daemon already wrote to
            // disk).  The earlier `daemon_wrote_file` guard also
            // handles the latter case at the call site.
        }
        SearchPayload::ShmemBlob(shmem_path_str) => {
            // Binary shmem transport: mmap the file and write bytes
            // directly to stdout with one syscall, then delete the
            // file.  No JSON decode, no intermediate allocation, no
            // UTF-8 re-validation — stdout takes bytes.
            let shmem_path = std::path::Path::new(&shmem_path_str);
            match target {
                OutputTarget::Stdout => {
                    let stdout = std::io::stdout();
                    let mut handle = stdout.lock();
                    uffs_client::shmem::stream_paths_blob_into(shmem_path, &mut handle)
                }
                OutputTarget::Sink(sink) => {
                    uffs_client::shmem::stream_paths_blob_into(shmem_path, sink)
                }
            }
            .with_context(|| format!("Failed to stream shmem_blob from {shmem_path_str}"))?;
        }
        SearchPayload::InlineBlob(blob) => {
            // Single write_all to stdout — the buffer is one
            // contiguous slice; the whole point of the blob
            // inline transport.
            match target {
                OutputTarget::Stdout => {
                    let stdout = std::io::stdout();
                    let mut handle = stdout.lock();
                    std::io::Write::write_all(&mut handle, blob.as_bytes())
                }
                OutputTarget::Sink(sink) => std::io::Write::write_all(sink, blob.as_bytes()),
            }
            .with_context(|| "Failed to write inline_blob")?;
        }
        SearchPayload::ShmemRows { path, .. } => {
            // Shmem rows variant: read the file (returns a
            // `SearchResponse` with `InlineRows`) and dispatch to
            // the per-row writer.  Re-encode rows to `Value` so the
            // existing `write_rows` path (which handles `--format`,
            // `--sep`, `--header`, column resolution, etc.) stays
            // untouched — one Vec allocation scales O(N) but beats
            // duplicating the column-resolution logic.
            let shmem_resp = uffs_client::shmem::read_search_results(std::path::Path::new(&path))
                .with_context(|| format!("Failed to read shmem_rows from {path}"))?;
            let row_values: Vec<serde_json::Value> = shmem_resp
                .payload
                .into_inline_rows()
                .unwrap_or_default()
                .iter()
                .filter_map(|row| serde_json::to_value(row).ok())
                .collect();
            write_row_values(&row_values, args, target)?;
        }
        SearchPayload::InlineRows(rows) => {
            // Traditional per-row format dispatch.  `write_rows`
            // accepts `&[serde_json::Value]` for format flexibility
            // (extract_field, parity-compat, drilldown), so re-
            // serialise the typed rows once up front.
            let row_values: Vec<serde_json::Value> = rows
                .iter()
                .filter_map(|row| serde_json::to_value(row).ok())
                .collect();
            write_row_values(&row_values, args, target)?;
        }
    }
    Ok(())
}

/// Per-row format dispatch to the chosen target.
fn write_row_values(
    rows: &[serde_json::Value],
    args: &[String],
    target: &mut OutputTarget<'_>,
) -> Result<()> {
    match target {
        OutputTarget::Stdout => write_rows(rows, args),
        OutputTarget::Sink(sink) => write_rows_into(sink, rows, args),
    }
}

#[cfg(test)]
mod tests {
    use uffs_client::protocol::SearchParams;

    use super::OutputMode;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// `--benchmark` must run the output pipeline into the sink (it is
    /// what the benchmark measures), `-v` / `--verbose` redirects it to
    /// the real stdout, and only `--no-output` skips the pass.  A value
    /// that merely contains the flag text is not the flag.
    #[test]
    fn output_mode_resolves_benchmark_verbose_and_no_output() {
        assert_eq!(OutputMode::from_args(&args(&["*.rs"])), OutputMode::Stdout);
        assert_eq!(
            OutputMode::from_args(&args(&["*.rs", "--profile"])),
            OutputMode::Stdout
        );
        assert_eq!(
            OutputMode::from_args(&args(&["*.rs", "--benchmark"])),
            OutputMode::Sink
        );
        assert_eq!(
            OutputMode::from_args(&args(&["--benchmark", "--drive", "C", "*"])),
            OutputMode::Sink
        );
        assert_eq!(
            OutputMode::from_args(&args(&["*.rs", "--benchmark", "-v"])),
            OutputMode::Stdout
        );
        assert_eq!(
            OutputMode::from_args(&args(&["*.rs", "--verbose", "--benchmark"])),
            OutputMode::Stdout
        );
        assert_eq!(
            OutputMode::from_args(&args(&["*.rs", "--no-output"])),
            OutputMode::Skip
        );
        assert_eq!(
            OutputMode::from_args(&args(&["*.rs", "--benchmark", "--no-output"])),
            OutputMode::Skip
        );
        assert_eq!(
            OutputMode::from_args(&args(&["--benchmark-results.txt"])),
            OutputMode::Stdout
        );
    }

    /// The sink counts every byte it is handed and never fails.
    #[test]
    fn counting_sink_counts_bytes() {
        use std::io::Write as _;
        let mut sink = super::CountingSink::default();
        sink.write_all(b"hello\n").expect("sink never fails");
        sink.write_all(b"world").expect("sink never fails");
        sink.flush().expect("sink never fails");
        assert_eq!(sink.bytes, 11);
    }

    #[test]
    fn from_cli_args_basic_search() {
        let args: Vec<String> = [
            "*.rs",
            "--drive",
            "C",
            "--format",
            "json",
            "--tz-offset",
            "-8",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let params = SearchParams::from_cli_args(&args).expect("should parse");
        // `*.rs` is promoted to pattern="*" + ext=Some("rs") so the
        // daemon can route through the ExtensionIndex fast path in
        // `numeric_top_n::ext_fast_path` instead of the trigram + glob
        // path.  See `is_pure_ext_glob` in cli_args.rs for the shape
        // acceptance matrix and `test_from_cli_args_ext_glob_promoted`
        // in uffs-client for the full rewrite semantics.
        assert_eq!(params.pattern, "*");
        assert_eq!(params.ext.as_deref(), Some("rs"));
        assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
        assert_eq!(params.output_tz_offset_hours, Some(-8_i32));
    }

    #[test]
    fn from_cli_args_sugar_begins_with() {
        let args: Vec<String> = ["--begins-with", "report"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let params = SearchParams::from_cli_args(&args).expect("should parse");
        assert_eq!(params.pattern, "report*");
    }

    #[test]
    fn from_cli_args_sugar_between() {
        let args: Vec<String> = ["*", "--between", "2026-01-01,2026-03-31"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let params = SearchParams::from_cli_args(&args).expect("should parse");
        assert_eq!(params.newer.as_deref(), Some("2026-01-01"));
        assert_eq!(params.older.as_deref(), Some("2026-03-31"));
    }
}
