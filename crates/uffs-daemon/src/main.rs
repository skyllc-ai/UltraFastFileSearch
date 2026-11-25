// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS Daemon binary — thin wrapper around [`uffs_daemon::run_daemon`].
//!
//! The actual daemon logic lives in the `uffs-daemon` library crate so it
//! can also be invoked via `uffs --daemon run` (single-binary deployment).
//!
//! # Usage
//!
//! ```bash
//! uffs-daemon                          # default settings
//! uffs-daemon --mft-file C.bin D.bin   # load specific MFT files
//! uffs-daemon --idle-timeout 300       # retire after 5 min idle
//! uffs-daemon --idle-timeout 0         # never retire (as --no-retire)
//! uffs-daemon --no-retire              # stay running indefinitely
//! uffs-daemon --log-level debug        # verbose logging
//! ```
// Note: the `windows_unix_domain_sockets` nightly feature is declared in
// `lib.rs` (where `std::os::windows::net::UnixListener` is actually used
// by the `ipc` module).  The thin binary here only calls
// `uffs_daemon::run_daemon()` and does not directly name any
// feature-gated item, so declaring the feature here produces a
// `unused_features` warning.

use std::path::PathBuf;

use mimalloc::MiMalloc;

/// Use mimalloc globally — faster than the Windows CRT heap and, critically,
/// `mi_collect(true)` can aggressively decommit freed pages so RSS reflects
/// actual usage after `MftIndex` temporaries are dropped.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Suppress unused-crate-dependency warnings for deps consumed by the
// library crate (lib.rs) rather than the binary.
use anyhow as _;
use clap::Parser;
use dirs_next as _;
use futures as _;
#[cfg(unix)]
use libc as _;
use libmimalloc_sys as _;
#[cfg(test)]
use proptest as _;
use rand as _;
use serde as _;
use serde_json as _;
#[cfg(test)]
use tempfile as _;
use thiserror as _;
use tokio as _;
// Phase 6 of memory-tiering: `toml` is consumed by the library
// crate's `daemon.toml` parser (`crate::config`).  The binary
// (this file) is a thin wrapper around `run_daemon`, so we suppress
// the `-W unused-crate-dependencies` warning here in line with the
// other library-only deps above.
use toml as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;
// `uffs_broker_protocol` is in `[target.'cfg(windows)'.dependencies]`
// of this crate and consumed only by the library's `broker_client.rs`
// on Windows.  The thin binary `uffsd` doesn't reference it directly.
// Mark intentional on Windows so `unused_crate_dependencies` stays
// quiet; on non-Windows the dep doesn't exist as an extern crate so
// no marker is needed (F5 / issue #205).
#[cfg(windows)]
use uffs_broker_protocol as _;
use uffs_client::connect_sync::UffsClientSync;
use uffs_client::protocol::response::LoadDriveResponse;
use uffs_core as _;
use uffs_format as _;
use uffs_mft as _;
use uffs_security as _;
#[cfg(windows)]
use windows as _;

/// UFFS background daemon — holds MFT index, serves queries via IPC.
#[derive(Parser)]
#[command(name = "uffsd", version, about = "UFFS background search daemon")]
struct Cli {
    /// MFT files to load (*.bin, *.raw, *.iocp, *.uffs).
    #[arg(long = "mft-file", value_name = "PATH")]
    mft_files: Vec<PathBuf>,

    /// Data directory containing `drive_*` subdirectories with MFT files.
    #[arg(long = "data-dir", value_name = "DIR")]
    data_dir_arg: Option<PathBuf>,

    /// Live drives to load (Windows only, e.g. C D E).
    #[arg(long = "drive", value_name = "LETTER")]
    drives: Vec<uffs_mft::platform::DriveLetter>,

    /// Idle timeout in seconds before auto-retire (default: 7200 = 2 hours;
    /// 0 = never retire, the same as `--no-retire`).
    #[arg(long, default_value = "7200")]
    idle_timeout: u64,

    /// Disable auto-retire (stay running indefinitely).
    #[arg(long)]
    no_retire: bool,

    /// Skip cache when loading (force fresh MFT parse).
    #[arg(long)]
    no_cache: bool,

    /// Log level (error, warn, info, debug, trace).
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Write daemon logs to this file instead of stdout.
    ///
    /// When set, all tracing output is appended to the specified file.
    /// Use `"-"` or omit the path to default to `./uffs_daemon.log`.
    #[arg(long, value_name = "PATH")]
    log_file: Option<PathBuf>,

    /// Run as an ephemeral, job-scoped instance identified by `ID`
    /// rather than the resident per-user daemon.
    ///
    /// Both the lifecycle directory (PID file) and the IPC endpoint are
    /// derived from `ID` — see [`uffs_daemon::DaemonConfig::ephemeral_id`]
    /// — so this instance can run alongside a resident daemon without
    /// colliding with it. Connect via
    /// `UffsClientSync::connect_at(&
    /// uffs_client::daemon_ctl::ephemeral_endpoint(ID))`.
    #[arg(long, value_name = "ID")]
    ephemeral_id: Option<String>,

    /// VSS-snapshot device to load, as `DEVICE_PATH=LETTER` (Windows
    /// only, repeatable — e.g.
    /// `\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy5=C`).
    ///
    /// Read fresh (never cached — see
    /// [`uffs_daemon::DaemonConfig::device_sources`]), so this instance
    /// never affects, and is never affected by, a resident daemon's
    /// cache for the named drive letter.
    #[arg(long = "device", value_name = "DEVICE_PATH=LETTER", value_parser = parse_device_source)]
    device_sources: Vec<(String, uffs_mft::platform::DriveLetter)>,
}

