// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Thin client library for the UFFS daemon.
//!
//! All surfaces (CLI, TUI, GUI, MCP) use this crate to communicate with
//! the daemon. It handles auto-start, connection, keepalive, and reconnect.
//!
//! # Concurrency
//!
//! Hybrid runtime model:
//!
//! * [`connect::UffsClient`] (async; feature `async` default-on) — runs on the
//!   caller's tokio runtime.  Per-RPC `read_line` ceiling 300 s
//!   (`Duration::from_mins(5)`; hard-coded).  Notification channel is
//!   `mpsc::unbounded_channel()` rate-bounded by the daemon's
//!   `broadcast::Sender` capacity (Phase 10d C5 verdict).
//! * [`connect_sync::UffsClientSync`] (sync) — synchronous I/O on the caller
//!   thread.  Per-RPC deadline 600 s default, env-overridable via
//!   `UFFS_CLIENT_TIMEOUT_SECS`; Windows path uses
//!   `windows_deadline::WindowsDeadlineGuard` (a `CancelSynchronousIo`
//!   watchdog, `#[cfg(windows)]`-only) to enforce the deadline on blocking pipe
//!   I/O.
//!
//! See `docs/architecture/code-quality/concurrency_policy.md` for the
//! workspace contract.
//!
//! # Example
//!
//! ```rust,ignore
//! let client = UffsClient::connect().await?;
//! let results = client.search("*.rs").await?;
//! let drives = client.drives().await?;
//! ```
//!
//! # Features
//!
//! Documented per the workspace dependency policy
//! (`docs/architecture/code-quality/dependency_policy.md`, playbook §988).
//!
//! | Feature | Default? | Enables | Adds deps | API impact | Semver |
//! |---|:---:|---|---|---|---|
//! | `async` | yes | The async [`connect::UffsClient`] (tokio reactor) used by `uffs-daemon`'s embedded client + `uffs-mcp`'s bridge | `dep:tokio` (with the `net` feature) | **Additive**: enabling adds [`connect`], `connect_keepalive`, `connect_logging`, `connect_platform`, and the async test surface.  All sync items ([`connect_sync::UffsClientSync`], [`protocol`], [`schema`]) compile identically with the feature off. | Removing an item behind `async` is breaking; adding one is not. |
//!
//! ## Why `async` is default-on
//!
//! `uffs-daemon` and `uffs-mcp` both consume `uffs-client` with default
//! features so the async [`connect::UffsClient`] is available without
//! an explicit feature opt-in.  `uffs-cli` overrides with
//! `default-features = false` (see `crates/uffs-cli/Cargo.toml`) to
//! drop tokio + `ws2_32.dll` from the sync-CLI binary — the canonical
//! "consumer chooses to shrink the surface" pattern.
//!
//! ## API hygiene policy (Phase 3b §3.4 / §3.6 / §3.7)
//!
//! - **`protocol::*` wire DTOs / wire enums** — `pub` fields **are** the
//!   contract (`serde` JSON keys 1:1; §3.4).  Kept exhaustive (§3.6): monorepo
//!   deploys daemon + client together (no skew scenario), hundreds of
//!   struct-literal construction sites, and the wire enums
//!   (`SearchPredicateOp`, `SearchPayload`, `DaemonStatus`, …) are dispatch
//!   enums where exhaustive `match` is the compile-time safety net.  Revisit
//!   when this crate publishes externally.
//! - **`schema::*` field-metadata DTOs / enums** — same field-discipline
//!   reasoning as `protocol`; closed type-code enums by definition.
//! - **`UffsClient` / `UffsClientSync`** — fields private, smart constructors
//!   protect reader/writer pairing and `next_id` monotonicity invariants; sync
//!   sibling adds the Windows `deadline_guard` watchdog-already-spawned
//!   invariant.  See the focused decision record on `connect::UffsClient`.
//! - **`ClientError`** — `#[non_exhaustive]` applied (the lone API attribute
//!   change in Phase 3b); safe under both external usage patterns in
//!   `uffs-cli`.
//! - **§3.7** N/A — no `pub trait` declarations in this crate.
//!
//! # Environment
//!
//! Env vars read by this crate (registry:
//! `docs/architecture/code-quality/build_codegen_policy.md` §5, playbook
//! §1049-1056):
//!
//! | Env var | Type | Default | Notes |
//! |---|---|---|---|
//! | `CARGO_PKG_VERSION` | `string` | (set by Cargo) | Read via `env!()` for `ResponseStatus::version`.  CARGO semver class. |
//! | `UFFS_CLIENT_SKIP_HEALTH_CHECK` | `bool` | `false` | Skips post-connect health probe in [`daemon_ctl`].  Used by `just ship` cross-check.  INTERNAL semver class. |
//! | `UFFS_CLIENT_TIMEOUT_SECS` | `int` (seconds) | `600` | Per-RPC deadline override (`0` disables it); sync default 600 s in `connect_sync_platform`, async default 300 s in `connect`.  INTERNAL semver class. |
//! | `UFFS_ELEVATE` | `string` (`auto` / `never` / `always` / `prefer`) | `auto` | Elevation policy for daemon spawn; read in `daemon_spawn::resolve_elevation_policy` (private module).  INTERNAL semver class. |
//! | `XDG_RUNTIME_DIR` | `path` | (XDG: `/run/user/$UID`) | Linux daemon-socket location in [`daemon_ctl`].  STANDARD semver class. |

