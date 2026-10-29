// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS Daemon library — reusable daemon entry point.
//!
//! This crate exposes [`run_daemon`] so the daemon logic can be invoked
//! both from the standalone `uffs-daemon` binary and from the embedded
//! `uffs --daemon run` subcommand in the CLI.
//!
//! Setup helpers + shutdown coordination live in sibling modules
//! ([`log_init`], `startup`, `shutdown` — the latter two are
//! crate-private); the orchestrator [`run_daemon`] and the `spawn_*`
//! cluster live here so the parent-task lifetime relationships stay
//! cohesive in one file.
//!
//! # Environment
//!
//! Env vars read by this crate (registry:
//! `docs/architecture/code-quality/build_codegen_policy.md` §5, playbook
//! §1049-1056).  Cache-tier knobs are read via `pub(crate) const FOO_ENV: &str
//! = "UFFS_FOO_…";` indirections in `cache::policy`; the audit script
//! `scripts/dev/build_codegen_audit.sh` detects these.
//!
//! | Env var | Type | Default | Notes |
//! |---|---|---|---|
//! | `CARGO_MANIFEST_DIR` | `path` | (set by Cargo) | Test-fixture path resolution.  CARGO semver class. |
//! | `CARGO_PKG_VERSION` | `string` | (set by Cargo) | Read via `env!()` for status payload + log preludes.  CARGO semver class. |
//! | `RUST_LOG` | `string` | `info` | `tracing-subscriber` filter directive; consulted in `main` when `UFFS_LOG` is unset.  STANDARD semver class (tracing convention). |
//! | `UFFS_LOG` | `string` | `info` | UFFS-specific log level override for the daemon binary.  INTERNAL semver class. |
//! | `UFFS_HOT_TO_WARM_IDLE_SECS` | `int` (seconds) | `600` (10 min) | Cache tier `Hot → Warm` transition timer override (via `HOT_TO_WARM_IDLE_ENV` const indirection).  INTERNAL semver class. |
//! | `UFFS_WARM_TO_PARKED_IDLE_SECS` | `int` (seconds) | `1_800` (30 min) | Cache tier `Warm → Parked` transition timer override (via `WARM_TO_PARKED_IDLE_ENV` const indirection).  INTERNAL semver class. |
//! | `UFFS_PARKED_TO_COLD_IDLE_SECS` | `int` (seconds) | `86_400` (24 h) | Cache tier `Parked → Cold` transition timer override (via `PARKED_TO_COLD_IDLE_ENV` const indirection).  INTERNAL semver class. |
//! | `UFFS_USN_REFRESH_INTERVAL_SECS` | `int` (seconds) | `300` (5 min) | USN journal refresh interval override (via `USN_REFRESH_INTERVAL_ENV` const indirection).  INTERNAL semver class. |
//! | `UFFS_SEARCH_MAX_CONCURRENCY` | `int` (search permits) | auto: `max(2, cpus × 26 / (drives × 10))` | Overrides the auto-tuned search-permit target for `(cpus, drives)` topology (via `index::DriveIndex::SEARCH_CONCURRENCY_ENV` const indirection).  INTERNAL semver class. |
//! | `XDG_RUNTIME_DIR` | `path` | (XDG: `/run/user/$UID`) | Linux daemon-socket location.  STANDARD semver class. |
//!
//! # Concurrency
//!
//! Runs on `#[tokio::main]` (default multi-threaded runtime).  The
//! daemon's startup graph spawns a fixed set of long-lived tasks via
//! the named `spawn_*` constructors above:
//!
//! * `spawn_load_task` — drive-load orchestration with per-drive timeout
//!   (`IndexManager::DRIVE_LOAD_TIMEOUT`).
//! * `spawn_ipc_servers` — Unix-socket accept loop + Windows `AF_UNIX` bridge;
//!   per-connection idle timeout (`IDLE_CONNECTION_SECS`).
//! * `spawn_stats_heartbeat` — periodic `DaemonStats` snapshot to subscribed
//!   clients.
//! * `spawn_idle_demote_controller` — memory-pressure-driven shard-demote
//!   signal source.
//! * `spawn_journal_loops_for_warm_shards` — per-shard USN journal loops, each
//!   cooperatively cancelled via a dedicated `watch::Sender<bool>`.
//! * `spawn_pressure_subscriber` — logs OS memory-pressure transitions for
//!   operators.  It takes no action: only the idle TTL ladder retires an index
//!   (owner ruling 2026-10-03).
//!
//! All shutdown coordination flows through the daemon's top-level
//! `LifecycleHandle` (`watch::Sender<bool>` broadcast + force-exit
//! watchdog).  See the crate-private `lifecycle` module for the full
//! ownership diagram and
//! `docs/architecture/code-quality/concurrency_policy.md` for the
//! workspace contract.

