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
use uffs_client::protocol::response::SearchPayload;

use super::args::{extract_spawn_args, inject_no_output_for_null_stdout, resolve_out_path};
use super::dispatch::{write_aggregations, write_rows};
use crate::client_profile::{ClientProfile, print_client_profile};
use crate::{args, dispatch, search_retry};

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
    // 2 minutes — `from_mins` is nightly-only as of 2026-04.
    let ready_timeout = core::time::Duration::from_secs(120);
    client
        .await_ready(ready_timeout)
        .with_context(|| "Daemon did not become ready in time")?;
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
    let raw_response = search_retry::search_cli_with_warm_retry(&mut client, &args_owned)
        .with_context(|| "Daemon search_cli failed")?;
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

    if args
        .iter()
        .any(|arg| arg == "--profile" || arg == "--benchmark")
    {
        print_client_profile(&ClientProfile {
            connect_ms,
            ready_ms,
            ipc_ms,
            duration_ms: response.duration_ms,
            promotion_ms: response.promotion_ms.unwrap_or(0),
            payload: &response.payload,
            total_count: response.total_count,
            daemon_profile: response.profile.as_ref(),
        });
    }

    // OPT-4: When --out is specified, the daemon writes the file directly
    // and returns `SearchPayload::Empty`.  Don't overwrite the file.
    // Handles both `--out foo.csv` (separate arg) and `--out=foo.csv` (= form).
    let has_out = args
        .iter()
        .any(|arg| arg == "--out" || arg.starts_with("--out="));
    let daemon_wrote_file = has_out && response.payload.is_empty();

    // Phase 3.1 NUL fast path: `--no-output` (explicit or auto-injected
    // for NUL stdout) skips every client-side stdout write.
    let suppress_stdout = args_owned.iter().any(|arg| arg == "--no-output" || arg == "--benchmark");

    if !daemon_wrote_file && !suppress_stdout {
        write_search_payload_to_stdout(response.payload, args)?;
    }

    if !suppress_stdout && !response.aggregations.is_empty() {
        // `write_aggregations` still consumes `&[serde_json::Value]`
        // for format flexibility — re-serialise the typed
        // `AggregateResultWire` list via `to_value` once up front
        // and pass the slice to the helper.  Allocation is one per
        // aggregation bucket, which is trivial compared to the
        // aggregation itself.
        let agg_values: Vec<serde_json::Value> = response
            .aggregations
            .iter()
            .filter_map(|agg| serde_json::to_value(agg).ok())
            .collect();
        write_aggregations(&agg_values, args)?;
    }

    Ok(())
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
/// `clippy::too_many_lines` cap.
fn write_search_payload_to_stdout(payload: SearchPayload, args: &[String]) -> Result<()> {
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
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            uffs_client::shmem::stream_paths_blob_into(shmem_path, &mut handle)
                .with_context(|| format!("Failed to stream shmem_blob from {shmem_path_str}"))?;
        }
        SearchPayload::InlineBlob(blob) => {
            // Single write_all to stdout — the buffer is one
            // contiguous slice; the whole point of the blob
            // inline transport.
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            std::io::Write::write_all(&mut handle, blob.as_bytes())
                .with_context(|| "Failed to write inline_blob to stdout")?;
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
            write_rows(&row_values, args)?;
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
            write_rows(&row_values, args)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use uffs_client::protocol::SearchParams;

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
