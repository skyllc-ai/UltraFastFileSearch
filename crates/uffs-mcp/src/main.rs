// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffsmcp` — standalone UFFS MCP server binary.
//!
//! Handles all MCP lifecycle commands: `run`, `serve`, `start`, `stop`,
//! `status`, `stats`, `kill`, `restart`, `reload`.
//!
//! The CLI (`uffs --mcp <action>`) delegates to this binary so the thin
//! client stays small.
//!
//! # MCP Configuration
//!
//! ```json
//! { "uffs": { "command": "uffsmcp" } }
//! ```

// Crates used by the library but not directly by this binary.
#[cfg(feature = "streamable-http")]
use axum as _;
use rmcp as _;
use schemars as _;
use serde as _;
use serde_json as _;
use thiserror as _;
#[cfg(feature = "streamable-http")]
use tower_service as _;
use tracing_appender as _;
use uffs_mft as _;

mod process;
mod supervisor;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

/// UFFS MCP server — bridges AI agents to the UFFS daemon via the
/// Model Context Protocol.
#[derive(Parser)]
#[command(name = "uffsmcp", version, about)]
struct Cli {
    /// Subcommand to execute.  When invoked without a subcommand,
    /// runs in stdio mode (for AI host integration).
    #[command(subcommand)]
    action: Option<Action>,

    /// Run as the supervised stdio worker (spawned by the supervisor —
    /// see `supervisor.rs`; not part of the public CLI surface).
    #[arg(long, hide = true, global = true)]
    worker: bool,
}