// Enable unstable Windows Unix domain socket support (Windows 10 1803+).
#![cfg_attr(windows, feature(windows_unix_domain_sockets))]

extern crate alloc;

// Linked for this crate's binary target(s), which call
// `uffs_version::handle_version!` in `main`; the library itself does not
// reference it (sanctioned unused-crate-dependencies marker).
use alloc::sync::Arc;
use std::path::PathBuf;

// Suppress unused crate warnings for deps used by sub-modules, the binary, or
// behind cfg gates.
use clap as _;
use dirs_next as _;
use mimalloc as _;
use serde as _;
use thiserror as _;
use tracing_appender as _;
use tracing_subscriber as _;
use uffs_mft as _;
use uffs_version as _;

/// Broker client — volume handle requests (Windows) / stubs (other).
mod broker_client;
/// Shard-based index cache with per-drive lifecycle.
///
/// Phase 1 of the memory-tiering work — see
/// `docs/refactor/memory-tiering-implementation-plan.md`.
mod cache;
/// `daemon.toml` parser — Phase 6 of memory-tiering.
///
/// Schema mirrors plan §11; defaults match Phase-3 static behavior.
/// The type is named [`config::Config`] (idiomatic
/// `crate::module::Type`) to avoid collision with the existing
/// [`DaemonConfig`] runtime-args wrapper that this file owns.
/// Commit C wires the loader into [`run_daemon`] startup and
/// replaces the env-var-overridable static getters in
/// [`crate::cache::policy`] with config-driven readers.
mod config;
/// Daemon event broadcasting — push notifications to connected clients.
pub mod events;
/// JSON-RPC request handler.
mod handler;
/// Index manager — loads and queries MFT data.
mod index;
/// IPC server — Unix domain socket / named pipe listener.
mod ipc;
/// Lifecycle manager — PID file, idle timer, shutdown coordination.
mod lifecycle;
/// `--status` observability signals (live-update loop count, daemon paths).
mod live_update;
/// JSON-RPC protocol types.
mod protocol;
/// Phase 2b memory-tiering: runtime-tempfile orphan cleanup at boot.
mod runtime_orphans;
/// Process-level memory and runtime telemetry.
pub(crate) mod telemetry;

/// Tracing-subscriber bootstrap (re-exports [`init_tracing`] as part
/// of the daemon library's public API).
pub mod log_init;
/// Graceful-shutdown + force-exit watchdog.
mod shutdown;
/// Pre-spawn startup helpers (panic-hook install, data-source
/// gathering, lifecycle bootstrap).
mod startup;

pub use log_init::init_tracing;

/// Configuration for [`run_daemon`].
pub struct DaemonConfig {
    /// MFT files to load.
    pub mft_files: Vec<PathBuf>,
    /// Data directory containing `drive_*` subdirectories.
    pub data_dir: Option<PathBuf>,
    /// Explicit drive letters (Windows only).
    pub drives: Vec<uffs_mft::platform::DriveLetter>,
    /// VSS-snapshot device sources — `(device_path, drive)` pairs
    /// (Windows only). Each is read fresh via
    /// `uffs_core::compact::MftSource::Device` and, unlike `drives`,
    /// never gets a background USN journal loop (the drive letter here
    /// names which live volume the snapshot was taken *from*, not a
    /// live volume this instance should keep polling — see
    /// `spawn_journal_loops_for_warm_shards`'s ephemeral-instance
    /// guard in `lib.rs`).
    pub device_sources: Vec<(String, uffs_mft::platform::DriveLetter)>,
    /// Idle timeout in seconds (0 = use default 7200s / 2 hours).
    pub idle_timeout: u64,
    /// Disable auto-retire.
    pub no_retire: bool,
    /// Skip cache.
    pub no_cache: bool,
    /// Log level string (e.g. "info", "debug").
    pub log_level: String,
    /// Optional log file path.  When set, daemon tracing output is
    /// written to this file instead of stdout.  If the value is empty
    /// or `"-"`, the daemon defaults to `./uffs_daemon.log` in the
    /// current working directory.
    pub log_file: Option<PathBuf>,
    /// Run as an ephemeral, job-scoped instance rather than the
    /// resident per-user daemon.
    ///
    /// When set, both the lifecycle directory (PID file, shutdown
    /// nonce) and the IPC endpoint (Unix socket / Windows named pipe)
    /// are derived from this id via
    /// [`uffs_client::daemon_ctl::ephemeral_lifecycle_dir`] /
    /// [`uffs_client::daemon_ctl::ephemeral_endpoint`] instead of the
    /// well-known per-user paths — so this instance can run alongside
    /// a resident daemon without colliding with it. Callers connect
    /// via [`uffs_client::connect_sync::UffsClientSync::connect_at`]
    /// with the matching `ephemeral_endpoint`.
    pub ephemeral_id: Option<String>,
}

