// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS MCP Server Library
//!
//! Provides the MCP (Model Context Protocol) server implementation for UFFS.
//! This bridges LLM hosts (Claude Desktop, Cursor, Windsurf, etc.) to the
//! UFFS daemon via the [`rmcp`] SDK.
//!
//! # Architecture
//!
//! ```text
//! LLM Host ──stdio──▶ UffsMcpServer ──UffsClient──▶ uffs-daemon
//! ```
//!
//! The server exposes UFFS tools, resources, and prompts over the MCP protocol.
//! It is **not** in the query data path — it merely bridges MCP framing to the
//! daemon's native protocol.
//!
//! # Concurrency
//!
//! Runs on `#[tokio::main]` (default multi-threaded runtime).  Spawns
//! a small number of long-lived tasks: the MCP stdio dispatcher (or
//! the `streamable-http` axum server when that feature is on), the
//! daemon-bridge `UffsClient` reader-loop, and per-RPC handler tasks
//! spawned by the rmcp SDK.  Per-RPC timeouts inherit the async
//! client's 300 s `read_line` ceiling; the reload pipeline in the
//! crate-private `process::mcp_reload` is intentionally sequential
//! (one-shot CLI context).  See
//! `docs/architecture/code-quality/concurrency_policy.md` for the
//! workspace contract.
//!
//! # Usage
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! uffs_mcp::run_mcp_server().await
//! # }
//! ```
//!
//! # Features
//!
//! Documented per the workspace dependency policy
//! (`docs/architecture/code-quality/dependency_policy.md`, playbook §988).
//!
//! | Feature | Default? | Enables | Adds deps | API impact | Semver |
//! |---|:---:|---|---|---|---|
//! | `streamable-http` | yes | The [`http`] module + the `uffs-mcp-http` binary (`src/bin/http_gateway.rs`).  Activates rmcp's streamable-HTTP server transport. | `rmcp/transport-streamable-http-server`, `dep:axum`, `dep:tower-service` | **Additive**: enabling adds `pub mod http`, the `mcp_serve` helper in `main.rs`, and the `uffs-mcp-http` binary (which carries `required-features = ["streamable-http"]` and so does not build at all when the feature is off).  All stdio surfaces ([`handler::UffsMcpServer`], `resources`, `roots`, `schemas`, [`text`], `tools`) compile identically with the feature off. | Removing an item behind `streamable-http` is breaking; adding one is not. |
//!
//! ## Why `streamable-http` is default-on
//!
//! UFFS ships the HTTP gateway as a first-class transport (the
//! stdio-only path covers Claude Desktop / Cursor / Windsurf, but
//! `uffs --daemon http` and browser-side MCP clients need the streamable
//! HTTP server).  Downstream MCP integrations that only want the
//! stdio transport can pass `default-features = false` to drop
//! axum and tower-service from their binary; both transports share
//! the same tool / resource / prompt surface.
//!
//! # Environment
//!
//! Env vars read by this crate (registry:
//! `docs/architecture/code-quality/build_codegen_policy.md` §5, playbook
//! §1049-1056):
//!
//! | Env var | Type | Default | Notes |
//! |---|---|---|---|
//! | `CARGO_PKG_VERSION` | `string` | (set by Cargo) | Read via `env!()` for the MCP `serverInfo.version` field.  CARGO semver class. |
//! | `UFFS_LOG` | `string` | `info` | Log level for `tracing-subscriber` (preferred over `RUST_LOG` for UFFS binaries).  INTERNAL semver class. |
//! | `UFFS_LOG_FILE` | `path` | (none) | Optional log-file path override for the MCP HTTP gateway binary.  INTERNAL semver class. |
//! | `UFFS_MCP_AUTH_TOKEN` | `token` | (auto-generated per session) | Bearer-token override for the MCP HTTP gateway binary in `bin/http_gateway.rs`.  INTERNAL semver class. |