/// MCP server lifecycle actions.
#[derive(Subcommand)]
enum Action {
    /// Run the MCP server on stdin/stdout (for AI hosts).
    Run {
        /// MFT file paths (passed to daemon auto-start).
        #[arg(long = "mft-file", value_name = "PATH")]
        mft_files: Vec<std::path::PathBuf>,
        /// Data directory (passed to daemon auto-start).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// Idle timeout in seconds (0 = no timeout).
        #[arg(long, default_value = "0")]
        idle_timeout: u64,
    },
    /// Start the MCP HTTP server as a background service.
    Start {
        /// MFT file paths (passed to daemon auto-start).
        #[arg(long = "mft-file", value_name = "PATH")]
        mft_files: Vec<std::path::PathBuf>,
        /// Data directory (passed to daemon auto-start).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// Skip index cache.
        #[arg(long)]
        no_cache: bool,
        /// HTTP port.
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Run the MCP HTTP gateway in-process (spawned by `start`).
    #[command(hide = true)]
    Serve {
        /// HTTP port.
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Data directory (passed to daemon auto-start).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// MFT file paths.
        #[arg(long = "mft-file", value_name = "PATH")]
        mft_files: Vec<std::path::PathBuf>,
    },
    /// Show MCP server process status.
    Status,
    /// Show MCP/daemon performance stats.
    Stats,
    /// Gracefully stop the MCP server.
    Stop,
    /// Force-kill the MCP server + clean up.
    Kill {
        /// HTTP port to scan for orphaned processes.
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Kill and restart the MCP server.
    Restart,
    /// Reload stale MCP components to pick up a new binary.
    Reload,
}

fn main() -> Result<()> {
    uffs_version::handle_version!("uffsmcp");
    let cli = Cli::parse();

    // Stdio mode (no subcommand, or `run`) is served through the
    // self-upgrading supervisor: it owns the client's pipes and pumps
    // JSON-RPC through a `--worker` child it can hot-swap when the
    // binary on disk is replaced (see `supervisor.rs`).  The worker
    // child — and every non-stdio action — takes the async path.
    let stdio_mode = matches!(cli.action, None | Some(Action::Run { .. }));
    if stdio_mode && !cli.worker {
        let _ignore = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_max_level(tracing::Level::INFO)
            .try_init();
        // Re-issue our own argv verbatim (plus `--worker`) so the child
        // serves exactly what the client asked the supervisor for.
        let mut worker_args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        worker_args.push(std::ffi::OsString::from("--worker"));
        return supervisor::run(&worker_args);
    }

    // The supervisor path above is pure std (blocking pipes + threads);
    // only the server/action paths need the async runtime.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?
        .block_on(async_main(cli))
}

/// The async half of `main`: the stdio worker and every lifecycle action.
async fn async_main(cli: Cli) -> Result<()> {
    match cli.action {
        // No subcommand → stdio mode (for AI hosts like Claude Desktop).
        None => {
            let _ignore = tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_max_level(tracing::Level::INFO)
                .try_init();
            uffs_mcp::run_mcp_server().await
        }
        Some(action) => run_action(action).await,
    }
}

/// Dispatch an MCP action.
async fn run_action(action: Action) -> Result<()> {
    match action {
        Action::Run {
            mft_files,
            data_dir,
            idle_timeout,
        } => mcp_run(&mft_files, data_dir.as_deref(), idle_timeout).await,
        Action::Serve {
            port,
            bind,
            data_dir,
            mft_files,
        } => mcp_serve(&bind, port, &mft_files, data_dir.as_deref()).await,
        Action::Start {
            mft_files,
            data_dir,
            no_cache,
            port,
            bind,
        } => mcp_start(&mft_files, data_dir.as_deref(), no_cache, &bind, port).await,
        Action::Status => mcp_status().await,
        Action::Stats => mcp_stats().await,
        Action::Stop => {
            mcp_stop();
            Ok(())
        }
        Action::Kill { port, bind } => {
            mcp_kill(port, &bind);
            Ok(())
        }
        Action::Restart => {
            process::mcp_restart();
            Ok(())
        }
        Action::Reload => process::mcp_reload().await,
    }
}

// ── run ─────────────────────────────────────────────────────────────

/// Run the MCP server in-process on stdin/stdout (invoked by AI hosts).
async fn mcp_run(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    idle_timeout: u64,
) -> Result<()> {
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| "info".to_owned());
    let log_file = std::env::var("UFFS_LOG_FILE")
        .ok()
        .map(std::path::PathBuf::from);
    let _guard = uffs_mcp::init_mcp_tracing(&log_spec, log_file.as_deref());

    let config = uffs_mcp::McpConfig {
        daemon_spawn_args: process::build_daemon_args(mft_files, data_dir),
        idle_timeout_secs: idle_timeout,
    };
    uffs_mcp::run_mcp_server_with_config(&config)
        .await
        .with_context(|| "MCP server exited with error")
}

// ── serve ───────────────────────────────────────────────────────────

/// Run the MCP HTTP gateway in-process (spawned by `start`).
#[cfg(feature = "streamable-http")]
async fn mcp_serve(
    bind: &str,
    port: u16,
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
) -> Result<()> {
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| "info".to_owned());
    let log_file = std::env::var("UFFS_LOG_FILE")
        .ok()
        .map(std::path::PathBuf::from);
    let _guard = uffs_mcp::init_mcp_tracing(&log_spec, log_file.as_deref());

    let daemon_args = process::build_daemon_args(mft_files, data_dir);

    tracing::info!("Ensuring daemon is running before HTTP gateway starts...");
    let mut client = uffs_client::connect::UffsClient::connect_with_args(&daemon_args)
        .await
        .with_context(|| "Failed to start daemon")?;
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;
    if let Ok(resp) = client.status().await {
        tracing::info!(pid = resp.pid, "Daemon ready");
    }
    drop(client);

    let transport = format!("http:{bind}:{port}");
    uffs_client::mcp_pid::write_mcp_pid_file_full(&transport, data_dir, mft_files, false);

    let addr: core::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("Invalid bind address: {bind}:{port}"))?;

    let config = uffs_mcp::http::HttpGatewayConfig {
        bind_addr: addr,
        auth_token: None,
        daemon_spawn_args: daemon_args,
    };

    let result = uffs_mcp::http::run_gateway(config).await;
    uffs_client::mcp_pid::remove_mcp_pid_file();
    result
}