/// Run the UFFS daemon with the given configuration.
///
/// This is the main entry point shared by both the standalone
/// `uffs-daemon` binary and the embedded `uffs --daemon run` subcommand.
///
/// **Does not return** until the daemon shuts down (idle timeout,
/// RPC shutdown, or signal).
///
/// # Errors
///
/// Returns an error if another daemon is already running, data sources
/// are missing, or the IPC server fails to bind.
pub async fn run_daemon(config: DaemonConfig) -> anyhow::Result<()> {
    startup::install_catastrophe_panic_hook();
    startup::log_daemon_starting(&config);

    let (event_tx, _event_rx) = events::event_channel();
    startup::emit_daemon_starting_event(&event_tx);

    let lifecycle_mgr = startup::bootstrap_lifecycle_manager(&config, event_tx.clone())?;

    // D5.0: clean up stale shmem files from previous daemon sessions.
    uffs_client::shmem::cleanup_stale_shmem_files();

    // Phase 2b: wipe runtime-tempfile leftovers from dead daemon PIDs
    // before our own PID-scoped subdir gets created by the first
    // `load_compact_cache` call.  The cross-platform `cleanup_orphans`
    // contract makes this safe even when other live daemons share the
    // root directory.
    runtime_orphans::sweep_runtime_tempfile_orphans();

    // Phase 6 Commit C task 6.5 — resolve `daemon.toml` from the
    // platform-default location.  Factored out to keep
    // `run_daemon`'s cognitive complexity under the workspace's
    // strict-clippy ceiling.
    let daemon_config = startup::load_daemon_config()?;

    // Create index manager — uses the user-supplied --data-dir for offline MFT
    // discovery and hot-loading (not the lifecycle directory).
    let idx = Arc::new(index::IndexManager::new(
        config.data_dir.clone(),
        event_tx,
        Arc::clone(&daemon_config),
    ));
    tracing::debug!(index_data_dir = ?idx.data_dir(), "Index manager created");

    let mft_files = startup::gather_mft_files(&config);
    let drives = startup::resolve_drive_list(&config);
    tracing::info!(mft_files = mft_files.len(), drives = ?drives, "Final data sources");

    // Refuse to start with zero data sources — an empty daemon is useless.
    startup::validate_data_sources(&mft_files, &drives, &config.device_sources, &lifecycle_mgr)?;
    tracing::info!("Data sources validated OK");

    let load_task = spawn_load_task(
        Arc::clone(&idx),
        mft_files,
        drives,
        config.device_sources.clone(),
        config.no_cache,
        lifecycle_mgr.handle(),
        config.ephemeral_id.is_some(),
    );

    let ipc_task = spawn_ipc_servers(&idx, &lifecycle_mgr, config.ephemeral_id.clone());
    let _stats_task = spawn_stats_heartbeat(Arc::clone(&idx), lifecycle_mgr.handle());
    let _mem_snapshot_task = telemetry::spawn_mem_snapshot_task(
        Arc::clone(&idx),
        telemetry::DEFAULT_MEM_SNAPSHOT_INTERVAL,
    );
    // Phase 3 Commit D — periodic shard idle-demote sweep.
    let _idle_demote_task = spawn_idle_demote_controller(Arc::clone(&idx));

    // Phase 7 activation: per-shard journal loops are spawned
    // inside `spawn_load_task` after `record_load_complete()`.  They
    // replace the deleted Phase-5 `spawn_usn_refresh_controller`
    // 5-min global tick with per-letter event-driven refresh — see
    // `spawn_journal_loops_for_warm_shards` below.

    // Memory-pressure subscriber: logs every kernel `Low` / `High`
    // transition so an operator can correlate them with the tier
    // ladder, and nothing else — a search is what brings a shard back
    // to Warm, idle time is what retires it.  Nothing fires on
    // Mac/Linux (the platform `PressureSignal` never does).
    let _pressure_task = spawn_pressure_subscriber(&idx);

    // Run idle timer (blocks until shutdown or timeout) then tear
    // everything down.  Returns `!` so `force_exit_with_watchdog`
    // covers the post-await tail.
    shutdown::await_shutdown_then_force_exit(lifecycle_mgr, ipc_task, load_task).await
}