/// Parse one `--device DEVICE_PATH=LETTER` argument.
///
/// Splits on the *last* `=` — Windows device paths never contain `=`,
/// but splitting from the end is the more conservative choice if that
/// ever changes.
fn parse_device_source(input: &str) -> Result<(String, uffs_mft::platform::DriveLetter), String> {
    let (path, letter) = input
        .rsplit_once('=')
        .ok_or_else(|| format!("expected DEVICE_PATH=LETTER, got '{input}' (missing '=')"))?;
    if path.is_empty() {
        return Err(format!("empty device path in '{input}'"));
    }
    let drive: uffs_mft::platform::DriveLetter = letter
        .parse()
        .map_err(|err| format!("invalid drive letter '{letter}' in '{input}': {err}"))?;
    Ok((path.to_owned(), drive))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    uffs_version::handle_version!("uffsd");
    let cli = Cli::parse();

    // Initialize tracing (standalone binary owns the subscriber).
    // UFFS_LOG overrides --log-level; RUST_LOG is accepted as an alias so
    // that the standard `$env:RUST_LOG="trace"` idiom works for diagnostics.
    let log_spec = std::env::var("UFFS_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| cli.log_level.clone());

    let tracing_setup = uffs_daemon::init_tracing(&log_spec, cli.log_file.as_deref());
    // Tracing is not up for these (they say why it is not up as asked), so
    // stderr is the channel; a failure to write it is the daemon's error.
    {
        use std::io::Write as _;
        let mut stderr = std::io::stderr().lock();
        for warning in &tracing_setup.warnings {
            writeln!(stderr, "{warning}")?;
        }
    }

    // Startup parameter dump — emitted at DEBUG so it is available for
    // diagnostics (`--log-level debug`) but never pollutes the default
    // `info` console of a detached/foreground daemon.
    tracing::debug!(
        drives = ?cli.drives,
        mft_files = ?cli.mft_files,
        log_spec = %log_spec,
        cli_log_level = %cli.log_level,
        log_file = ?cli.log_file,
        env_uffs_log = ?std::env::var("UFFS_LOG").ok(),
        env_rust_log = ?std::env::var("RUST_LOG").ok(),
        "uffsd startup parameters"
    );

    // Keep copies for potential IPC forwarding (moved into config below).
    let fwd_drives = cli.drives.clone();
    let fwd_mft_files: Vec<String> = cli
        .mft_files
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let fwd_no_cache = cli.no_cache;
    let is_ephemeral = cli.ephemeral_id.is_some();

    let config = uffs_daemon::DaemonConfig {
        mft_files: cli.mft_files,
        data_dir: cli.data_dir_arg,
        drives: cli.drives,
        retire: uffs_daemon::Retire::from_flags(cli.idle_timeout, cli.no_retire),
        no_cache: cli.no_cache,
        log_level: cli.log_level,
        log_file: cli.log_file,
        ephemeral_id: cli.ephemeral_id,
        device_sources: cli.device_sources,
    };

    match uffs_daemon::run_daemon(config).await {
        Ok(()) => Ok(()),
        // "Forward to the already-running daemon" only makes sense for
        // the resident, well-known-endpoint daemon — an ephemeral
        // instance has no sibling to forward to, and the resident
        // daemon (if any) is a different, unrelated instance.
        Err(err) if is_already_running(&err) && !is_ephemeral => {
            // Another daemon is running — forward the load request via IPC.
            forward_to_running_daemon(&fwd_drives, &fwd_mft_files, fwd_no_cache)
        }
        Err(err) => Err(err),
    }
}

/// Check if the error is the "already running" sentinel.
fn is_already_running(err: &anyhow::Error) -> bool {
    format!("{err}").contains("Another daemon instance is already running")
}

/// Log the results of a [`LoadDriveResponse`].
fn log_load_response(resp: &LoadDriveResponse) {
    for letter in &resp.loaded {
        tracing::info!(drive = %letter, "Drive loaded");
    }
    for letter in &resp.already_loaded {
        tracing::info!(drive = %letter, "Drive already loaded");
    }
    for msg in &resp.errors {
        tracing::error!(error = %msg, "Failed to load drive");
    }
}

/// Forward `--drive` / `--mft-file` to the running daemon via IPC.
fn forward_to_running_daemon(
    drives: &[uffs_mft::platform::DriveLetter],
    mft_files: &[String],
    no_cache: bool,
) -> anyhow::Result<()> {
    tracing::debug!(
        ?drives,
        ?mft_files,
        no_cache,
        "forward_to_running_daemon: begin"
    );

    if drives.is_empty() && mft_files.is_empty() {
        tracing::info!("Daemon is already running. Nothing to load.");
        return Ok(());
    }

    tracing::info!("Daemon is already running — forwarding load request via IPC...");
    let mut client = UffsClientSync::connect()?;

    if !drives.is_empty() {
        // Per-item outcomes are logged by `log_load_response`.
        log_load_response(&client.load_drive_letters(drives, no_cache)?);
    }
    if !mft_files.is_empty() {
        log_load_response(&client.load_drive(mft_files, no_cache)?);
    }

    Ok(())
}