/// Fallback when HTTP gateway feature is not enabled.
#[cfg(not(feature = "streamable-http"))]
#[expect(
    clippy::unused_async,
    reason = "signature must match the streamable-http variant which genuinely awaits"
)]
async fn mcp_serve(
    _bind: &str,
    _port: u16,
    _mft_files: &[std::path::PathBuf],
    _data_dir: Option<&std::path::Path>,
) -> Result<()> {
    anyhow::bail!(
        "HTTP gateway requires the `streamable-http` feature. Rebuild with: cargo build -p uffs-mcp --features streamable-http"
    );
}

// ── start ───────────────────────────────────────────────────────────

/// Start the MCP HTTP server as a background service.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn mcp_start(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    no_cache: bool,
    bind: &str,
    port: u16,
) -> Result<()> {
    let daemon_args = {
        let mut args = process::build_daemon_args(mft_files, data_dir);
        if no_cache {
            args.push(std::ffi::OsString::from("--no-cache"));
        }
        args
    };

    if !cfg!(windows) && daemon_args.is_empty() {
        anyhow::bail!(
            "No MFT data sources specified.\n\
             Provide --mft-file <path> or --data-dir <path>."
        );
    }

    let gateway_alive = uffs_client::mcp_pid::is_mcp_server_running().is_some()
        || port_is_occupied(bind, port).await;

    if gateway_alive && preflight_reclaim_or_reuse(bind, port, &daemon_args).await? {
        return Ok(());
    }

    // Spawn `uffsmcp serve` as a detached child.
    let exe = std::env::current_exe().with_context(|| "Failed to get current exe path")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["serve", "--bind", bind, "--port", &port.to_string()]);
    for arg in &daemon_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    if std::env::var("UFFS_LOG_FILE").is_err() {
        let default_log = uffs_security::log_dir::log_dir().join("mcp-gateway.log");
        cmd.env("UFFS_LOG_FILE", &default_log);
    }

    // An explicit start revokes any earlier stop intent.
    uffs_client::daemon_ctl::clear_stop_intent(uffs_client::daemon_ctl::ServiceKind::Mcp);
    println!("Starting MCP HTTP server on {bind}:{port}...");
    let mut child = cmd.spawn().with_context(|| "Failed to spawn MCP server")?;
    let pid = child.id();
    println!("  Spawned (PID {pid})");

    let health_url = format!("http://{bind}:{port}/health");
    let deadline = std::time::Instant::now() + core::time::Duration::from_mins(3);
    let mut ready = false;

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(core::time::Duration::from_millis(250)).await;
        if let Some(exit_status) = child.try_wait().ok().flatten() {
            anyhow::bail!(
                "MCP server process (PID {pid}) exited immediately (status: {exit_status}).\n\
                 Run with logging: UFFS_LOG=debug UFFS_LOG_FILE=/tmp/mcp.log uffsmcp serve --port {port}"
            );
        }
        if let Ok(resp) = process::reqwest_lite_get(&health_url).await
            && resp == "ok"
        {
            ready = true;
            break;
        }
    }

    if ready {
        if child.try_wait().ok().flatten().is_some() {
            anyhow::bail!(
                "Health check passed but spawned process (PID {pid}) is no longer alive."
            );
        }
        println!("  MCP HTTP server ready at http://{bind}:{port}/mcp");
        println!("  Health:  http://{bind}:{port}/health");
        println!("  Status:  http://{bind}:{port}/status");
    } else {
        println!("  ⚠ Server spawned but /health not reachable within 3 minutes.");
    }
    Ok(())
}

/// Check whether a TCP port is already occupied.
async fn port_is_occupied(bind: &str, port: u16) -> bool {
    let addr = format!("{bind}:{port}");
    tokio::net::TcpStream::connect(&addr).await.is_ok()
}