// On docs.rs only: enable the `doc_cfg` rustdoc feature so cfg-gated items
// render with their cfg badge.  Gated behind `cfg(docsrs)` so local
// `cargo doc` never exercises the nightly-only feature.  Post-Rust-1.92
// the `doc_auto_cfg` feature was merged into `doc_cfg`
// (rust-lang/rust#138907).
#![cfg_attr(docsrs, feature(doc_cfg))]

// `clap` is used by the `uffsmcp` binary, not this library crate.
// Linked for this crate's binary target(s), which call
// `uffs_version::handle_version!` in `main`; the library itself does not
// reference it (sanctioned unused-crate-dependencies marker).
use clap as _;
use uffs_version as _;

// ── MCP tracing initialisation ────────────────────────────────────────

extern crate alloc;

/// Initialise tracing for the MCP server.
///
/// **Stdout is the protocol channel** — all logging MUST go to stderr or
/// to `log_file`.  Behaviour mirrors `uffs_daemon::init_tracing`:
///
/// * `UFFS_LOG=trace` sets the filter; default is `"info"`.
/// * `UFFS_LOG_FILE=/tmp/mcp.log` redirects to a file.  When a verbose level
///   (`debug`/`trace`) is active and no file is specified, a default file is
///   used so diagnostic output isn't lost.
///
/// Returns an optional guard that **must** be held for the lifetime of
/// the MCP server — dropping it flushes the non-blocking writer.
#[must_use]
pub fn init_mcp_tracing(
    log_spec: &str,
    log_file: Option<&std::path::Path>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = tracing_subscriber::EnvFilter::try_new(log_spec)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let is_verbose = {
        let lower = log_spec.to_ascii_lowercase();
        lower.contains("debug") || lower.contains("trace")
    };

    let effective_file: Option<std::path::PathBuf> = match log_file {
        Some(path) => {
            let resolved = if path.as_os_str().is_empty() || path == std::path::Path::new("-") {
                default_mcp_log_file()
            } else {
                path.to_path_buf()
            };
            Some(resolved)
        }
        None if is_verbose => Some(default_mcp_log_file()),
        None => None,
    };

    if let Some(resolved) = effective_file {
        if let Some(parent) = resolved.parent() {
            let _ignore = std::fs::create_dir_all(parent);
        }
        // Daily rotation with bounded retention — NOT `rolling::never`,
        // which appends forever with no size cap and no pruning. The daemon
        // used `never` and one production log reached 305 MB; the MCP server
        // is equally long-lived, so it gets the same bound rather than
        // waiting its turn to be the one that fills a disk.
        let parent_dir = resolved
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let stem = resolved
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("uffs_mcp");
        let extension = resolved
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("log");
        let built = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(stem)
            .filename_suffix(extension)
            .max_log_files(KEPT_MCP_LOG_FILES)
            .build(parent_dir);
        let Ok(file_appender) = built else {
            let _ignore = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .try_init();
            return None;
        };
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let _ignore = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_ansi(false)
            .with_writer(non_blocking)
            .try_init();
        Some(guard)
    } else {
        // Default: log to stderr (stdout is the MCP protocol channel).
        let _ignore = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .try_init();
        None
    }
}

/// Daily MCP log files kept before the oldest is pruned.
///
/// Matches the daemon's retention so both halves of an incident cover the
/// same window.
const KEPT_MCP_LOG_FILES: usize = 7;

/// Default log file path for MCP diagnostic sessions.
///
/// Uses the shared per-platform native log directory resolved by
/// [`uffs_security::log_dir::log_dir`] (overridable via `UFFS_LOG_DIR`);
/// an explicit `UFFS_LOG_FILE` still takes precedence at the call site.
fn default_mcp_log_file() -> std::path::PathBuf {
    uffs_security::log_dir::log_dir().join("uffs_mcp.log")
}