// On docs.rs only: enable the `doc_cfg` rustdoc feature so cfg-gated items
// (`#[cfg(feature = "async")]`, `#[cfg(windows)]`, etc.) render with their
// cfg badge.  Gated behind `cfg(docsrs)` so local `cargo doc` never
// exercises the nightly-only feature.  Post-Rust-1.92 the `doc_auto_cfg`
// feature was merged into `doc_cfg` (rust-lang/rust#138907).
#![cfg_attr(docsrs, feature(doc_cfg))]

// Suppress unused crate warnings for deps used in sub-modules
use serde as _;
use uffs_security as _;

/// Async `UffsClient` over tokio — used by the MCP gateway and daemon.
///
/// Gated behind the `async` feature so the sync CLI can drop tokio (and
/// `ws2_32.dll`) from its binary.
#[cfg(feature = "async")]
pub mod connect;
/// Connect / auto-start entry points for [`connect::UffsClient`] — split
/// off `connect.rs` to keep that file under the 800-LOC policy ceiling
/// after the drive-load-progress `await_ready` resilience work grew it.
#[cfg(feature = "async")]
mod connect_autostart;
/// Background keepalive task + `KeepaliveGuard` for long-lived clients.
///
/// `start_keepalive` is re-attached to `UffsClient` via a split `impl`;
/// external callers import `KeepaliveGuard` directly from this module
/// (no cascade through `connect`).
#[cfg(feature = "async")]
pub(crate) mod connect_keepalive;
/// Tracing helpers used only by [`connect`].  Private; sibling file
/// to keep `connect.rs` under the 800-LOC file-size policy after the
/// v0.5.36 UAC work expanded its public entry points.
#[cfg(feature = "async")]
mod connect_logging;
/// Platform-specific `platform_connect` impls for [`connect::UffsClient`].
///
/// Split `impl` blocks live on `UffsClient` via
/// [`connect::UffsClient::from_parts`]; callers see no change.
/// Extracted after the Run 10 Part B `cached_status` addition pushed
/// `connect.rs` over the 800-LOC policy ceiling.
#[cfg(feature = "async")]
mod connect_platform;
pub mod connect_sync;
/// Auto-start daemon helpers (`auto_start_daemon`, `is_process_alive`,
/// `is_daemon_process`) — split off `connect_sync` to keep that file
/// under the 800-LOC policy ceiling.
pub(crate) mod connect_sync_autostart;
/// Platform-specific `platform_connect` impls and the `rpc_deadline` helper.
///
/// Split `impl` blocks live on [`connect_sync::UffsClientSync`];
/// callers see no change.  Also hosts the env-override regression
/// tests for `rpc_deadline`.
/// Transport-error classification for the sync client: the client's own
/// deadline surfaces as `ClientError::Timeout`, never as a raw `Io`.
pub(crate) mod connect_sync_errors;
/// Memory-tiering RPC helpers (`hibernate`, `preload`).
///
/// Phase 8-B / 8-C — split off `connect_sync` so the tiering cluster
/// stays under the 800-LOC policy ceiling without a file-size
/// exception.  Same precedent as the daemon-state types in
/// [`protocol::response_status`].
pub(crate) mod connect_sync_journal;
pub(crate) mod connect_sync_platform;
pub use connect_sync_platform::rpc_deadline;
/// Wire-protocol unit tests for [`connect_sync::UffsClientSync`].
///
/// Exercises the JSON-RPC request/response path via in-memory
/// reader/writer halves (no real socket).  `#[cfg(test)]` keeps it
/// out of release builds entirely.
#[cfg(test)]
mod connect_sync_tests;
pub(crate) mod connect_sync_tiering;
/// Wire-protocol unit tests for [`connect::UffsClient`].
///
/// Exercises the JSON-RPC request/response path via in-memory tokio
/// `AsyncRead` / `AsyncWrite` halves (no real socket).  `#[cfg(test)]`
/// keeps it out of release builds entirely; gated on the `async`
/// feature so it compiles only when [`connect::UffsClient`] itself does.
#[cfg(test)]
#[cfg(feature = "async")]
mod connect_tests;
/// Child-process handle for spawned daemons.
///
/// Exposes `DaemonChildHandle`, `try_wait`, and the platform-specific
/// cleanup logic.  Canonical home — no cascade through `daemon_ctl`.
pub(crate) mod daemon_child;
pub mod daemon_ctl;
/// Daemon spawn implementation.
///
/// Exposes `spawn_daemon`, `ElevationPolicy`, the MSVCRT-compatible
/// arg quoter, and the Windows UAC helpers.  Canonical home — no
/// cascade through `daemon_ctl`.
/// Resident-marker merge for the auto-spawn path (split off
/// `daemon_spawn` to stay under the 800-LOC policy ceiling).
pub(crate) mod daemon_resident;
pub(crate) mod daemon_spawn;
pub mod error;
pub mod format;
pub mod mcp_pid;
pub mod protocol;
pub mod shmem;
pub mod stdout_kind;
// Phase 3: types and verify have zero external module-path use.
pub(crate) mod types;
pub(crate) mod verify;
/// Windows-only per-RPC deadline enforcement.
///
/// Background watchdog thread that cancels synchronous I/O on the
/// owning thread when an armed deadline expires.  See the module
/// docs for the full design rationale.
#[cfg(windows)]
pub(crate) mod windows_deadline;

/// Access Broker detection (Windows-only): probe the broker named pipe so
/// daemon-spawn can skip the elevation gate when a broker is running.
#[cfg(windows)]
pub(crate) mod broker_probe;

pub mod schema;