/// Spawn the parallel load task that reads `mft_files` from disk and
/// (on Windows) the live drives via the broker / direct path.  The
/// task also enforces the zero-drive shutdown guard so a daemon that
/// fails every load doesn't linger in `Ready` with an empty index.
fn spawn_load_task(
    load_index: Arc<index::IndexManager>,
    mft_files: Vec<PathBuf>,
    drives: Vec<uffs_mft::platform::DriveLetter>,
    device_sources: Vec<(String, uffs_mft::platform::DriveLetter)>,
    no_cache: bool,
    load_lifecycle: lifecycle::LifecycleHandle,
    is_ephemeral: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!(mft_files = mft_files.len(), drives = ?drives, "Load task starting");
        if !mft_files.is_empty() {
            tracing::info!("Loading MFT files from data dir...");
            load_index.load_from_data_dir(&mft_files, no_cache).await;
            tracing::info!("MFT files loaded");
        }
        // Live-MFT scanning is Windows-only; on every other platform the
        // load task is fully covered by `load_from_data_dir` above.
        #[cfg(windows)]
        load_live_drives_if_windows(&load_index, &drives, no_cache, &load_lifecycle).await;
        #[cfg(windows)]
        if !device_sources.is_empty() {
            tracing::info!("Loading VSS-snapshot device sources...");
            load_index.load_device_drives(&device_sources).await;
            tracing::info!("Device sources loaded");
        }
        #[cfg(not(windows))]
        debug_assert!(
            device_sources.is_empty(),
            "MftSource::Device is Windows-only (VSS snapshots don't exist elsewhere); \
             device_sources must be empty on this platform"
        );
        tracing::info!("Load task completed");

        // Latch the load phase as complete: from this point on, a daemon
        // serving zero queries against fully-loaded drives is legitimately
        // idle (not stalled), so the load-stall force-retire guard is
        // permanently disarmed.  Per-drive stuck-load detection remains
        // active via `DRIVE_LOAD_TIMEOUT` inside the load loop itself.
        // Closes the Phase 5 G4 finding (LOG/uffsd-G4-bonus.log line 104,
        // 2 h 11 min force-retire on a healthy idle daemon).
        load_lifecycle.record_load_complete();

        // Phase 7 activation: spawn one per-shard journal loop for
        // every loaded letter.  Replaces the deleted Phase-5 global
        // 5-min `refresh_usn_for_warm_shards` tick.  The applier
        // task's JoinHandle is fire-and-forget here — it stays alive
        // for the daemon's lifetime via the `Arc<RegistryPatchSink>`
        // captured by the per-loop sink clones; on shutdown the
        // applier exits cleanly when the last sink Arc drops.
        //
        // Skipped entirely for an ephemeral instance: its loaded drive
        // letters (from `--device`) name the volume a VSS snapshot was
        // taken *from*, not a live volume to keep polling — applying
        // live USN deltas onto that frozen, point-in-time capture would
        // silently corrupt the very guarantee VSS exists to provide.
        let _journal_applier = if is_ephemeral {
            tracing::info!("Ephemeral instance: skipping per-shard journal loops");
            None
        } else {
            Some(spawn_journal_loops_for_warm_shards(&load_index).await)
        };

        zero_drive_shutdown_guard(&load_index, &load_lifecycle).await;
    })
}