/// Deep health check when target port is occupied.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn preflight_reclaim_or_reuse(
    bind: &str,
    port: u16,
    daemon_args: &[std::ffi::OsString],
) -> Result<bool> {
    let health_url = format!("http://{bind}:{port}/health");
    let gateway_ok = process::reqwest_lite_get(&health_url)
        .await
        .is_ok_and(|body| body == "ok");

    if !gateway_ok {
        println!("  Stale process on port {port} is not healthy — killing it...");
        let tracked_pid = uffs_client::mcp_pid::parse_mcp_pid_file().map(|(pid, _ts)| pid);
        if let Some(pid) = tracked_pid {
            process::signal_pid(pid, true);
        }
        process::kill_process_on_port(port, tracked_pid.unwrap_or(0));
        uffs_client::mcp_pid::remove_mcp_pid_file();
        tokio::time::sleep(core::time::Duration::from_secs(1)).await;
        if port_is_occupied(bind, port).await {
            anyhow::bail!("Port {port} is still in use after killing the stale process.");
        }
        return Ok(false);
    }

    let daemon_ok = match uffs_client::connect::UffsClient::connect_raw().await {
        Ok(mut client) => client.status().await.is_ok(),
        Err(_) => false,
    };

    if daemon_ok {
        println!("MCP HTTP server already running on {bind}:{port} (gateway ✓, daemon ✓).");
        process::reload_stale_stdio_sessions();
        return Ok(true);
    }

    // A *supervised* restart must not undo a deliberate `uffs --daemon
    // stop`. Interactively, "you asked for the gateway, the gateway
    // needs a daemon" is the helpful reading and still applies. But the
    // watchdog is not the operator: when it revives a gateway it must
    // not silently drag a daemon the operator stopped on purpose back
    // up with it — which is exactly how a deliberate stop was observed
    // to bounce straight back.
    if std::env::var_os("UFFS_SUPERVISED_RESTART").is_some()
        && uffs_client::daemon_ctl::stop_intent_path(uffs_client::daemon_ctl::ServiceKind::Daemon)
            .exists()
    {
        println!("  Gateway on port {port} is alive; daemon is stopped on purpose — leaving it.");
        process::reload_stale_stdio_sessions();
        return Ok(true);
    }

    println!("  Gateway on port {port} is alive but daemon is unreachable.");
    println!("  Restarting daemon...");
    let mut client = uffs_client::connect::UffsClient::connect_with_args(daemon_args)
        .await
        .with_context(|| "Failed to restart daemon")?;
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;
    println!("  Daemon restarted — gateway on {bind}:{port} is ready.");
    process::reload_stale_stdio_sessions();
    Ok(true)
}

// ── status ──────────────────────────────────────────────────────────

/// Show MCP server process status + backend info.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn mcp_status() -> Result<()> {
    println!("uffs-mcp v{}", env!("CARGO_PKG_VERSION"));
    println!();

    match uffs_client::mcp_pid::parse_mcp_pid_file_full() {
        Some(info) => {
            let alive = uffs_client::mcp_pid::is_mcp_server_running().is_some();
            let uptime_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |dur| dur.as_secs().saturating_sub(info.start_ts));
            let uptime = core::time::Duration::from_secs(uptime_secs);
            if alive {
                println!("MCP server:    running (PID {})", info.pid);
                println!("  Transport:   {}", info.transport);
                println!(
                    "  Uptime:      {}",
                    uffs_client::format::format_duration(uptime)
                );
                if let Some((bind, port)) = info.http_addr() {
                    let url = format!("http://{bind}:{port}/health");
                    match process::reqwest_lite_get(&url).await {
                        Ok(body) if body == "ok" => {
                            println!("  Health:      ✓ (http://{bind}:{port}/health)");
                        }
                        Ok(body) => {
                            println!("  Health:      ⚠ unexpected: {body}");
                        }
                        Err(err) => {
                            println!("  Health:      ✗ unreachable ({err})");
                        }
                    }
                }
            } else {
                // The process that wrote the file is gone (killed, or its
                // host exited without letting it clean up).  Remove the
                // leftover so it is not reported on every later status,
                // and say so.
                uffs_client::mcp_pid::remove_mcp_pid_file();
                println!(
                    "MCP server:    not running (removed the stale PID file left by PID {})",
                    info.pid
                );
            }
        }
        None => {
            println!("MCP server:    not running (no PID file)");
        }
    }

    println!();
    if let Ok(mut client) = uffs_client::connect::UffsClient::connect_raw().await {
        if let Ok(status) = client.status().await {
            println!("Daemon:        reachable (PID {})", status.pid);
            let state = match &status.status {
                uffs_client::protocol::response::DaemonStatus::Ready => "Ready",
                uffs_client::protocol::response::DaemonStatus::Loading { .. } => "Loading",
                uffs_client::protocol::response::DaemonStatus::Refreshing { .. } => "Refreshing",
            };
            println!("  Status:      {state}");
        } else {
            println!("Daemon:        connected but not responding");
        }
    } else {
        println!("Daemon:        not running");
        println!("  (will auto-start when MCP server connects)");
    }

    Ok(())
}