// Phase 3 module-layout: most submodules are crate-internal. Only
// `http` (bin/http_gateway.rs + main.rs `--gateway`) and `text`
// (main_tests.rs format_aggregate_summary) need cross-bin visibility.
// All other submodules are reachable only via internal `crate::*` paths.
/// Agent cookbook — curated example queries (backing `uffs://cookbook`).
pub(crate) mod cookbook;
/// MCP bridge error types.
pub(crate) mod error;
/// MCP [`rmcp::ServerHandler`] implementation.
///
/// Kept `pub` because `tests/mcp_protocol.rs` (integration test,
/// separate compilation unit) imports `uffs_mcp::handler::UffsMcpServer`.
pub mod handler;
/// Streamable HTTP gateway (feature-gated).
#[cfg(feature = "streamable-http")]
pub mod http;
/// Static and live MCP resource implementations.
pub(crate) mod resources;
/// MCP roots mapping policy.
pub(crate) mod roots;
/// Output schema types for `outputSchema` / `structuredContent`.
pub(crate) mod schemas;
/// MCP server runtime statistics (lock-free counters).
pub(crate) mod stats;
/// Human-readable text formatting for tool responses.
pub mod text;
/// Individual MCP tool handlers.
pub(crate) mod tools;

// tower-service is used by http::tests — suppress unused-crate-dep warning.
use core::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use rmcp::ServiceExt as _;
#[cfg(feature = "streamable-http")]
use tower_service as _;
use tracing::info;

// ── Configuration ───────────────────────────────────────────────────

/// Configuration for the MCP server.
///
/// # Field discipline (Phase 3b §3.4)
///
/// Both fields are `pub` because this is a **configuration DTO**.
/// `daemon_spawn_args` is a forwarded `Vec<OsString>` (no invariants to
/// protect); `idle_timeout_secs == 0` is the documented sentinel for
/// "no timeout" and is validated at the call site, not in a setter.
///
/// # `#[non_exhaustive]` decision (Phase 3b §3.6)
///
/// **Kept exhaustive.**  `uffs-mcp` is a bin-dominant internal app
/// (Layer 4 in `docs/architecture/crate-graph.md`), not externally
/// consumed; the only struct-literal construction lives in this
/// crate's own bin (`src/main.rs`) and tests.  If `uffs-mcp` ever
/// publishes a library facade for embedding the MCP server in
/// external apps, revisit and add `#[non_exhaustive]` plus an
/// `McpConfigBuilder`.
#[derive(Debug, Clone)]
pub struct McpConfig {
    /// Extra CLI args forwarded to `uffs --daemon run` when auto-starting
    /// (e.g. `["--data-dir", "/path"]`).
    pub daemon_spawn_args: Vec<std::ffi::OsString>,
    /// Idle timeout in seconds.  The MCP server will auto-exit if no
    /// MCP messages are received within this period.  `0` = no timeout.
    pub idle_timeout_secs: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            daemon_spawn_args: Vec::new(),
            idle_timeout_secs: 7200,
        }
    }
}

// ── Server entry points ─────────────────────────────────────────────