/// Windows live-drive load step.  Walks the broker for an elevated
/// handle per drive (best-effort), then drives `load_live_drives` to
/// scan their MFTs.
#[cfg(windows)]
async fn load_live_drives_if_windows(
    load_index: &Arc<index::IndexManager>,
    drives: &[uffs_mft::platform::DriveLetter],
    no_cache: bool,
    load_lifecycle: &lifecycle::LifecycleHandle,
) {
    if drives.is_empty() {
        return;
    }
    broker_client::warm_up_broker_handles_unless_elevated(drives);
    tracing::info!(drives = ?drives, "Loading live drives...");
    load_index
        .load_live_drives(drives, no_cache, load_lifecycle)
        .await;
    tracing::info!("Live drives loaded");
}

/// Catch the "every load failed but `Ready` fired anyway" zombie
/// state.  Triggers an explicit shutdown request when the post-load
/// drive count is zero so the lifecycle's `select!` tears the daemon
/// down cleanly instead of leaving it queryable-but-empty.
async fn zero_drive_shutdown_guard(
    load_index: &Arc<index::IndexManager>,
    load_lifecycle: &lifecycle::LifecycleHandle,
) {
    let loaded_drives = load_index.loaded_drive_letters().await;
    if loaded_drives.is_empty() {
        tracing::error!(
            "Daemon loaded zero drives even though data sources were provided — every \
             parse attempt failed.  Shutting down to avoid the Ready-with-no-data state. \
             Check the load errors above; common causes: missing/corrupt .iocp files, \
             insufficient privileges on live drives, or a data_dir that contains no MFT \
             captures."
        );
        load_lifecycle.request_shutdown();
    }
}

/// Run the primary (`AF_UNIX`) IPC listener to completion, logging any
/// terminal error instead of propagating it — matches the fire-and-log
/// contract `spawn_ipc_servers`'s `tokio::spawn` body used to embed
/// inline before this helper was split out for cfg-branching.
#[cfg(unix)]
async fn run_primary_ipc_server(
    index: Arc<index::IndexManager>,
    lifecycle: lifecycle::LifecycleHandle,
    ephemeral_id: Option<String>,
) {
    if let Err(ipc_err) = ipc::run_ipc_server(index, lifecycle, ephemeral_id.as_deref()).await {
        tracing::error!(error = %ipc_err, "IPC server error");
    }
}

/// Windows counterpart of [`run_primary_ipc_server`].
///
/// The legacy `AF_UNIX` bridge (`ipc::run_ipc_server` on Windows) has no
/// ephemeral-endpoint support, and none is needed: an ephemeral instance
/// is reached exclusively via the named pipe (`spawn_ipc_servers`'s
/// `_pipe_task`, which IS ephemeral-aware). Skip the bridge entirely for
/// an ephemeral instance rather than bind it to the resident daemon's
/// well-known `AF_UNIX` path, where it would collide.
#[cfg(windows)]
async fn run_primary_ipc_server(
    index: Arc<index::IndexManager>,
    lifecycle: lifecycle::LifecycleHandle,
    ephemeral_id: Option<String>,
) {
    if ephemeral_id.is_some() {
        tracing::info!("Ephemeral instance: skipping legacy AF_UNIX bridge (named pipe only)");
        return;
    }
    if let Err(ipc_err) = ipc::run_ipc_server(index, lifecycle).await {
        tracing::error!(error = %ipc_err, "IPC server error");
    }
}