// ── stats ───────────────────────────────────────────────────────────

/// Show MCP/daemon performance stats.
#[expect(clippy::print_stdout, reason = "CLI output")]
async fn mcp_stats() -> Result<()> {
    match uffs_client::mcp_pid::is_mcp_server_running() {
        Some(pid) => println!("MCP server PID: {pid}"),
        None => println!("MCP server:     not running"),
    }

    let Ok(mut client) = uffs_client::connect::UffsClient::connect_raw().await else {
        println!("Daemon:         not running — no stats available.");
        return Ok(());
    };

    let stats = client
        .stats()
        .await
        .with_context(|| "Failed to query stats from daemon")?;

    let fmt = uffs_client::format::format_duration;
    let uptime = core::time::Duration::from_secs(stats.uptime_secs);
    let startup = core::time::Duration::from_millis(stats.startup_duration_ms);
    let avg_query =
        core::time::Duration::from_micros(uffs_client::format::f64_to_u64(stats.avg_query_time_us));
    let total_query = core::time::Duration::from_micros(stats.total_query_time_us);

    println!();
    println!("═══ Performance Stats ═══");
    println!("Backend uptime:    {}", fmt(uptime));
    println!("Startup duration:  {}", fmt(startup));
    println!(
        "Total records:     {}",
        uffs_client::format::format_number_commas(stats.total_records as u64)
    );
    println!("Queries served:    {}", stats.total_queries);
    if stats.total_queries > 0 {
        println!("Avg query time:    {}", fmt(avg_query));
        println!("Total query time:  {}", fmt(total_query));
    }
    println!("Queries/second:    {:.2}", stats.queries_per_second);
    Ok(())
}

// ── stop ────────────────────────────────────────────────────────────

/// Gracefully stop the MCP server.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_stop() {
    let Some(pid) = uffs_client::mcp_pid::is_mcp_server_running() else {
        println!("MCP server is not running.");
        return;
    };
    // Record intent BEFORE signalling, for the same reason the daemon
    // does: between the process dying and the marker appearing, a
    // watchdog tick would read an unexplained death and restart it.
    uffs_client::daemon_ctl::record_stop_intent(uffs_client::daemon_ctl::ServiceKind::Mcp);
    println!("Stopping MCP server (PID {pid})...");
    process::signal_pid(pid, cfg!(windows));
    println!("MCP server stopped.");
    println!("  (The daemon continues running independently.)");
}

// ── kill ────────────────────────────────────────────────────────────

/// Force-kill the MCP server + clean up PID file.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_kill(port: u16, _bind: &str) {
    let pid_path = uffs_client::mcp_pid::mcp_pid_file_path();
    let mut killed_any = false;

    let tracked_pid = if let Some(info) = uffs_client::mcp_pid::parse_mcp_pid_file_full() {
        println!("Killing MCP server (PID {})...", info.pid);
        process::signal_pid(info.pid, true);
        killed_any = true;
        if let Some((_file_bind, file_port)) = info.http_addr() {
            process::kill_process_on_port(file_port, info.pid);
        }
        info.pid
    } else {
        println!("No MCP server PID file found.");
        0
    };

    process::kill_process_on_port(port, tracked_pid);
    drop(std::fs::remove_file(&pid_path));
    if killed_any {
        println!("MCP server PID file cleaned up.");
    }
    println!("  (The daemon is not affected.)");
}

#[cfg(test)]
#[expect(
    clippy::default_numeric_fallback,
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    reason = "test code"
)]
#[path = "main_tests.rs"]
mod tests;