/// Run the MCP server on stdin/stdout with the given configuration.
///
/// Writes a PID file on start and removes it on exit.  Connects to the
/// UFFS daemon (auto-starting with forwarded args if needed), creates the
/// [`handler::UffsMcpServer`], and serves MCP over stdio until the client
/// disconnects or the idle timeout fires.
///
/// # Errors
///
/// Returns an error if the daemon connection fails or the MCP transport
/// encounters an I/O error.
#[cfg_attr(
    feature = "streamable-http",
    expect(
        clippy::cognitive_complexity,
        reason = "MCP server startup with daemon connect, idle timer, and transport orchestration"
    )
)]
pub async fn run_mcp_server_with_config(config: &McpConfig) -> anyhow::Result<()> {
    info!(
        idle_timeout = config.idle_timeout_secs,
        daemon_args = ?config.daemon_spawn_args,
        "UFFS MCP server starting (rmcp)…"
    );

    // Stdio servers do NOT write PID files — multiple stdio sessions
    // (one per AI host) can coexist.  Only the HTTP gateway writes a
    // PID file (via `write_mcp_pid_file_with_transport` in http.rs).

    // Connect to the daemon (auto-starts with forwarded args if needed)
    // just to perform the readiness handshake.  The handler itself uses
    // a per-call connection model (see `handler::ClientSlot`), so this
    // client is intentionally dropped once the readiness gate passes —
    // the first tool call will open its own fresh connection.
    let mut client = uffs_client::connect::UffsClient::connect_with_args(&config.daemon_spawn_args)
        .await
        .context("Failed to connect to UFFS daemon")?;

    // Wait for the daemon to finish loading indices before serving MCP
    // requests.  Without this, tool calls hit empty data when the daemon
    // was just auto-started.
    info!("Connected to daemon, waiting for indices to load…");
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .context("Daemon did not become ready within 120s")?;
    drop(client);
    info!("Daemon ready, starting MCP stdio transport…");

    let server = handler::UffsMcpServer::new(config.daemon_spawn_args.clone());

    // Capture the activity handle BEFORE `serve` consumes the server.
    // Every MCP tool call / resource read / prompt list calls `touch()`,
    // which stores the current epoch-second into this atomic.  The
    // sliding-window loop below uses it to extend the idle deadline.
    let last_activity = server.last_activity_handle();

    // Serve MCP over stdin/stdout using rmcp's stdio transport.
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;

    if config.idle_timeout_secs > 0 {
        let timeout_secs = config.idle_timeout_secs;
        let ct = service.cancellation_token();

        // Race: transport close (client disconnect) vs sliding-window idle.
        //
        // `service.waiting()` takes ownership, so it cannot be used inside a
        // loop.  Instead the sliding-window logic lives in a self-contained
        // async helper (`wait_for_genuine_idle`) which only resolves once the
        // idle window has truly expired.
        tokio::select! {
            result = service.waiting() => {
                result?;
                info!("MCP server shut down (client disconnected).");
            }
            () = wait_for_genuine_idle(&last_activity, timeout_secs) => {
                info!(
                    timeout_secs,
                    "MCP server idle timeout — shutting down."
                );
                ct.cancel();
            }
        }
    } else {
        // No timeout — wait for client disconnect only.
        service.waiting().await?;
        info!("MCP server shut down cleanly.");
    }

    // PID file removed by _pid_guard drop.
    Ok(())
}

/// Sliding-window idle timer that resolves only on genuine inactivity.
///
/// # Algorithm
///
/// 1. Sleep for the full `timeout_secs` window.
/// 2. On wake, read `last_activity` (epoch seconds set by every MCP request).
/// 3. If activity occurred during the sleep, compute the remaining time until
///    `last_activity + timeout_secs` and sleep again for exactly that long.
///    This avoids both polling and unnecessary wakeups.
/// 4. Repeat until no activity has occurred for a full window.
///
/// Each iteration creates at most one new [`tokio::time::Sleep`], which is
/// negligible overhead for a multi-hour timeout window.
async fn wait_for_genuine_idle(last_activity: &core::sync::atomic::AtomicU64, timeout_secs: u64) {
    let mut remaining = core::time::Duration::from_secs(timeout_secs);
    loop {
        tokio::time::sleep(remaining).await;

        let last_secs = last_activity.load(Ordering::Relaxed);
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs());
        let elapsed_since_activity = now_secs.saturating_sub(last_secs);

        if elapsed_since_activity >= timeout_secs {
            // Genuinely idle — no MCP request for a full timeout window.
            return;
        }

        // Activity extended the window.  Sleep for the precise remainder.
        remaining = core::time::Duration::from_secs(timeout_secs - elapsed_since_activity);
        info!(
            remaining_secs = remaining.as_secs(),
            "MCP idle deadline extended — activity within window"
        );
    }
}

/// Run the MCP server on stdin/stdout with default configuration.
///
/// Convenience wrapper around [`run_mcp_server_with_config`] using
/// [`McpConfig::default`].
///
/// # Errors
///
/// Returns an error if the daemon connection fails or the MCP transport
/// encounters an I/O error.
pub async fn run_mcp_server() -> anyhow::Result<()> {
    run_mcp_server_with_config(&McpConfig::default()).await
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