/// Spawn the IPC server task(s).
///
/// Always spawns the `AF_UNIX` listener (the cross-platform fallback);
/// on Windows additionally spawns the named-pipe listener which is
/// the preferred transport (no `ws2_32` dependency, ~54 ms faster per
/// CLI launch).  Returns the `AF_UNIX` `JoinHandle` so the caller can
/// `.abort()` it during graceful shutdown; the named-pipe task is
/// fire-and-forget — the watchdog will reap it.
fn spawn_ipc_servers(
    idx: &Arc<index::IndexManager>,
    lifecycle_mgr: &lifecycle::LifecycleManager,
    ephemeral_id: Option<String>,
) -> tokio::task::JoinHandle<()> {
    let ipc_index = Arc::clone(idx);
    let ipc_lifecycle = lifecycle_mgr.handle();
    // On Windows `ephemeral_id` is needed again below for the named-pipe
    // task, so this must clone; on Unix there is no second use, so moving
    // it directly avoids a real `redundant_clone` clippy finding.
    #[cfg(windows)]
    let ipc_ephemeral_id = ephemeral_id.clone();
    #[cfg(not(windows))]
    let ipc_ephemeral_id = ephemeral_id;

    tracing::info!("Starting IPC server...");
    let ipc_task = tokio::spawn(run_primary_ipc_server(
        ipc_index,
        ipc_lifecycle,
        ipc_ephemeral_id,
    ));
    tracing::info!("IPC server task spawned");

    // Task ownership (Phase 10c): the Windows named-pipe IPC server is
    // **fire-and-forget** — the `_pipe_task` `JoinHandle` is bound but
    // never `.abort()`-ed.  Rationale (mirrors the parent fn rustdoc):
    //
    //   * **Owner:** the binding `_pipe_task` lives until end-of-scope in
    //     `spawn_ipc_servers`; the task itself outlives the binding (Tokio
    //     detaches a spawned task once its `JoinHandle` drops).
    //   * **Shutdown:** none cooperative — the pipe `accept` loop has no
    //     cancellation hook; the `await_shutdown_then_force_exit` watchdog
    //     `process::exit`s the daemon, terminating the task with the runtime.
    //   * **Error obs.:** body logs via `tracing::error!`; outer `JoinHandle`
    //     drop discards the result.
    //   * **Cancel behavior:** process-exit only.
    //
    // The sibling AF_UNIX task IS returned + held + `.abort()`-ed —
    // that's the "primary" IPC transport on Unix.  On Windows, both
    // transports coexist (AF_UNIX for tools using the cross-platform
    // socket path; named-pipe for native Windows tooling); aborting
    // just one and letting the other ride out process exit is a
    // deliberate asymmetry.
    #[cfg(windows)]
    let _pipe_task = {
        let pipe_index = Arc::clone(idx);
        let pipe_lifecycle = lifecycle_mgr.handle();
        let pipe_ephemeral_id = ephemeral_id;
        tracing::info!("Starting named-pipe IPC server...");
        tokio::spawn(async move {
            if let Err(pipe_err) =
                ipc::run_pipe_server(pipe_index, pipe_lifecycle, pipe_ephemeral_id.as_deref()).await
            {
                tracing::error!(error = %pipe_err, "Named-pipe IPC server error");
            }
        })
    };

    ipc_task
}

/// Spawn the periodic stats heartbeat — pushes `StatsHeartbeat`
/// events to all connected clients every 30 seconds.
fn spawn_stats_heartbeat(
    stats_index: Arc<index::IndexManager>,
    stats_lifecycle: lifecycle::LifecycleHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(core::time::Duration::from_secs(30));
        // Skip the first tick (fires immediately).
        interval.tick().await;
        loop {
            interval.tick().await;
            let total_records = stats_index.total_records().await;
            let stats = stats_index.stats().await;
            stats_index
                .event_sender()
                .emit(events::DaemonEvent::StatsHeartbeat {
                    total_queries: stats.total_queries,
                    uptime_secs: stats.uptime_secs,
                    total_records,
                    connections: stats_lifecycle.active_connections(),
                });
        }
    })
}

/// Spawn the Phase 3 idle-demote controller — every 30 s, ask
/// `IndexManager::demote_idle_shards` to walk the registry and
/// demote any shard whose idle time exceeds its tier's TTL (see
/// `cache::policy`).
///
/// The 30 s cadence is shorter than the shortest TTL (300 s
/// Hot→Warm) by an order of magnitude, so a freshly idle Hot
/// shard demotes within at most one tick of crossing its
/// boundary.  Sampling `unix_now_ms()` once at the top of each
/// tick (and threading it into the manager) keeps every shard's
/// idle-secs computation referenced to the same baseline — a
/// slow read-lock acquisition can't push later shards in the
/// same batch over a TTL boundary mid-walk.
fn spawn_idle_demote_controller(idx: Arc<index::IndexManager>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(core::time::Duration::from_secs(30));
        // Skip the first tick (fires immediately at task spawn).
        // We want the controller to take its first reading 30 s
        // after startup, not at t=0 when no shard could possibly
        // be idle yet.
        interval.tick().await;
        loop {
            interval.tick().await;
            idx.demote_idle_shards(cache::unix_now_ms()).await;
        }
    })
}

/// Phase 7 activation: spawn one [`crate::cache::journal_loop::JournalLoop`]
/// per loaded drive letter.
///
/// Replaces the deleted Phase-5 `spawn_usn_refresh_controller`
/// global 5-min tick with per-shard event-driven refresh.  Each
/// loop polls its drive's NTFS USN journal at the configured
/// cadence (default 500 ms via
/// [`crate::cache::journal_loop::JournalLoopConfig`]) and fires save triggers
/// when the per-shard `SaveTrigger`'s events-threshold (50K events) or
/// age-threshold (5 min) crosses. Each save trigger drives a full
/// [`crate::index::IndexManager::handle_journal_refresh`] which
/// reuses the Phase-5
/// [`uffs_core::compact_loader::load_drive_with_usn_refresh`] infrastructure
/// for the body refresh + compact-cache write.
///
/// All loops share a single [`crate::cache::journal_sink::RegistryPatchSink`]
/// applier task: the sink's mpsc channel preserves FIFO ordering
/// across letters and avoids re-entering the runtime from the
/// loop's sync `accept` callback — see the `journal_sink` module
/// docs for the full rationale.
///
/// **Platform split**:
/// * **Windows**: each letter gets a `WindowsJournalSource` (real
///   `FSCTL_QUERY_USN_JOURNAL` + `FSCTL_READ_USN_JOURNAL`) and a
///   [`crate::cache::cursor_store::DiskCursorStore`] rooted at
///   `uffs_mft::cache::cache_dir()`.
/// * **macOS / Linux**: each letter gets a
///   [`crate::cache::journal_loop::sources::MacStubJournalSource`]
///   (always-empty polls) and a
///   [`crate::cache::journal_loop::sources::NullCursorStore`] (no-op).  The
///   loop ticks at the configured cadence but produces no patches and triggers
///   no saves — there's no live USN journal to drive against.
///
/// Returns the [`tokio::task::JoinHandle`] for the applier task.
/// The caller (`run_daemon`) keeps it alive for the daemon's
/// lifetime; per-letter loop handles are stored on `IndexManager`
/// via [`crate::index::IndexManager::attach_journal_handle`].  On
/// daemon shutdown the per-loop `watch::Sender`s are dropped with
/// the `IndexManager`, signalling every loop's cancellation arm
/// (matching the existing fire-and-forget pattern used by
/// `spawn_idle_demote_controller` / `spawn_pressure_subscriber`).
async fn spawn_journal_loops_for_warm_shards(
    idx: &Arc<index::IndexManager>,
) -> tokio::task::JoinHandle<()> {
    use cache::cursor_store::DiskCursorStore;
    use cache::journal_loop::sources::NullCursorStore;
    use cache::journal_loop::{
        CursorStore, JournalLoopConfig, JournalSource, PatchSink, spawn_journal_loop,
    };
    use cache::journal_sink::RegistryPatchSink;

    // Cursor-store choice: Windows persists per-drive cursors next
    // to the compact cache; Mac/Linux uses the always-zero
    // NullCursorStore (no journal → no cursor → fall back to
    // "start from journal head" semantics, which is the correct
    // no-op on those platforms).
    //
    // Shared with the applier: the sink persists each loop's cursor
    // in lockstep with a successful compact-cache body save (the
    // loops only seed from it and reset it on wrap), so the on-disk
    // cursor never outruns the on-disk body.  See `journal_sink`.
    let cursor_store: Arc<dyn CursorStore> = if cfg!(windows) {
        Arc::new(DiskCursorStore::new(uffs_mft::cache::cache_dir()))
    } else {
        Arc::new(NullCursorStore)
    };

    let (sink, applier_handle) =
        RegistryPatchSink::spawn_with_applier(idx, Arc::clone(&cursor_store));
    let sink_dyn: Arc<dyn PatchSink> = sink;

    let config = JournalLoopConfig::from_env();
    let letters = idx.loaded_drive_letters().await;
    tracing::info!(
        target: "shard.journal",
        count = letters.len(),
        letters = ?letters,
        poll_interval_ms = config.poll_interval.as_millis(),
        apply_interval_ms = config.apply_interval.as_millis(),
        save_threshold_events = config.save_threshold_events,
        save_threshold_age_secs = config.save_threshold_age.as_secs(),
        save_min_interval_secs = config.save_min_interval.as_secs(),
        save_age_min_events = config.save_age_min_events,
        save_age_ceiling_secs = config.save_age_ceiling.as_secs(),
        "Spawning per-shard journal loops",
    );
    let loop_count = letters.len();
    for letter in letters {
        let source: Arc<dyn JournalSource> = make_journal_source(letter);
        let handle = spawn_journal_loop(
            letter,
            source,
            Arc::clone(&sink_dyn),
            Arc::clone(&cursor_store),
            config.clone(),
        );
        idx.attach_journal_handle(letter, handle);
    }
    // Record live-update liveness once, here — so `--status` can report "N
    // loops running" without any per-patch bookkeeping on the apply hot path.
    live_update::set_active_loops(loop_count);

    applier_handle
}

/// Construct the platform-correct [`crate::cache::journal_loop::JournalSource`]
/// for `letter`.
///
/// Extracted from [`spawn_journal_loops_for_warm_shards`] as two
/// cfg-gated definitions so the unused-`letter` parameter on
/// Mac/Linux uses the canonical `_letter` rename rather than a
/// `let _ = letter;` body workaround that clippy flags.
#[cfg(windows)]
fn make_journal_source(
    letter: uffs_mft::platform::DriveLetter,
) -> Arc<dyn cache::journal_loop::JournalSource> {
    Arc::new(cache::journal_loop::sources::WindowsJournalSource::new(
        letter,
    ))
}

/// Mac/Linux variant of [`make_journal_source`] — always returns
/// the [`crate::cache::journal_loop::sources::MacStubJournalSource`]
/// stub (no NTFS USN journal on these platforms).
#[cfg(not(windows))]
fn make_journal_source(
    _letter: uffs_mft::platform::DriveLetter,
) -> Arc<dyn cache::journal_loop::JournalSource> {
    Arc::new(cache::journal_loop::sources::MacStubJournalSource)
}

/// Spawn the memory-pressure subscriber.
///
/// Subscribes to [`IndexManager::subscribe_pressure`] and logs every
/// transition at `INFO` under `target: "cache.pressure"` so an operator
/// reading the daemon log can line kernel pressure up against the tier
/// ladder.  That is the whole job: the subscriber **never demotes**.
///
/// Until 2026-10-03 a `Low` transition cascade-parked LRU Warm shards
/// one by one until pressure cleared.  On a memory-starved host that
/// fired 2.5 minutes after start, parked all six drives right after a
/// benchmark had warmed them, and turned the next `*.*` into a
/// four-minute MFT re-read (the 2026-10-03 benchmark run).  The owner
/// ruled the forced demotion out: a shard goes Parked → Hot when a
/// search needs it and is left alone afterwards; the idle TTL ladder
/// (`spawn_idle_demote_controller`) is the only thing that retires it.
///
/// On Mac/Linux the platform [`PressureSignal`] never fires, so this
/// task blocks on `rx.changed().await` forever.
///
/// Returns the [`tokio::task::JoinHandle`] so the caller can `.abort()`
/// it during graceful shutdown.  When the [`watch::Sender`] inside
/// `IndexManager::pressure` is dropped the receiver's `changed()`
/// returns `Err`; the loop breaks cleanly without any extra signal.
///
/// `pub(crate)` so `crate::index::tests::lifecycle_hooks` can pin the
/// observe-only contract against a `ControllablePressureSignal` fake.
///
/// [`PressureSignal`]: crate::cache::pressure::PressureSignal
/// [`IndexManager::subscribe_pressure`]: crate::index::IndexManager::subscribe_pressure
/// [`watch::Sender`]: tokio::sync::watch::Sender
pub(crate) fn spawn_pressure_subscriber(idx: &index::IndexManager) -> tokio::task::JoinHandle<()> {
    let mut rx = idx.subscribe_pressure();
    tokio::spawn(async move {
        loop {
            // Wait for a pressure transition.  `changed()` returns
            // `Err` when the watch sender is dropped, which happens
            // when `IndexManager` (the only owner) is dropped at
            // daemon shutdown.
            if rx.changed().await.is_err() {
                tracing::debug!(
                    target: "cache.pressure",
                    "Pressure-signal sender dropped; subscriber loop exiting",
                );
                break;
            }
            let level = *rx.borrow_and_update();
            tracing::info!(
                target: "cache.pressure",
                ?level,
                "Pressure transition observed",
            );
        }
    })
}

#[cfg(test)]
mod tests;
