#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! toml = "0.8"
//! anyhow = "1"
//! colored = "2"
//! dirs-next = "2"
//!
//! [target.'cfg(windows)'.dependencies]
//! uds_windows = "1"
//! ```
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//!
//! api-validation.rs — Pure JSON-RPC API validation test suite for UFFS daemon.
//!
//! ALL tests communicate with the daemon exclusively via JSON-RPC over a
//! Unix-domain socket (Windows named pipe on Windows).  No CLI binary is
//! spawned.  This validates the daemon's API surface end-to-end.
//!
//! Startup timing (COLD → WARM → HOT) lives in scripts/dev/daemon-readiness.rs.
//!
//! Usage:
//!   rust-script scripts/windows/api-validation.rs
//!   rust-script scripts/windows/api-validation.rs --filter "T04"
//!   rust-script scripts/windows/api-validation.rs --data-dir /Users/me/uffs_data
//!   rust-script scripts/windows/api-validation.rs --bin target/release/uffs

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde_json::{json, Value};

// ── Process-level lock ────────────────────────────────────────────────────────

/// RAII lock guard.  Only one validation script (CLI *or* API) can run
/// at a time because they share the daemon.
///
/// Uses a PID-file at `/tmp/uffs-validation.lock`.  If a stale lock is
/// detected (owner PID no longer alive), it is forcibly removed.
struct ValidationLock {
    path: PathBuf,
}

impl ValidationLock {
    const LOCK_PATH: &'static str = "/tmp/uffs-validation.lock";
    const MAX_WAIT_SECS: u64 = 120;
    const POLL_MS: u64 = 500;

    /// Acquire the lock, waiting up to `MAX_WAIT_SECS`.
    fn acquire() -> Self {
        let path = PathBuf::from(Self::LOCK_PATH);
        let my_pid = std::process::id();
        let start = Instant::now();
        let mut warned = false;

        loop {
            // Try to read existing lock.
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if let Ok(owner_pid) = contents.trim().parse::<u32>() {
                    if Self::pid_alive(owner_pid) {
                        // Another instance is running.
                        if !warned {
                            eprintln!(
                                "  {} Another validation script (PID {}) is running — waiting...",
                                "⏳".yellow(), owner_pid
                            );
                            warned = true;
                        }
                        if start.elapsed().as_secs() >= Self::MAX_WAIT_SECS {
                            eprintln!(
                                "  {} Timed out waiting for lock (PID {} held it for {}s)",
                                "❌".red(), owner_pid, Self::MAX_WAIT_SECS
                            );
                            std::process::exit(1);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(Self::POLL_MS));
                        continue;
                    }
                    // Stale lock — owner is dead.
                    eprintln!(
                        "  {} Removing stale lock (PID {} is no longer running)",
                        "🔓".yellow(), owner_pid
                    );
                }
                // Invalid or stale — remove and retry.
                let _ = std::fs::remove_file(&path);
            }

            // Try to write our PID atomically.
            // Use create_new (O_EXCL) to avoid TOCTOU race.
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = write!(f, "{my_pid}");
                    if warned {
                        eprintln!("  {} Lock acquired after {}s",
                            "🔒".green(), start.elapsed().as_secs());
                    }
                    return Self { path };
                }
                Err(_) => {
                    // Another process won the race — loop back.
                    std::thread::sleep(std::time::Duration::from_millis(Self::POLL_MS));
                }
            }
        }
    }

    /// Check if a PID is still alive (Unix-only).
    fn pid_alive(pid: u32) -> bool {
        // kill(pid, 0) checks existence without sending a signal.
        libc_kill(pid as i32, 0) == 0
    }
}

impl Drop for ValidationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// Minimal FFI: `kill(pid, sig)` — returns 0 if process exists.
#[cfg(unix)]
extern "C" { fn kill(pid: i32, sig: i32) -> i32; }
#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) -> i32 { unsafe { kill(pid, sig) } }
#[cfg(not(unix))]
fn libc_kill(_pid: i32, _sig: i32) -> i32 { -1 }


// ── Debug Logging ────────────────────────────────────────────────────────────

use std::sync::Mutex;

/// Returns `true` when `UFFS_API_LOG` is set (any non-empty value).
fn log_enabled() -> bool {
    std::env::var("UFFS_API_LOG").map_or(false, |v| !v.is_empty())
}

/// Lazy-initialised log file handle.
/// Log file path comes from `UFFS_API_LOG` env var.
fn log_write(msg: &str) {
    use std::io::Write;
    use std::sync::OnceLock;
    static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
    let mtx = LOG.get_or_init(|| {
        let path = std::env::var("UFFS_API_LOG").unwrap_or_else(|_| "/tmp/uffs-api-validation.log".into());
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("Cannot open log file {path}: {e}"));
        Mutex::new(f)
    });
    if let Ok(mut f) = mtx.lock() {
        let _ = writeln!(f, "{msg}");
    }
}

// ── CLI Args ─────────────────────────────────────────────────────────────────

struct ScriptArgs {
    /// `"--data-dir"` or `"--mft-file"`, or `None` for Windows live drives.
    source_flag: Option<&'static str>,
    /// The path value for the flag (empty when using live drives).
    source_path: String,
    filter: Option<String>,
    bin: String,
}

/// Walk up from CWD to find the workspace root (has Cargo.toml + .cargo).
fn find_workspace_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = cwd.as_path();
    loop {
        if dir.join("Cargo.toml").exists() && dir.join(".cargo").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    cwd
}

/// Locate an existing uffs binary; do **not** auto-build.
///
/// Validation scripts run against whatever artifact is on disk so the
/// user can control which build is being exercised (release tag,
/// local `cargo build --release`, or installed `just use` payload).
/// Auto-rebuilding from inside the script masks the very mismatch
/// these suites are meant to detect.
///
/// Search order (cross-platform):
///   1. `$HOME/bin/uffs[.exe]`           — `just use` install location
///   2. `target/release/uffs[.exe]`      — `cargo build --release` output
///   3. Bare `uffs[.exe]`                — falls through to PATH lookup
///
/// If none exists, the first `Command::new(bin).output()` call surfaces
/// the OS's "executable not found" error with the path-search
/// diagnostic — clearer than a fabricated panic from this layer.
fn default_binary() -> String {
    let bin_name = if cfg!(windows) { "uffs.exe" } else { "uffs" };
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = std::env::var(home_var).unwrap_or_else(|_| ".".to_string());
    let candidates = [
        PathBuf::from(&home).join("bin").join(bin_name),
        PathBuf::from("target").join("release").join(bin_name),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    bin_name.to_string()
}

/// Run `<bin> daemon status` and extract the daemon's reported
/// version string for inclusion in the validation-summary block.
///
/// Returns the trimmed value of the `Version:` line printed by
/// `uffs --daemon status` on uffs ≥ 0.5.79.  Pre-0.5.79 daemons emit
/// `<unknown> (daemon) / X.Y.Z (cli)` via the CLI's back-compat
/// renderer; pre-this-feature daemons (no `Version:` line at all)
/// surface as `<line not found>`.  When the daemon is unreachable
/// the helper reports `<not running>` instead of erroring — the
/// version line is informational, not load-bearing.
fn capture_daemon_version(bin: &str) -> String {
    match Command::new(bin).args(["--daemon", "status"]).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("Version:") {
                    return rest.trim().to_owned();
                }
            }
            "<line not found>".to_owned()
        }
        _ => "<not running>".to_owned(),
    }
}

/// Detect whether the user passed a file or directory and return the
/// appropriate uffs CLI flag + value.
fn detect_data_source(path: &str) -> Result<(&'static str, String)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        bail!("Path does not exist: {path}");
    }
    if p.is_file() {
        Ok(("--mft-file", path.to_owned()))
    } else if p.is_dir() {
        Ok(("--data-dir", path.to_owned()))
    } else {
        bail!("Path is neither a file nor a directory: {path}");
    }
}

fn parse_args() -> ScriptArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut path: Option<String> = None;
    let mut filter = None;
    let mut bin = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" | "--tests" => { i += 1; filter = args.get(i).cloned(); }
            "--bin" | "-b" | "--binary" => { i += 1; bin = args.get(i).cloned(); }
            other if !other.starts_with('-') && path.is_none() => {
                path = Some(other.to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let (source_flag, source_path) = match path {
        Some(ref p) => {
            match detect_data_source(p) {
                Ok((flag, val)) => (Some(flag), val),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        None if !cfg!(windows) => {
            // Default to ~/uffs_data on non-Windows.
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let default = format!("{home}/uffs_data");
            if std::path::Path::new(&default).is_dir() {
                eprintln!("  (defaulting to {default})");
                (Some("--data-dir"), default)
            } else {
                eprintln!("Error: No PATH given and ~/uffs_data not found.\n");
                eprintln!("Usage:");
                eprintln!("  rust-script scripts/windows/api-validation.rs ~/uffs_data");
                eprintln!("  rust-script scripts/windows/api-validation.rs /path/to/C_mft.iocp");
                std::process::exit(1);
            }
        }
        None => {
            // Windows: auto-discover live NTFS drives.
            (None, String::new())
        }
    };

    ScriptArgs {
        source_flag,
        source_path,
        filter,
        bin: bin.unwrap_or_else(default_binary),
    }
}

// ── JSON-RPC Client ─────────────────────────────────────────────────────────

static REQ_ID: AtomicU64 = AtomicU64::new(1);

/// Resolve the daemon socket path.
///
/// The daemon ALWAYS creates its socket at the platform default location
/// (`dirs_next::data_local_dir()/uffs/daemon.sock`), regardless of
/// `--data-dir` (which only controls where MFT index data lives).
/// Must match `IpcServer::socket_path()` in the daemon crate.
fn daemon_socket_path() -> String {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let p = base.join("uffs").join("daemon.sock");
    p.to_string_lossy().to_string()
}

/// Send a JSON-RPC request and return the parsed response.
///
/// Returns `Err` on transport failures (connect, parse) AND on
/// JSON-RPC `error` responses.  For negative-path tests that want
/// to inspect the error object, pass the full response by calling
/// [`rpc_call_allow_error`] or set `allow_error = true` via
/// `rpc_call_impl`.
fn rpc_call(sock_path: &str, method: &str, params: Option<Value>) -> Result<Value> {
    rpc_call_impl(sock_path, method, params, false)
}

/// Like [`rpc_call`] but returns the full JSON-RPC response even when
/// the daemon responds with an `error` object.  Used by expected-
/// error tests (e.g. `RPC.5`) so the test's validator can inspect
/// `error.code` / `error.message`.
fn rpc_call_allow_error(sock_path: &str, method: &str, params: Option<Value>) -> Result<Value> {
    rpc_call_impl(sock_path, method, params, true)
}

/// Shared transport implementation for [`rpc_call`] and
/// [`rpc_call_allow_error`].  `allow_error` controls whether a JSON-
/// RPC `error` response becomes `Err("RPC error: ...")` (false, the
/// default) or is returned as `Ok(resp)` with the error object intact
/// (true, used by negative-path tests).
fn rpc_call_impl(
    sock_path: &str,
    method: &str,
    params: Option<Value>,
    allow_error: bool,
) -> Result<Value> {
    let id = REQ_ID.fetch_add(1, Ordering::Relaxed);
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    let payload = serde_json::to_string(&req)?;

    if log_enabled() {
        log_write(&format!("[RPC-REQ id={id}] method={method}"));
        if let Ok(pretty) = serde_json::to_string_pretty(&req) {
            log_write(&format!("[RPC-REQ id={id}] payload:\n{pretty}"));
        }
    }

    #[cfg(windows)]
    let mut stream = {
        uds_windows::UnixStream::connect(sock_path)
            .context("Cannot connect to daemon socket (AF_UNIX)")?
    };

    #[cfg(not(windows))]
    let mut stream = {
        std::os::unix::net::UnixStream::connect(sock_path)
            .context("Cannot connect to daemon socket")?
    };

    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(&mut stream);

    // Read lines until we get the actual response.
    // The daemon may send notifications (e.g. daemon.connection_changed)
    // before the response — these have a "method" field but no "id".
    // Skip them and keep reading.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() { continue; }

        let resp: Value = serde_json::from_str(line.trim())
            .context("Failed to parse JSON-RPC response")?;

        // Notification = has "method" but no "result" and no "error".
        // Skip it and read the next line.
        if resp.get("method").is_some() && resp.get("result").is_none() && resp.get("error").is_none() {
            continue;
        }

        if let Some(err) = resp.get("error") {
            if log_enabled() {
                log_write(&format!("[RPC-ERR id={id}] {}", serde_json::to_string_pretty(err).unwrap_or_default()));
            }
            if allow_error {
                // Caller asked for the full response (e.g. negative-path
                // test).  Fall through and return `resp`.
                return Ok(resp);
            }
            bail!("RPC error: {}", serde_json::to_string_pretty(err)?);
        }

        if log_enabled() {
            // Log response summary (rows count, aggregations count) to
            // avoid flooding with 25M rows of JSON.
            let rows_n = resp.pointer("/result/rows").and_then(|v| v.as_array()).map_or(0, |a| a.len());
            let aggs_n = resp.pointer("/result/aggregations").and_then(|v| v.as_array()).map_or(0, |a| a.len());
            let dur = resp.pointer("/result/duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            let scanned = resp.pointer("/result/records_scanned").and_then(|v| v.as_u64()).unwrap_or(0);
            log_write(&format!("[RPC-RSP id={id}] rows={rows_n} aggs={aggs_n} scanned={scanned} daemon_ms={dur}"));
            // Log first 3 rows if present.
            if let Some(rows) = resp.pointer("/result/rows").and_then(|v| v.as_array()) {
                for (i, row) in rows.iter().take(3).enumerate() {
                    log_write(&format!("[RPC-RSP id={id}] row[{i}]: {}", serde_json::to_string(row).unwrap_or_default()));
                }
            }
            // Log full aggregation results.
            if let Some(aggs) = resp.pointer("/result/aggregations").and_then(|v| v.as_array()) {
                for (i, agg) in aggs.iter().enumerate() {
                    let kind = agg.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let buckets = agg.get("buckets").and_then(|v| v.as_array()).map_or(0, |a| a.len());
                    let value = agg.get("value");
                    log_write(&format!("[RPC-RSP id={id}] agg[{i}]: kind={kind} buckets={buckets} value={value:?}"));
                }
            }
        }

        return Ok(resp);
    }
}


// ── Response Helpers ─────────────────────────────────────────────────────────

/// Extract the row list from a `SearchResponse` JSON value, handling
/// both the legacy flat `rows` field and the v0.5.62+ tagged
/// `payload` enum.
///
/// # Wire shapes handled
///
/// - **Legacy (pre-v0.5.62):** `{ "rows": [...] }` — a flat array on
///   the result root.  Kept for forward-compat with any snapshot
///   replayers.
/// - **Current (tagged enum):** `{ "payload": { "kind": "inline_rows",
///   "data": [...] } }` — the unified `SearchPayload` channel.
///   Returns the inline row list verbatim.
/// - **Empty / blob / shmem payload:** returns `vec![]`.  The harness
///   treats these as "zero rows" which is correct for
///   `expect_min_rows` assertions (blob variants never reach API
///   callers because MCP-style requests leave `output_format = None`,
///   keeping the payload as `inline_rows`).
/// - **Projected-JSON mode:** `{ "projected_rows": [...] }` — each row
///   is a `{column: value}` map.  Returned as-is since the column
///   checks only inspect named fields.
fn get_rows(result: &Value) -> Vec<&Value> {
    if let Some(rows) = result.get("rows").and_then(|v| v.as_array()) {
        return rows.iter().collect();
    }
    if let Some(payload) = result.get("payload") {
        let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "inline_rows" {
            if let Some(data) = payload.get("data").and_then(|v| v.as_array()) {
                return data.iter().collect();
            }
        }
    }
    if let Some(proj) = result.get("projected_rows").and_then(|v| v.as_array()) {
        return proj.iter().collect();
    }
    Vec::new()
}

/// Return the `SearchPayload` kind string (`"empty"`, `"inline_rows"`,
/// `"shmem_rows"`, `"inline_blob"`, `"shmem_blob"`) or `""` when the
/// result predates the v0.5.62 payload unification.
fn payload_kind(result: &Value) -> &str {
    result
        .get("payload")
        .and_then(|p| p.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// `result.shmem_path` equivalent for the post-unification wire shape.
///
/// - Legacy flat field `shmem_path` → returned verbatim.
/// - Payload kind `shmem_rows`   → `payload.data.path`.
/// - Payload kind `shmem_blob`   → `payload.data` (string).
/// - Anything else              → `None`.
fn payload_shmem_path(result: &Value) -> Option<&str> {
    if let Some(s) = result.get("shmem_path").and_then(Value::as_str) {
        return Some(s);
    }
    let payload = result.get("payload")?;
    match payload_kind(result) {
        "shmem_rows" => payload
            .get("data")
            .and_then(|d| d.get("path"))
            .and_then(Value::as_str),
        "shmem_blob" => payload.get("data").and_then(Value::as_str),
        _ => None,
    }
}

/// `result.shmem_count` equivalent for the post-unification wire shape.
///
/// Only the `shmem_rows` variant carries a count; `shmem_blob` is a
/// pre-formatted byte buffer with no row granularity.
fn payload_shmem_count(result: &Value) -> Option<u64> {
    if let Some(n) = result.get("shmem_count").and_then(Value::as_u64) {
        return Some(n);
    }
    if payload_kind(result) == "shmem_rows" {
        return result
            .get("payload")
            .and_then(|p| p.get("data"))
            .and_then(|d| d.get("count"))
            .and_then(Value::as_u64);
    }
    None
}

/// Virtualise legacy top-level SearchResponse keys against the
/// v0.5.62+ `payload` enum.
///
/// Lets TOML test specs keep using human-readable
/// `result_has_key = ["rows"]` / `["shmem_path"]` assertions without
/// every TOML file needing to be rewritten to the new enum shape.
/// The mapping enumerated here must stay in lock-step with the
/// [`SearchPayload`] variants in
/// `crates/uffs-client/src/protocol/response.rs`.
fn result_has_virtual_key(result: &Value, key: &str) -> bool {
    if result.get(key).is_some() {
        return true;
    }
    let kind = payload_kind(result);
    match key {
        // "rows" is the row list; empty / inline_rows / shmem_rows
        // all expose row data (empty may legitimately mean zero
        // rows — `T40 no results` sets `total_count_min = 0`).
        "rows" => matches!(kind, "empty" | "inline_rows" | "shmem_rows"),
        // shmem transport surfaces as either variant.
        "shmem_path" => matches!(kind, "shmem_rows" | "shmem_blob"),
        "shmem_count" => kind == "shmem_rows",
        _ => false,
    }
}


fn field_str(row: &Value, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn field_u64(row: &Value, key: &str) -> u64 {
    row.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}


fn field_bool(row: &Value, key: &str) -> bool {
    row.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn has_flag(row: &Value, flag: u32) -> bool {
    let flags = row.get("flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    (flags & flag) != 0
}

#[allow(dead_code)] // NTFS flag constants — kept complete for future tests.
mod ntfs_flags {
    pub const FLAG_READONLY: u32  = 0x0001;
    pub const FLAG_HIDDEN: u32    = 0x0002;
    pub const FLAG_SYSTEM: u32    = 0x0004;
    pub const FLAG_DIRECTORY: u32 = 0x0010;
    pub const FLAG_ARCHIVE: u32   = 0x0020;
    pub const FLAG_SPARSE: u32    = 0x0200;
    pub const FLAG_REPARSE: u32   = 0x0400;
    pub const FLAG_COMPRESSED: u32= 0x0800;
    pub const FLAG_OFFLINE: u32   = 0x1000;
    pub const FLAG_ENCRYPTED: u32 = 0x4000;
}
use ntfs_flags::*;

fn get_aggs(result: &Value) -> Vec<&Value> {
    result.get("aggregations").and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn get_buckets(agg: &Value) -> Vec<&Value> {
    agg.get("buckets").and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}


// ── Test Infrastructure ─────────────────────────────────────────────────────

type CheckFn = Box<dyn Fn(&Value) -> Result<String> + Send + Sync>;

struct TestSpec {
    name: String,
    method: String,
    params: Option<Value>,
    check: CheckFn,
    /// Equivalent CLI args for debugging failed tests.
    /// e.g. `["*.txt", "--files-only", "--limit", "10"]`
    cli_args: Vec<String>,
    /// When `true`, the test expects the RPC response to contain an
    /// `error` object (not `result`).  The `check` fn receives the
    /// FULL response (not just the inner `result`) so it can inspect
    /// the error code / message.
    expect_error: bool,
}

#[derive(Debug)]
struct TestResult {
    name: String,
    passed: bool,
    message: String,
    elapsed_ms: u128,
    /// The RPC method + params for replay/debugging.
    rpc_call: String,
    /// Equivalent CLI command for copy-paste debugging.
    cli_command: String,
}

/// Choose a parallelism level that scales with the host CPU count.
///
/// Mirrors the CLI script's `max_parallelism()` so both validation suites
/// stretch the daemon the same way on any given machine (Linux `sched_getaffinity`,
/// Windows `GetActiveProcessorCount`, macOS `host_processor_info`).  The fallback
/// of 8 matches the old hard-coded chunk size for small/unknown machines.
fn max_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
}

fn run_tests(sock: &str, specs: Vec<TestSpec>, args: &ScriptArgs) -> Vec<TestResult> {
    use std::sync::Mutex;
    let results = Arc::new(Mutex::new(Vec::new()));
    // Build the CLI prefix once: binary + source flags.
    let mut cli_prefix = vec![args.bin.clone()];
    if let Some(flag) = args.source_flag {
        cli_prefix.push(flag.to_string());
        cli_prefix.push(args.source_path.clone());
    }
    let cli_prefix = Arc::new(cli_prefix);
    // Run tests with a thread pool sized to the host's logical CPU count.
    let specs = Arc::new(specs);
    let chunk_size = max_parallelism();
    let total = specs.len();
    let mut idx = 0;
    while idx < total {
        let end = (idx + chunk_size).min(total);
        let mut handles = Vec::new();
        for i in idx..end {
            let sock = sock.to_string();
            let specs = Arc::clone(&specs);
            let results = Arc::clone(&results);
            let cli_prefix = Arc::clone(&cli_prefix);
            handles.push(std::thread::spawn(move || {
                let spec = &specs[i];
                if log_enabled() {
                    log_write(&format!("\n{}", "=".repeat(70)));
                    log_write(&format!("[TEST] {} | method={} | cli_args={:?}", spec.name, spec.method, spec.cli_args));
                    if let Some(p) = &spec.params {
                        log_write(&format!("[TEST] RPC params:\n{}", serde_json::to_string_pretty(p).unwrap_or_default()));
                    }
                }
                let t0 = Instant::now();
                // Negative-path tests (expect_error) use the allow-error
                // transport so the error response reaches the validator.
                let transport_result = if spec.expect_error {
                    rpc_call_allow_error(&sock, &spec.method, spec.params.clone())
                } else {
                    rpc_call(&sock, &spec.method, spec.params.clone())
                };
                let outcome = transport_result.and_then(|resp| {
                    if spec.expect_error {
                        // Negative-path test: the daemon must return an
                        // `error` object, not `result`.  Pass the FULL
                        // response so the validator can inspect the code.
                        if resp.get("result").is_some() {
                            return Err(anyhow::anyhow!(
                                "Expected RPC error response but got success: {}",
                                serde_json::to_string(&resp).unwrap_or_default()
                            ));
                        }
                        if resp.get("error").is_none() {
                            return Err(anyhow::anyhow!(
                                "Expected RPC error object but response has neither `result` nor `error`: {}",
                                serde_json::to_string(&resp).unwrap_or_default()
                            ));
                        }
                        (spec.check)(&resp)
                    } else {
                        let result = resp.get("result").cloned()
                            .ok_or_else(|| anyhow::anyhow!(
                                "RPC response missing 'result' field: {}",
                                serde_json::to_string(&resp).unwrap_or_default()
                            ))?;
                        (spec.check)(&result)
                    }
                });
                let elapsed_ms = t0.elapsed().as_millis();
                let (passed, message) = match &outcome {
                    Ok(msg) => (true, msg.clone()),
                    Err(e) => (false, format!("{e:#}")),
                };
                if log_enabled() {
                    let status = if passed { "PASS" } else { "FAIL" };
                    log_write(&format!("[TEST] {status} {elapsed_ms}ms — {}: {message}", spec.name));
                }
                let rpc_call = match &spec.params {
                    Some(p) => format!("{}({})", spec.method, p),
                    None => spec.method.clone(),
                };
                // Build the full CLI command for debugging.
                // For RPC-only methods (status, drives, etc.) map to
                // their `uffs --daemon <method>` CLI equivalent.
                let cli_command = {
                    let mut cli_parts = cli_prefix.as_ref().clone();
                    let bin = cli_parts.remove(0);
                    let mut full = vec![bin];
                    // Daemon subcommands don't take --data-dir/--mft-file;
                    // they connect to the already-running daemon.
                    // Subcommands that connect to the running daemon and
                    // don't accept --data-dir / --mft-file.
                    let is_daemon_cmd = spec.cli_args.first().map(|a| a.as_str()) == Some("daemon")
                        || spec.cli_args.first().map(|a| a.as_str()) == Some("info")
                        || (spec.cli_args.is_empty()
                            && matches!(spec.method.as_str(),
                                "status" | "drives" | "stats" | "keepalive" | "info"));
                    if spec.cli_args.is_empty() {
                        // Pure RPC test — map method to CLI subcommand.
                        match spec.method.as_str() {
                            "status" => full.extend(["daemon".into(), "status".into()]),
                            "drives" => full.extend(["daemon".into(), "status".into()]),
                            "stats"  => full.extend(["daemon".into(), "status".into(), "-v".into()]),
                            "keepalive" => full.extend(["daemon".into(), "status".into()]),
                            "search" => full.push("\"*\"".into()),
                            other => full.push(format!("# RPC method: {other} (no CLI equivalent)")),
                        }
                    } else {
                        full.extend(spec.cli_args.clone());
                    }
                    if !is_daemon_cmd {
                        full.extend(cli_parts);
                    }
                    full.iter()
                        .map(|a| {
                            if a.contains(' ') || a.contains('*') || a.contains('>') || a.contains('<') {
                                format!("\"{a}\"")
                            } else {
                                a.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                let tr = TestResult {
                    name: spec.name.clone(),
                    passed,
                    message,
                    elapsed_ms,
                    rpc_call,
                    cli_command,
                };
                // Print immediately as each test completes.
                let status = if tr.passed { "PASS".green().bold() } else { "FAIL".red().bold() };
                let timing = format!("{:>5}ms", tr.elapsed_ms).dimmed();
                if tr.passed {
                    eprintln!("  [{status}] {timing}  {}: {} [api]", tr.name, tr.message);
                } else {
                    eprintln!("  [{status}] {timing}  {}: {}", tr.name, tr.message.red());
                }
                results.lock().unwrap().push(tr);
            }));
        }
        for h in handles { h.join().unwrap(); }
        idx = end;
    }
    let mut out = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    // Sort by test name for stable output (used by summary).
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}


// ── TOML Test Definitions ──────────────────────────────────────────────────
//
// Shared definitions loaded from `scripts/tests/test-definitions.toml`.
// Same schema as cli-validation — one TOML, two consumers.

#[derive(serde::Deserialize)]
struct TestDefsFile { test: Vec<TestDef> }

#[derive(Clone, serde::Deserialize)]
struct TestDef {
    #[allow(dead_code)] id: String,
    #[allow(dead_code)] group: String,
    name: String,
    #[allow(dead_code)] title: String,
    #[allow(dead_code)] short_desc: String,
    #[allow(dead_code)] long_desc: Option<String>,
    #[serde(default)]
    cli_args: Vec<String>,
    #[allow(dead_code)] cli_format: Option<String>,
    rpc_method: Option<String>,
    rpc_params: Option<String>,

    // ── Shared assertions (validated by ALL targets) ──────────────
    expect_min_rows: Option<usize>,
    expect_max_rows: Option<usize>,
    #[allow(dead_code)] expect_columns_all: Option<bool>,
    #[serde(default)] column_checks: Vec<ColumnCheck>,
    #[serde(default)] sort_checks: Vec<SortCheck>,
    validator: Option<String>,
    #[serde(default = "default_targets")] targets: Vec<String>,
    skip: Option<bool>,
    #[serde(default)] #[allow(dead_code)] tags: Vec<String>,

    // ── Expected-error tests ───────────────────────────────────────
    /// When `true`, the test expects the daemon RPC to return an error
    /// response (no `result` field) rather than a successful result.
    /// The test's `validator` (if any) is called on the FULL RPC
    /// response including the `error` object, so it can inspect the
    /// error code / message.  Used for negative-path tests like
    /// "unknown method returns -32601" (see `RPC.5`).
    #[serde(default)] expect_error: Option<bool>,

    // ── Per-target checks ────────────────────────────────────────
    /// CLI-specific (ignored by this script).
    #[serde(default)] #[allow(dead_code)] cli_checks: CliChecks,
    /// API-specific JSON-RPC response checks.
    #[serde(default)] api_checks: ApiChecks,
    /// MCP-specific checks (future).
    #[serde(default)] #[allow(dead_code)] mcp_checks: McpChecks,

    // ── Legacy flat fields (backward compat, ignored by API) ─────
    #[serde(default)] #[allow(dead_code)] stdout_contains: Vec<String>,
    #[serde(default)] #[allow(dead_code)] stdout_not_contains: Vec<String>,
    #[serde(default)] #[allow(dead_code)] stderr_contains: Vec<String>,
    #[serde(default)] #[allow(dead_code)] expect_exit_code: Option<i32>,
    #[serde(default)] #[allow(dead_code)] json_checks: Vec<JsonCheck>,
}

fn default_targets() -> Vec<String> { vec!["cli".into(), "api".into()] }

/// CLI-specific output checks (ignored by API validator).
#[derive(Clone, Default, serde::Deserialize)]
#[allow(dead_code)]
struct CliChecks {
    #[serde(default)] stdout_contains: Vec<String>,
    #[serde(default)] stdout_not_contains: Vec<String>,
    #[serde(default)] stderr_contains: Vec<String>,
    #[serde(default)] expect_exit_code: Option<i32>,
    #[serde(default)] expect_min_rows: Option<usize>,
    #[serde(default)] expect_max_rows: Option<usize>,
}

/// API-specific JSON-RPC response checks.
#[derive(Clone, Default, serde::Deserialize)]
struct ApiChecks {
    /// Minimum number of aggregation result blocks in response.
    #[serde(default)] expect_agg_results: Option<usize>,
    /// Keys that must exist in the top-level JSON-RPC result.
    #[serde(default)] result_has_key: Vec<String>,
    /// Aggregation result labels that must appear.
    #[serde(default)] agg_label_contains: Vec<String>,
    /// Aggregation `kind` values that must appear in at least one result.
    #[serde(default)] agg_kind_contains: Vec<String>,
    /// Minimum total buckets across all agg results.
    #[serde(default)] bucket_min_count: Option<usize>,
    /// Bucket key substrings that must appear in at least one bucket.
    #[serde(default)] bucket_key_contains: Vec<String>,
    /// At least one agg result must contain a `stats` object with these keys.
    #[serde(default)] agg_stats_has_keys: Vec<String>,
    /// At least one bucket must have `verified = true`.
    #[serde(default)] bucket_has_verified_true: Option<bool>,
    /// At least one bucket must have non-empty `sample_rows`.
    #[serde(default)] bucket_has_samples: Option<bool>,
    /// At least one bucket must have non-empty `sub_buckets`.
    #[serde(default)] bucket_has_sub_buckets: Option<bool>,
    /// At least one agg result must contain a `next_cursor` field.
    #[serde(default)] agg_has_cursor: Option<bool>,
    /// The `total_count` field must be >= this value.
    #[serde(default)] total_count_min: Option<u64>,
    /// Check that specific agg results have `exact` field with this value.
    #[serde(default)] agg_exact: Option<bool>,
    /// The `shmem_count` field must be >= this value (verifies shmem transfer).
    #[serde(default)] shmem_count_min: Option<u64>,
}

/// MCP-specific checks (future expansion).
#[derive(Clone, Default, serde::Deserialize)]
#[allow(dead_code)]
struct McpChecks {
    #[serde(default)] tool_name: Option<String>,
    #[serde(default)] response_contains: Vec<String>,
}

#[derive(Clone, serde::Deserialize)]
struct ColumnCheck { column: String, op: String, value: String, case: Option<String> }

#[derive(Clone, serde::Deserialize)]
struct SortCheck { column: String, order: String, #[serde(rename = "type", default = "default_u64")] #[allow(dead_code)] sort_type: String }

fn default_u64() -> String { "u64".to_string() }

#[derive(Clone, serde::Deserialize)]
#[allow(dead_code)]
struct JsonCheck { path: String, op: String, value: Option<String> }

/// Map CSV column name → JSON field name in the RPC response.
fn csv_col_to_json(col: &str) -> &str {
    match col {
        "Name"           => "name",
        "Size"           => "size",
        "Directory Flag" => "is_directory",
        "Path Only"      => "_path_only",
        "Descendants"    => "descendants",
        "Tree Size"      => "treesize",
        "Tree Allocated" => "tree_allocated",
        "Created"        => "created",
        "Modified"       => "written",
        "Accessed"       => "accessed",
        "Hidden"         => "hidden",
        "System"         => "system",
        "ReadOnly"       => "readonly",
        "Flags"          => "flags",
        "Size on Disk"   => "allocated",
        "Extension"      => "_ext",
        // Computed columns — handled by rpc_field_computed.
        "Bulkiness"      => "_bulkiness",
        "Name Length"    => "_name_length",
        "Path Length"    => "_path_length",
        other            => other,
    }
}

/// Dot-gated extension extraction matching the sort engine's key
/// (`extract_extension_after_dot` in
/// `crates/uffs-core/src/search/filters/ext_match.rs`) and the CLI/MCP
/// display helpers (`extension_from_name` in `uffs-format`/`uffs-core`).
///
/// Returns an empty string for:
///   * dotless names           (`README`           -> `""`)
///   * leading-dot "hidden"    (`.bash_history`   -> `""`)
///   * trailing-dot names      (`foo.`             -> `""`)
///
/// Without this guard, `".bash_history".rsplit('.').next()` returns
/// `"bash_history"`, which (a) breaks T62 `--sort extension asc` because
/// `"bash_history" > "2008"` lexically, and (b) misclassifies dotless
/// rows in the `type_*` allowlist validator (their full name becomes the
/// purported extension).
fn extract_ext_dot_gated(name: &str) -> String {
    let Some(dot) = name.rfind('.') else { return String::new(); };
    if dot == 0 || dot + 1 >= name.len() {
        return String::new();
    }
    name.get(dot + 1..).unwrap_or("").to_ascii_lowercase()
}

/// Compute derived column values that don't exist directly in the
/// JSON-RPC response but can be calculated from existing fields.
fn rpc_field_computed(row: &Value, json_key: &str) -> Option<String> {
    match json_key {
        "_bulkiness" => {
            let is_dir = row.get("is_directory").and_then(|v| v.as_bool()).unwrap_or(false);
            let (logical, alloc) = if is_dir {
                (field_u64(row, "treesize"), field_u64(row, "tree_allocated"))
            } else {
                (field_u64(row, "size"), field_u64(row, "allocated"))
            };
            if logical == 0 { Some("0".to_owned()) }
            else { Some(((alloc * 100) / logical).to_string()) }
        }
        "_name_length" => {
            let name = field_str(row, "name");
            Some(name.len().to_string())
        }
        "_path_length" => {
            // Full path already includes name.
            let path = field_str(row, "path");
            Some(path.len().to_string())
        }
        "_ext" => {
            let name = field_str(row, "name");
            Some(extract_ext_dot_gated(&name))
        }
        "_path_only" => {
            // Directory path only (strip filename from full path).
            let path = field_str(row, "path");
            let name = field_str(row, "name");
            let dir = path.strip_suffix(&name)
                .unwrap_or(&path)
                .trim_end_matches('\\')
                .to_owned();
            Some(dir)
        }
        _ => None,
    }
}

/// Convert CLI args to JSON-RPC params for the "search" method.
/// Convert a power-syntax agg string (e.g. "terms:extension,top=5,sample=2")
/// into an `AggregateSpecWire` JSON value.
fn parse_agg_power_syntax(input: &str) -> Value {
    let (kind_str, rest) = if let Some(pos) = input.find(':') {
        (&input[..pos], &input[pos + 1..])
    } else {
        (input, "")
    };
    let mut spec = serde_json::Map::new();
    // Map shorthand aliases.
    let kind = match kind_str {
        "facet" => "terms",
        "hist" => "histogram",
        "datehist" => "date_histogram",
        k => k,
    };
    spec.insert("kind".into(), json!(kind));
    if kind == "preset" {
        spec.insert("preset".into(), json!(rest));
        return Value::Object(spec);
    }
    if !rest.is_empty() {
        // Parse "field,key=val,key=val,..."
        let mut parts = rest.splitn(2, ',');
        if let Some(field_part) = parts.next() {
            // Field may contain '+' for duplicates (e.g. "size+name").
            if !field_part.contains('=') {
                spec.insert("field".into(), json!(field_part));
            }
        }
        if let Some(opts) = parts.next() {
            for kv in opts.split(',') {
                if let Some(eq) = kv.find('=') {
                    let k = &kv[..eq];
                    let v = &kv[eq + 1..];
                    match k {
                        "top" => { if let Ok(n) = v.parse::<u16>() { spec.insert("top".into(), json!(n)); } }
                        "sample" => { if let Ok(n) = v.parse::<u8>() { spec.insert("sample".into(), json!(n)); } }
                        "sort" => { spec.insert("sample_sort".into(), json!(v)); }
                        "interval" => { if let Ok(n) = v.parse::<u64>() { spec.insert("interval".into(), json!(n)); } }
                        "calendar" => { spec.insert("calendar".into(), json!(v)); }
                        "depth" => { if let Ok(n) = v.parse::<u8>() { spec.insert("depth".into(), json!(n)); } }
                        "bins" => {
                            let bins: Vec<u64> = v.split('+').filter_map(|b| b.parse().ok()).collect();
                            spec.insert("boundaries".into(), json!(bins));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Value::Object(spec)
}

fn cli_args_to_rpc_params(args: &[String]) -> Value {
    let mut params = serde_json::Map::new();
    let mut agg_specs: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--files-only"   => { params.insert("filter".into(), json!("files")); }
            "--dirs-only"    => { params.insert("filter".into(), json!("dirs")); }
            "--hide-system"  => { params.insert("hide_system".into(), json!(true)); }
            "--sort-desc"    => { params.insert("sort_desc".into(), json!(true)); }
            "--name-only"    => { /* table format flag, not relevant for RPC */ }
            "--columns"      => { i += 1; /* skip value — CLI-only display option */ }
            "--format"       => { i += 1; /* CLI-only */ }
            "--out"          => { i += 1; /* CLI-only */ }
            "--limit" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("limit".into(), json!(n)); }
                }
            }
            "--ext" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("ext".into(), json!(v)); }
            }
            "--min-size" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("min_size".into(), json!(n)); }
                }
            }
            "--max-size" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("max_size".into(), json!(n)); }
                }
            }
            "--sort" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let s = v.as_str();
                    if s.starts_with('-') {
                        params.insert("sort".into(), json!(&s[1..]));
                        params.insert("sort_desc".into(), json!(true));
                    } else {
                        params.insert("sort".into(), json!(s));
                    }
                }
            }
            "--exclude" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("exclude".into(), json!(v)); }
            }
            "--newer" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("newer".into(), json!(v)); }
            }
            "--older" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("older".into(), json!(v)); }
            }
            "--newer-created" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("newer_created".into(), json!(v)); }
            }
            "--older-created" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("older_created".into(), json!(v)); }
            }
            "--newer-accessed" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("newer_accessed".into(), json!(v)); }
            }
            "--older-accessed" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("older_accessed".into(), json!(v)); }
            }
            "--type" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("type_filter".into(), json!(v)); }
            }
            "--min-bulkiness" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_bulkiness".into(), json!(n)); } }
            }
            "--max-bulkiness" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_bulkiness".into(), json!(n)); } }
            }
            "--min-treesize" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_treesize".into(), json!(n)); } }
            }
            "--max-treesize" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_treesize".into(), json!(n)); } }
            }
            "--min-tree-allocated" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_tree_allocated".into(), json!(n)); } }
            }
            "--max-tree-allocated" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_tree_allocated".into(), json!(n)); } }
            }
            "--min-name-length" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u16>() { params.insert("min_name_len".into(), json!(n)); } }
            }
            "--max-name-length" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u16>() { params.insert("max_name_len".into(), json!(n)); } }
            }
            "--min-path-length" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u16>() { params.insert("min_path_len".into(), json!(n)); } }
            }
            "--max-path-length" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u16>() { params.insert("max_path_len".into(), json!(n)); } }
            }
            "--min-size-on-disk" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("min_allocated".into(), json!(n)); } }
            }
            "--max-size-on-disk" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() { params.insert("max_allocated".into(), json!(n)); } }
            }
            "--exact-size" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() {
                    params.insert("min_size".into(), json!(n));
                    params.insert("max_size".into(), json!(n));
                } }
            }
            "--exact-size-on-disk" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u64>() {
                    params.insert("min_allocated".into(), json!(n));
                    params.insert("max_allocated".into(), json!(n));
                } }
            }
            "--exact-descendants" => {
                i += 1;
                if let Some(v) = args.get(i) { if let Ok(n) = v.parse::<u32>() {
                    params.insert("min_descendants".into(), json!(n));
                    params.insert("max_descendants".into(), json!(n));
                } }
            }
            "--between" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let mut parts = v.splitn(2, ',');
                    if let Some(start) = parts.next() {
                        params.insert("newer".into(), json!(start.trim()));
                    }
                    if let Some(end) = parts.next() {
                        params.insert("older".into(), json!(end.trim()));
                    }
                }
            }
            "--month" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let months = parse_month_spec(v);
                    if !months.is_empty() { params.insert("allowed_months".into(), json!(months)); }
                }
            }
            "--case" | "--case-sensitive" => { params.insert("case_sensitive".into(), json!(true)); }
            "--word" => { params.insert("whole_word".into(), json!(true)); }
            "--benchmark" => { /* CLI-only */ }
            "--smart-case" => { /* handled by pattern casing heuristic */ }
            "--header" => { i += 1; /* CLI-only */ }
            "--sep" => { i += 1; /* CLI-only */ }
            "--quotes" => { i += 1; /* CLI-only */ }
            "--begins-with" => {
                // Pattern sugar: --begins-with PREFIX → pattern = "PREFIX*"
                i += 1;
                if let Some(v) = args.get(i) {
                    params.insert("pattern".into(), json!(format!("{v}*")));
                }
            }
            "--ends-with" => {
                // Pattern sugar: --ends-with SUFFIX → pattern = "*SUFFIX"
                i += 1;
                if let Some(v) = args.get(i) {
                    params.insert("pattern".into(), json!(format!("*{v}")));
                }
            }
            "--contains" => {
                // Pattern sugar: --contains NEEDLE → pattern = "*NEEDLE*"
                i += 1;
                if let Some(v) = args.get(i) {
                    params.insert("pattern".into(), json!(format!("*{v}*")));
                }
            }
            "--not-contains" => {
                // Maps to exclude filter.
                i += 1;
                if let Some(v) = args.get(i) {
                    params.insert("exclude".into(), json!(format!("*{v}*")));
                }
            }
            "--in-path" => {
                // Match against full path.
                i += 1;
                if let Some(v) = args.get(i) {
                    params.insert("path_contains".into(), json!(v));
                }
            }
            "--attr" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("attr".into(), json!(v)); }
            }
            "--min-depth" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("min_depth".into(), json!(n)); }
                }
            }
            "--max-depth" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("max_depth".into(), json!(n)); }
                }
            }
            "--min-descendants" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("min_descendants".into(), json!(n)); }
                }
            }
            "--max-descendants" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("max_descendants".into(), json!(n)); }
                }
            }
            "--agg" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    agg_specs.push(parse_agg_power_syntax(v));
                }
            }
            "--facet" => {
                // --facet ext:10 → terms:extension,top=10
                i += 1;
                if let Some(v) = args.get(i) {
                    let (field, top) = if let Some(pos) = v.find(':') {
                        (&v[..pos], v[pos+1..].parse::<u16>().unwrap_or(50))
                    } else {
                        (v.as_str(), 50)
                    };
                    agg_specs.push(json!({"kind": "terms", "field": field, "top": top}));
                }
            }
            "--count" => {
                agg_specs.push(json!({"kind": "count"}));
            }
            "--stats" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    agg_specs.push(json!({"kind": "stats", "field": v.as_str()}));
                }
            }
            "--histogram" => {
                // --histogram size:1048576
                i += 1;
                if let Some(v) = args.get(i) {
                    let (field, interval) = if let Some(pos) = v.find(':') {
                        (&v[..pos], v[pos+1..].parse::<u64>().unwrap_or(1_048_576))
                    } else {
                        (v.as_str(), 1_048_576u64)
                    };
                    agg_specs.push(json!({"kind": "histogram", "field": field, "interval": interval}));
                }
            }
            "--rows" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("limit".into(), json!(n)); }
                    params.insert("include_rows".into(), json!(true));
                }
            }
            "--agg-page-size" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() { params.insert("agg_page_size".into(), json!(n)); }
                }
            }
            "--agg-cursor" => {
                i += 1;
                if let Some(v) = args.get(i) { params.insert("agg_cursor".into(), json!(v)); }
            }
            "--drive" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    params.insert("drives".into(), json!([v.as_str()]));
                }
            }
            "--drives" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let drives: Vec<&str> = v.split(',').collect();
                    params.insert("drives".into(), json!(drives));
                }
            }
            s if !s.starts_with('-') && !params.contains_key("pattern") => {
                // ── "agg" subcommand ────────────────────────────────
                // `cli_args = ["agg", "count", "--format", "json"]`
                // The CLI's `aggregate` subcommand uses `pattern: "*"`
                // with `include_rows: false` and a single preset-based
                // aggregation spec.
                if s == "agg" || s == "aggregate" {
                    params.insert("pattern".into(), json!("*"));
                    params.insert("include_rows".into(), json!(false));
                    params.insert("limit".into(), json!(0));
                    // Next positional arg is the preset name.
                    i += 1;
                    if let Some(preset) = args.get(i) {
                        if preset == "count" {
                            agg_specs.push(json!({"kind": "count"}));
                        } else {
                            agg_specs.push(json!({"kind": "preset", "preset": preset}));
                        }
                    }
                }
                // ── Regular pattern ─────────────────────────────────
                // Handle path: / dir: / file: prefixes.
                else if let Some(rest) = s.strip_prefix("path:") {
                    params.insert("pattern".into(), json!(rest));
                    params.insert("match_path".into(), json!(true));
                } else if let Some(rest) = s.strip_prefix("dir:") {
                    params.insert("pattern".into(), json!(rest));
                    params.insert("filter".into(), json!("dirs"));
                } else if let Some(rest) = s.strip_prefix("file:") {
                    params.insert("pattern".into(), json!(rest));
                    params.insert("filter".into(), json!("files"));
                } else {
                    params.insert("pattern".into(), json!(s));
                }
            }
            _ => {} // ignore unknown flags
        }
        i += 1;
    }
    // Default pattern if not set.
    if !params.contains_key("pattern") {
        params.insert("pattern".into(), json!("*"));
    }
    // Merge aggregation specs into the params.
    if !agg_specs.is_empty() {
        params.insert("aggregations".into(), Value::Array(agg_specs));
        // For agg-only queries, cap row limit to avoid serializing
        // millions of rows.  The aggregation engine scans all matching
        // records regardless of `limit`, so results are still correct.
        if !params.contains_key("limit") {
            params.insert("limit".into(), json!(1));
        }
    }
    Value::Object(params)
}

/// Parse a month spec like "1,2,3" or "jan,feb,mar" or "1-3" into month numbers.
fn parse_month_spec(spec: &str) -> Vec<u32> {
    let mut months = Vec::new();
    for part in spec.split(',') {
        let p = part.trim().to_lowercase();
        if let Some(dash) = p.find('-') {
            if let (Ok(start), Ok(end)) = (p[..dash].parse::<u32>(), p[dash+1..].parse::<u32>()) {
                for m in start..=end { if (1..=12).contains(&m) { months.push(m); } }
            }
        } else if let Ok(n) = p.parse::<u32>() {
            if (1..=12).contains(&n) { months.push(n); }
        } else {
            let n = match p.as_str() {
                "jan" => 1, "feb" => 2, "mar" => 3, "apr" => 4,
                "may" => 5, "jun" => 6, "jul" => 7, "aug" => 8,
                "sep" => 9, "oct" => 10, "nov" => 11, "dec" => 12,
                _ => 0,
            };
            if n > 0 { months.push(n); }
        }
    }
    months
}

/// Get a JSON field value as string (handles booleans and numbers).
fn rpc_field_str(row: &Value, json_key: &str) -> String {
    match row.get(json_key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b))   => if *b { "1".to_string() } else { "0".to_string() },
        Some(Value::Null)      => String::new(),
        _                      => String::new(),
    }
}

/// Map CSV attribute column name to its NTFS flag bitmask.
/// (Flag constants and `has_flag` are defined earlier in the file.)
fn attr_col_to_flag(col: &str) -> Option<u32> {
    match col {
        "Hidden"     => Some(FLAG_HIDDEN),
        "System"     => Some(FLAG_SYSTEM),
        "Archive"    => Some(FLAG_ARCHIVE),
        "ReadOnly" | "Read-only" => Some(FLAG_READONLY),
        "Compressed" => Some(FLAG_COMPRESSED),
        "Sparse"     => Some(FLAG_SPARSE),
        "Reparse"    => Some(FLAG_REPARSE),
        "Offline"    => Some(FLAG_OFFLINE),
        _ => None,
    }
}

/// Apply a column check to an RPC row (JSON object).
fn apply_rpc_column_check(row_idx: usize, row: &Value, check: &ColumnCheck) -> Result<()> {
    // Special handling for "Directory Flag" — maps to JSON bool `is_directory`.
    if check.column == "Directory Flag" {
        let is_dir = row.get("is_directory").and_then(|v| v.as_bool()).unwrap_or(false);
        let expected = &check.value == "1";
        match check.op.as_str() {
            "eq" => { if is_dir != expected { bail!("Row {row_idx}: is_directory={is_dir}, expected={expected}"); } }
            "ne" => { if is_dir == expected { bail!("Row {row_idx}: is_directory={is_dir}, expected !={expected}"); } }
            _ => {}
        }
        return Ok(());
    }

    // Special handling for NTFS attribute columns → check flags bitmask.
    if let Some(flag) = attr_col_to_flag(&check.column) {
        let has = has_flag(row, flag);
        let expected = &check.value == "1";
        match check.op.as_str() {
            "eq" => {
                if has != expected {
                    bail!("Row {row_idx}: {} flag expected={expected}, got={has}", check.column);
                }
            }
            "ne" => {
                if has == expected {
                    bail!("Row {row_idx}: {} flag expected !={expected}, got={has}", check.column);
                }
            }
            _ => {} // Other ops don't apply to flags.
        }
        return Ok(());
    }

    let json_key = csv_col_to_json(&check.column);
    let raw = rpc_field_computed(row, json_key)
        .unwrap_or_else(|| rpc_field_str(row, json_key));
    let v = match check.case.as_deref() {
        Some("lower") => raw.to_lowercase(),
        _ => raw.clone(),
    };
    let expected = match check.case.as_deref() {
        Some("lower") => check.value.to_lowercase(),
        _ => check.value.clone(),
    };
    match check.op.as_str() {
        "eq"  => { if v != expected { bail!("Row {row_idx}: {json_key}={raw}, expected {}", check.value); } }
        "ne"  => { if v == expected { bail!("Row {row_idx}: {json_key}={raw}, expected != {}", check.value); } }
        "contains"       => { if !v.contains(&expected) { bail!("Row {row_idx}: {json_key}={raw}, expected contains {}", check.value); } }
        "not_contains"   => { if v.contains(&expected) { bail!("Row {row_idx}: {json_key}={raw}, expected !contains {}", check.value); } }
        "starts_with"    => { if !v.starts_with(&expected) { bail!("Row {row_idx}: {json_key}={raw}, expected starts_with {}", check.value); } }
        "not_starts_with"=> { if v.starts_with(&expected) { bail!("Row {row_idx}: {json_key}={raw}, expected !starts_with {}", check.value); } }
        "ends_with"      => { if !v.ends_with(&expected) { bail!("Row {row_idx}: {json_key}={raw}, expected ends_with {}", check.value); } }
        "gt"  => { let n: u64 = v.parse().unwrap_or(0); let e: u64 = expected.parse().unwrap_or(0); if n <= e { bail!("Row {row_idx}: {json_key}={n}, expected > {e}"); } }
        "gte" => { let n: u64 = v.parse().unwrap_or(0); let e: u64 = expected.parse().unwrap_or(0); if n <  e { bail!("Row {row_idx}: {json_key}={n}, expected >= {e}"); } }
        "lt"  => { let n: u64 = v.parse().unwrap_or(u64::MAX); let e: u64 = expected.parse().unwrap_or(0); if n >= e { bail!("Row {row_idx}: {json_key}={n}, expected < {e}"); } }
        "lte" => { let n: u64 = v.parse().unwrap_or(u64::MAX); let e: u64 = expected.parse().unwrap_or(0); if n >  e { bail!("Row {row_idx}: {json_key}={n}, expected <= {e}"); } }
        other => bail!("Unknown column_check op: {other}"),
    }
    Ok(())
}

/// Run a named custom RPC validator.
fn run_rpc_custom_validator(name: &str, result: &Value) -> Result<String> {
    match name {
        "ext_multi_check" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows"); }
            let exts = ["jpg", "png", "gif"];
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if !exts.iter().any(|e| n.ends_with(&format!(".{e}"))) {
                    bail!("Row {i}: {n} not in {{jpg,png,gif}}");
                }
            }
            Ok(format!("{} rows, all image extensions", rows.len()))
        }
        "T31" => Ok("(--out not applicable for RPC)".into()),
        "T87" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate ext filter"); }
            let exts = ["txt", "log", "md"];
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if !exts.iter().any(|e| n.ends_with(&format!(".{e}"))) { bail!("Row {i}: {n} not in {{txt,log,md}}"); }
            }
            Ok(format!("{} rows, ext filtered", rows.len()))
        }
        "T88" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate name filter"); }
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if !n.contains("config") { bail!("Row {i}: {n} doesn't contain 'config'"); }
            }
            Ok(format!("{} rows", rows.len()))
        }
        "T88a" | "T88i" => {
            let rows = get_rows(result);
            let needle = if name == "T88a" { "windows" } else { "notepad" };
            if rows.is_empty() { bail!("No rows — cannot validate path contains '{needle}'"); }
            for (i, row) in rows.iter().enumerate() {
                let path = field_str(row, "path").to_lowercase();
                let n = field_str(row, "name").to_lowercase();
                let full = format!("{path}{n}");
                if !full.contains(needle) { bail!("Row {i}: '{full}' doesn't contain '{needle}'"); }
            }
            Ok(format!("{} rows, all contain '{needle}'", rows.len()))
        }
        "T88j" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate contains+not_contains"); }
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if !n.contains("update") { bail!("Row {i}: {n} doesn't contain 'update'"); }
                if n.contains("old") { bail!("Row {i}: {n} contains 'old'"); }
            }
            Ok(format!("{} rows", rows.len()))
        }
        "T88k" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate dir filter"); }
            for (i, row) in rows.iter().enumerate() {
                if !field_bool(row, "is_directory") { bail!("Row {i}: not a directory"); }
            }
            Ok(format!("{} dirs sorted", rows.len()))
        }
        "T88l" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate begins_with+ext"); }
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if !n.starts_with("win") { bail!("Row {i}: {n} doesn't start with 'win'"); }
                if !n.ends_with(".exe") && !n.ends_with(".dll") { bail!("Row {i}: {n} not .exe/.dll"); }
            }
            Ok(format!("{} rows", rows.len()))
        }
        "T67d" => {
            let rows = get_rows(result);
            if rows.len() < 2 { bail!("Need ≥ 2 rows to validate name-length sort, got {}", rows.len()); }
            let lens: Vec<usize> = rows.iter().map(|r| field_str(r, "name").len()).collect();
            for w in lens.windows(2) { if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); } }
            Ok(format!("{} rows, sorted by name length desc", rows.len()))
        }
        "T67f2" => {
            // PathOnly sort must honour Windows Explorer's `Folder` column
            // convention: when two rows share the same parent directory
            // (case-insensitive), the secondary tiebreaker is filename
            // ASC.  Mirrored by
            // `search_index_path_only_sort_name_asc_within_same_folder`
            // in crates/uffs-core/src/search/backend_tests.rs.
            //
            // This validator *only* checks the tiebreaker invariant — it
            // intentionally does NOT re-validate the primary `path_only`
            // ASC ordering.  Primary ordering is T67f's job (via the
            // generic sort_check framework, which folds with
            // `.to_lowercase()`); the daemon's sort uses NTFS $UpCase
            // (upper-fold), and the two conventions only disagree on
            // characters between `Z` (0x5A) and `a` (0x61) — notably `_`
            // (0x5F).  Validating primary here would spuriously fail on
            // path pairs like `pmf_ryzenaimax` vs `pmf_ryzen_ai` where
            // upper-fold places `A`<`_` and lower-fold places `_`<`a`.
            //
            // The sibling rows we care about (same path_only) have
            // identical case-folded prefixes so this ambiguity never
            // surfaces for tiebreaker comparison.
            let rows = get_rows(result);
            if rows.len() < 2 {
                bail!("Need ≥ 2 rows to validate path_only+name sort, got {}", rows.len());
            }
            let pairs: Vec<(String, String)> = rows.iter().map(|r| {
                let path = field_str(r, "path");
                let name = field_str(r, "name");
                let dir = path
                    .strip_suffix(&name)
                    .unwrap_or(&path)
                    .trim_end_matches('\\')
                    .to_owned();
                (dir, name.to_owned())
            }).collect();
            let mut saw_tiebreaker = false;
            for w in pairs.windows(2) {
                let (po0, n0) = &w[0];
                let (po1, n1) = &w[1];
                if po0.eq_ignore_ascii_case(po1) {
                    saw_tiebreaker = true;
                    let n0_fold = n0.to_lowercase();
                    let n1_fold = n1.to_lowercase();
                    if n0_fold > n1_fold {
                        bail!(
                            "Not asc (name tiebreaker within '{}'): '{}' > '{}'",
                            po0, n0, n1
                        );
                    }
                }
            }
            if !saw_tiebreaker {
                bail!(
                    "Test vacuous: {} rows all have distinct path_only values — \
                     no adjacent pair with equal path_only to exercise the name \
                     tiebreaker.  Expand the search or raise --limit so rows from \
                     the same folder appear together.",
                    rows.len()
                );
            }
            Ok(format!(
                "{} rows, name-ASC tiebreaker verified for same-folder siblings",
                rows.len()
            ))
        }
        "T75" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate ext+size filter"); }
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if !n.ends_with(".exe") && !n.ends_with(".dll") { bail!("Row {i}: {n} not exe/dll"); }
                let s = field_u64(row, "size");
                if s < 1_048_576 { bail!("Row {i}: size={s} < 1MB"); }
            }
            Ok(format!("{} rows", rows.len()))
        }
        "T78" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate exclude+size filter"); }
            for (i, row) in rows.iter().enumerate() {
                let n = field_str(row, "name").to_lowercase();
                if n.starts_with("debug") { bail!("Row {i}: {n} matches exclude"); }
                let s = field_u64(row, "size");
                if s > 1_048_576 { bail!("Row {i}: size={s} > 1MB"); }
            }
            Ok(format!("{} rows", rows.len()))
        }
        "T34" => {
            let rows = get_rows(result);
            if rows.len() < 2 { bail!("Need ≥ 2 rows to validate sort+size, got {}", rows.len()); }
            let sizes: Vec<u64> = rows.iter().map(|r| field_u64(r, "size")).collect();
            for w in sizes.windows(2) { if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); } }
            for (i, row) in rows.iter().enumerate() {
                let s = field_u64(row, "size");
                if s < 1_048_576 { bail!("Row {i}: size={s} < 1MB"); }
            }
            Ok(format!("{} rows, size desc ≥ 1MB", rows.len()))
        }
        "T65" | "T67a" | "T67b" | "T94" => {
            let col = match name {
                "T65"  => "descendants",
                "T67a" => "tree_size",
                "T67b" => "tree_allocated",
                "T94"  => "size",
                _      => "size",
            };
            let rows = get_rows(result);
            if rows.len() < 2 { bail!("Need ≥ 2 rows to validate {col} sort, got {}", rows.len()); }
            let vals: Vec<u64> = rows.iter().map(|r| field_u64(r, col)).collect();
            for w in vals.windows(2) { if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); } }
            Ok(format!("{} rows, sorted {} desc", rows.len(), col))
        }
        "T76" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows — cannot validate descendants range"); }
            for (i, row) in rows.iter().enumerate() {
                let d = field_u64(row, "descendants");
                if d < 10 || d > 1000 { bail!("Row {i}: desc={d} outside 10..1000"); }
            }
            Ok(format!("{} dirs, desc range 10..1000", rows.len()))
        }
        // JSON/agg validators that don't apply to standard search response.
        "T20" | "T79" | "T126" | "T127" | "T136" | "T137" | "T146" | "T147"
        | "T149" | "T150" | "T151" | "T153" | "T154" | "T155" | "T156" | "T157"
        | "T158" | "T160" | "T161" | "T162" | "T163" | "T164" | "T165" | "T166"
        | "T167" | "T173" | "T174" | "T175" | "T176"
        | "S3A.1" | "S3A.3" | "S3A.4" | "S3C.1" | "S3D.3" | "S3D.4" | "S3G.1" | "S3G.8" => {
            // Aggregation tests use different response shapes — check aggs exist.
            let aggs = get_aggs(result);
            if aggs.is_empty() {
                bail!("No aggregation results — cannot validate (daemon has data?)");
            }
            Ok(format!("{} aggregation results", aggs.len()))
        }
        // ── S3B: Facet values (via terms aggregation) ────────────────
        "S3B.1" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("No aggregation results"); }
            let buckets = get_buckets(aggs[0]);
            if buckets.is_empty() { bail!("No facet value buckets"); }
            if buckets.len() > 3 { bail!("Expected ≤ 3 buckets, got {}", buckets.len()); }
            Ok(format!("{} facet values (top 3)", buckets.len()))
        }
        "S3B.2" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("No aggregation results"); }
            let buckets = get_buckets(aggs[0]);
            if buckets.is_empty() { bail!("No facet value buckets"); }
            Ok(format!("{} facet values (all extensions)", buckets.len()))
        }

        // ── S3misc: Aggregation variants ─────────────────────────────
        "S3misc.1" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("No aggs from raw kind"); }
            let buckets = get_buckets(aggs[0]);
            if buckets.is_empty() { bail!("No buckets from raw kind"); }
            Ok(format!("{} buckets from raw power syntax", buckets.len()))
        }
        "S3misc.2" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("No aggs from count"); }
            let value = aggs[0].get("value").and_then(|v| v.as_u64());
            if value.is_none() { bail!("Count agg missing 'value'"); }
            Ok(format!("count={}", value.unwrap()))
        }
        "S3misc.3" => {
            let rows = get_rows(result);
            let aggs = get_aggs(result);
            // RPC converter sets limit=1 for agg-only queries (so ≤1 row expected).
            if rows.len() > 1 { bail!("Expected ≤1 rows for agg-only query, got {}", rows.len()); }
            if aggs.is_empty() { bail!("No aggs despite agg-only query"); }
            Ok(format!("{} rows, {} aggs (agg-only)", rows.len(), aggs.len()))
        }
        "S3misc.4" => {
            let rows = get_rows(result);
            let aggs = get_aggs(result);
            if rows.is_empty() { bail!("Expected rows with include_rows=true"); }
            if aggs.is_empty() { bail!("Expected aggs with include_rows=true"); }
            Ok(format!("{} rows + {} aggs", rows.len(), aggs.len()))
        }

        // ── S4C: Duplicate verification ───────────────────────────────
        "S4C.1" | "S4C.2" | "S4C.3" | "S4C.4" | "S4C.5" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("Expected duplicate aggregation results"); }
            let buckets = get_buckets(aggs[0]);
            if buckets.is_empty() { bail!("Expected at least one duplicate bucket"); }
            for (i, b) in buckets.iter().enumerate() {
                let count = b.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                if count < 2 { bail!("Bucket {i}: count={count}, expected ≥2 for duplicates"); }
            }
            Ok(format!("{} duplicate buckets, all count≥2", buckets.len()))
        }

        // ── Aggregation sample validators ─────────────────────────────
        "T139" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("Expected agg results"); }
            let buckets = get_buckets(aggs[0]);
            if buckets.is_empty() { bail!("No buckets"); }
            for (i, b) in buckets.iter().enumerate() {
                let samples = b.get("sample_rows").and_then(|v| v.as_array());
                match samples {
                    Some(s) if !s.is_empty() => {}
                    _ => bail!("Bucket {i}: missing or empty sample_rows"),
                }
            }
            Ok(format!("{} buckets, all have samples", buckets.len()))
        }
        "T140" => {
            let aggs = get_aggs(result);
            if aggs.is_empty() { bail!("Expected agg results"); }
            let buckets = get_buckets(aggs[0]);
            if buckets.is_empty() { bail!("No buckets"); }
            for (i, b) in buckets.iter().enumerate() {
                let count = b.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                if count < 2 { bail!("Bucket {i}: count={count}, expected ≥2 for duplicates"); }
                let samples = b.get("sample_rows").and_then(|v| v.as_array());
                match samples {
                    Some(s) if !s.is_empty() => {}
                    _ => bail!("Bucket {i}: missing or empty sample_rows"),
                }
            }
            Ok(format!("{} dup buckets with samples", buckets.len()))
        }

        // ── T178: --case (case-sensitive) ─────────────────────────
        "T178" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows"); }
            for (i, row) in rows.iter().enumerate() {
                let name = field_str(row, "name");
                if !name.starts_with("README") {
                    bail!("Row {i}: name '{name}' doesn't start with 'README' (case-sensitive)");
                }
            }
            Ok(format!("{} rows, all case-sensitive match", rows.len()))
        }

        // ── T180: --exact-size 0 (zero-byte files) ─────────────────
        "T180" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows"); }
            for (i, row) in rows.iter().enumerate() {
                let size = field_u64(row, "size");
                if size != 0 {
                    bail!("Row {i}: size={size}, expected 0 for --exact-size 0");
                }
            }
            Ok(format!("{} rows, all size=0", rows.len()))
        }

        // ── T181: --exact-descendants 0 (empty dirs) ────────────────
        "T181" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("No rows"); }
            Ok(format!("{} empty directory rows", rows.len()))
        }

        // ── T184: --case negative (lowercase 'readme*') ────────────
        "T184" => {
            let rows = get_rows(result);
            for (i, row) in rows.iter().enumerate() {
                let name = field_str(row, "name");
                if name.starts_with("README") {
                    bail!("Row {i}: '{name}' starts with uppercase README — case-sensitive should only match lowercase");
                }
            }
            Ok(format!("{} rows, none uppercase README", rows.len()))
        }

        // ── RPC.1: status method ────────────────────────────────────
        "RPC.1" => {
            let pid = result.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            if pid == 0 { bail!("Missing or zero 'pid' in status response"); }
            let uptime = result.get("uptime_secs").and_then(|v| v.as_u64());
            if uptime.is_none() { bail!("Missing 'uptime_secs' in status response"); }
            let state = result.get("status")
                .and_then(|v| v.get("state"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if state.is_empty() { bail!("Missing 'status.state' in status response"); }
            Ok(format!("pid={pid}, uptime={uptime:?}s, state={state}"))
        }

        // ── RPC.2: drives method ────────────────────────────────────
        //
        // Tier-aware contract (Phase 3+): Warm / Hot shards have their
        // body in RAM and must report `records > 0`; Parked / Cold
        // shards legitimately report `records: 0` because their body
        // was released by the demote ladder (bloom + trie stay
        // resident).  The `tier` field is populated by every daemon
        // from v0.5.82 onward; for older daemons we fall back to the
        // `source` string ("parked" / "cold") which carries the same
        // information.
        "RPC.2" => {
            let drives = result.get("drives").and_then(|v| v.as_array())
                .ok_or_else(|| anyhow::anyhow!("Missing 'drives' array"))?;
            if drives.is_empty() { bail!("No drives loaded"); }
            let mut resident = 0_usize;
            let mut demoted = 0_usize;
            for (i, d) in drives.iter().enumerate() {
                let letter = d.get("letter").and_then(|v| v.as_str()).unwrap_or("");
                if letter.is_empty() { bail!("Drive {i}: missing 'letter'"); }
                let records = d.get("records").and_then(|v| v.as_u64()).unwrap_or(0);
                let tier = d.get("tier").and_then(|v| v.as_str()).unwrap_or("");
                let source = d.get("source").and_then(|v| v.as_str()).unwrap_or("");
                // Body-released shards: `tier` ∈ {parked, cold, evicting,
                // unknown}, or (pre-v0.5.82 daemons) `source` ∈ {parked, cold}.
                let body_released = matches!(tier, "parked" | "cold" | "evicting" | "unknown")
                    || matches!(source, "parked" | "cold");
                if body_released {
                    demoted += 1;
                    continue;
                }
                if records == 0 {
                    bail!(
                        "Drive {i} ({letter}, tier={tier:?}, source={source:?}): \
                         0 records but shard is not Parked/Cold — body should be loaded"
                    );
                }
                resident += 1;
            }
            Ok(format!(
                "{} drives loaded ({resident} resident, {demoted} demoted)",
                drives.len(),
            ))
        }

        // ── RPC.3: info method ──────────────────────────────────────
        "RPC.3" => {
            // Info may return a match or an error depending on whether the
            // file exists in the index.  Just verify the response structure.
            if result.is_null() { bail!("Null result from info method"); }
            Ok(format!("info returned: {}", if result.is_object() { "object" } else { "value" }))
        }

        // ── RPC.4: stats method ─────────────────────────────────────
        "RPC.4" => {
            if result.is_null() { bail!("Null result from stats method"); }
            let uptime = result.get("uptime_secs")
                .or_else(|| result.get("uptime"))
                .and_then(|v| v.as_f64().or_else(|| v.as_u64().map(|u| u as f64)));
            let records = result.get("total_records")
                .and_then(|v| v.as_u64());
            let queries = result.get("queries_served")
                .or_else(|| result.get("queries_total"))
                .and_then(|v| v.as_u64());
            if uptime.is_none() { bail!("stats missing uptime field"); }
            if records.is_none() { bail!("stats missing total_records field"); }
            Ok(format!("uptime={:.0}s, records={}, queries={}",
                uptime.unwrap_or(0.0),
                records.unwrap_or(0),
                queries.unwrap_or(0)))
        }

        "no_shmem" => {
            if payload_shmem_path(result).is_some() {
                bail!("Expected inline rows (no shmem) but response has shmem_path");
            }
            let rows = get_rows(result);
            Ok(format!("{} inline rows, no shmem", rows.len()))
        }

        // Type validators: check that all row names have an extension
        // belonging to the expected semantic category.
        v @ ("type_code" | "type_document" | "type_executable" | "type_picture" | "type_system") => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("{v}: 0 rows returned"); }

            let allowed: &[&str] = match v {
                "type_code"       => &["rs","py","js","ts","c","cpp","h","hpp","cs","java","go",
                                       "rb","php","swift","kt","scala","r","lua","pl","sh","bash",
                                       "zsh","fish","ps1","psm1","psd1","vue","svelte","jsx","tsx",
                                       "mjs","cjs","coffee","dart","zig","nim","v","hs","ml","ex",
                                       "exs","erl","clj","lisp","scm","asm","s","f90","f","for",
                                       "vb","vbs","m","mm","d","ada","adb","ads","cob","cbl"],
                "type_document"   => &["pdf","doc","docx","xls","xlsx","ppt","pptx","odt","ods",
                                       "odp","rtf","txt","csv","tsv","md","rst","epub","mobi",
                                       "tex","latex","pages","numbers","key"],
                "type_executable" => &["exe","msi","bat","cmd","ps1","com","scr"],
                "type_picture"    => &["jpg","jpeg","png","gif","bmp","tif","tiff","ico","svg",
                                       "webp","heic","heif","raw","cr2","nef","dng","psd","ai",
                                       "eps","pcx","tga"],
                "type_system"     => &["sys","drv","dll","ocx","cpl","inf","cat","mum","man",
                                       "evt","evtx","etl","reg"],
                _ => unreachable!(),
            };

            let mut bad = Vec::new();
            for (i, row) in rows.iter().enumerate() {
                let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if v == "type_system" {
                    // System files are $-prefixed OR have system extensions
                    if name.starts_with('$') { continue; }
                }
                let ext = extract_ext_dot_gated(name);
                if !allowed.contains(&ext.as_str()) {
                    bad.push(format!("row {i}: {name} (ext={ext})"));
                    if bad.len() >= 3 { break; }
                }
            }
            if !bad.is_empty() {
                bail!("{v}: unexpected extensions: {}", bad.join(", "));
            }
            Ok(format!("{v}: {}/{} rows have valid extensions", rows.len(), rows.len()))
        }

        "drives_cd" => {
            let rows = get_rows(result);
            if rows.is_empty() { bail!("drives_cd: 0 rows returned"); }
            for (i, row) in rows.iter().enumerate() {
                let path = row.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if !path.starts_with("C:") && !path.starts_with("D:") {
                    bail!("drives_cd: row {i} path does not start with C: or D:: {path}");
                }
            }
            Ok(format!("drives_cd: all {} rows on C: or D:", rows.len()))
        }

        "RPC.5" => {
            // Expected-error test: unknown RPC method must return
            // JSON-RPC error code -32601 (Method not found), not a
            // crash, not a silent success.  The `result` parameter
            // here is the FULL RPC response (set up by the
            // `expect_error=true` branch in the caller).  Mirrors the
            // M700 MCP test and the daemon's own integration test at
            // `crates/uffs-daemon/tests/ipc_integration.rs:209-218`.
            let error = result.get("error").ok_or_else(|| anyhow::anyhow!(
                "RPC.5: response has no `error` object (expect_error branch should have caught this)"
            ))?;
            let code = error.get("code").and_then(Value::as_i64).ok_or_else(|| anyhow::anyhow!(
                "RPC.5: error object missing numeric `code`: {}",
                serde_json::to_string(error).unwrap_or_default()
            ))?;
            if code != -32601 {
                bail!(
                    "RPC.5: expected JSON-RPC error code -32601 (MethodNotFound), got {code}: {}",
                    serde_json::to_string(error).unwrap_or_default()
                );
            }
            let msg = error.get("message").and_then(Value::as_str).unwrap_or("");
            Ok(format!("error code -32601: {msg}"))
        }

        other => bail!("custom validator '{other}' not yet ported to RPC — implement it or add api_checks")
    }
}

/// Build a declarative RPC validator closure from a TOML `TestDef`.
fn build_rpc_validator(def: &TestDef) -> CheckFn {
    let def = def.clone();
    Box::new(move |result: &Value| {
        // Expected-error tests: the caller has already asserted the
        // response contains an `error` object (not `result`), and
        // passed us the FULL response.  Row/column/sort/anti-vacuous
        // checks are meaningless here — delegate directly to the
        // custom validator if any, otherwise succeed trivially.
        if def.expect_error.unwrap_or(false) {
            if let Some(ref name) = def.validator {
                let detail = run_rpc_custom_validator(name, result)?;
                return Ok(format!("expected-error path: {detail}"));
            }
            return Ok("expected-error path: error object present (no custom validator)".into());
        }

        let mut details: Vec<String> = Vec::new();
        let rows = get_rows(result);
        let n = rows.len();

        // ── Row count checks ─────────────────────────────────────────
        if let Some(min) = def.expect_min_rows {
            if n < min { bail!("Expected >= {min} rows, got {n}"); }
        }
        if let Some(max) = def.expect_max_rows {
            if n > max { bail!("Expected <= {max} rows, got {n}"); }
        }

        // ── Anti-vacuous guard ───────────────────────────────────────
        // Every non-error test must produce a non-empty response — either
        // search rows > 0 or aggregation results > 0.  This prevents
        // "free-pass" tests that happen to pass because they validate
        // nothing.  Tests that legitimately expect empty results should
        // set expect_min_rows = 0.
        let has_validation = !def.column_checks.is_empty()
            || !def.sort_checks.is_empty()
            || def.validator.is_some();
        let has_aggs = !get_aggs(result).is_empty();

        let is_search = def.rpc_method.as_deref().unwrap_or("search") == "search";
        if is_search && has_validation && def.expect_min_rows.is_none() && n == 0 && !has_aggs {
            bail!(
                "0 rows returned — test has {}{}{} but nothing to validate. \
                 Set expect_min_rows = 0 to explicitly allow empty results.",
                if !def.column_checks.is_empty() { "column_checks " } else { "" },
                if !def.sort_checks.is_empty() { "sort_checks " } else { "" },
                if def.validator.is_some() { "validator" } else { "" },
            );
        }

        // Universal anti-vacuous: even if no explicit validators, the RPC
        // must return SOMETHING (rows or aggregations) unless the test
        // explicitly allows empty results or uses a non-search RPC method
        // (where "rows" don't exist in the response).
        let is_search_method = def.rpc_method.as_deref().unwrap_or("search") == "search";
        if is_search_method && def.expect_min_rows.is_none() && n == 0 && !has_aggs {
            bail!(
                "0 rows and 0 aggregations — empty response. \
                 Set expect_min_rows = 0 to explicitly allow empty results.",
            );
        }

        details.push(format!("{n} rows{}", if has_aggs { format!(", {} aggs", get_aggs(result).len()) } else { String::new() }));

        // ── Column checks ────────────────────────────────────────────
        for (i, row) in rows.iter().enumerate() {
            for check in &def.column_checks {
                apply_rpc_column_check(i, row, check)?;
            }
        }

        // ── Sort checks ──────────────────────────────────────────────
        for sc in &def.sort_checks {
            let json_key = csv_col_to_json(&sc.column);
            // Require at least 2 rows to verify sort order.
            if !def.sort_checks.is_empty() && def.expect_min_rows.is_none() && rows.len() < 2 {
                bail!(
                    "Only {} row(s) — sort check on '{}' cannot verify ordering. \
                     Need ≥ 2 rows to validate sort.",
                    rows.len(), sc.column
                );
            }
            if rows.len() >= 2 {
                match sc.sort_type.as_str() {
                    "string" => {
                        let vals: Vec<String> = rows.iter().map(|r| {
                            rpc_field_computed(r, json_key)
                                .unwrap_or_else(|| field_str(r, json_key))
                                .to_lowercase()
                        }).collect();
                        for w in vals.windows(2) {
                            match sc.order.as_str() {
                                "asc"  => { if w[0] > w[1] { bail!("Not asc: {} > {}", w[0], w[1]); } }
                                "desc" => { if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); } }
                                _ => {}
                            }
                        }
                    }
                    _ => {
                        let vals: Vec<u64> = rows.iter().map(|r| {
                            rpc_field_computed(r, json_key)
                                .and_then(|s| s.parse().ok())
                                .unwrap_or_else(|| field_u64(r, json_key))
                        }).collect();
                        for w in vals.windows(2) {
                            match sc.order.as_str() {
                                "asc"  => { if w[0] > w[1] { bail!("Not asc: {} > {}", w[0], w[1]); } }
                                "desc" => { if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); } }
                                _ => {}
                            }
                        }
                    }
                }
            }
            details.push(format!("sorted {} {}", sc.column, sc.order));
        }

        // ── API-specific checks (api_checks.*) ───────────────────────
        let aggs = get_aggs(result);

        // api_checks.expect_agg_results: validate agg result count.
        if let Some(expected) = def.api_checks.expect_agg_results {
            if aggs.len() < expected {
                bail!("Expected >= {expected} agg results, got {}", aggs.len());
            }
            details.push(format!("{} agg results", aggs.len()));
        }

        // api_checks.result_has_key: validate top-level keys exist.
        // Delegates to `result_has_virtual_key` so legacy keys like
        // `rows` / `shmem_path` remain valid assertions after the
        // v0.5.62 `SearchPayload` unification.
        for key in &def.api_checks.result_has_key {
            if !result_has_virtual_key(result, key.as_str()) {
                bail!("result missing expected key: {key}");
            }
        }

        // api_checks.agg_label_contains: validate agg labels/kinds.
        // Matches against either `label` or `kind` field (whichever exists).
        for label in &def.api_checks.agg_label_contains {
            let found = aggs.iter().any(|a| {
                let lbl = a.get("label").and_then(|l| l.as_str());
                let kind = a.get("kind").and_then(|k| k.as_str());
                lbl == Some(label.as_str()) || kind == Some(label.as_str())
            });
            if !found {
                let actual: Vec<String> = aggs.iter()
                    .map(|a| {
                        let lbl = a.get("label").and_then(|l| l.as_str()).unwrap_or("-");
                        let kind = a.get("kind").and_then(|k| k.as_str()).unwrap_or("-");
                        format!("label={lbl},kind={kind}")
                    })
                    .collect();
                bail!("agg label/kind '{label}' not found; actual: {actual:?}");
            }
        }

        // api_checks.agg_kind_contains: verify specific kind values.
        for kind in &def.api_checks.agg_kind_contains {
            let found = aggs.iter().any(|a|
                a.get("kind").and_then(|k| k.as_str()) == Some(kind.as_str())
            );
            if !found {
                let actual: Vec<&str> = aggs.iter()
                    .filter_map(|a| a.get("kind").and_then(|k| k.as_str()))
                    .collect();
                bail!("agg kind '{kind}' not found; actual: {actual:?}");
            }
        }

        // api_checks.bucket_min_count: validate total buckets.
        let all_buckets: Vec<&Value> = aggs.iter()
            .flat_map(|a| get_buckets(a))
            .collect();
        if let Some(min_buckets) = def.api_checks.bucket_min_count {
            if all_buckets.len() < min_buckets {
                bail!("Expected >= {min_buckets} total buckets, got {}", all_buckets.len());
            }
            details.push(format!("{} total buckets", all_buckets.len()));
        }

        // api_checks.bucket_key_contains: verify specific bucket keys.
        for key_sub in &def.api_checks.bucket_key_contains {
            let found = all_buckets.iter().any(|b|
                b.get("key").and_then(|k| k.as_str())
                    .map_or(false, |k| k.contains(key_sub.as_str()))
            );
            if !found {
                let actual: Vec<&str> = all_buckets.iter()
                    .filter_map(|b| b.get("key").and_then(|k| k.as_str()))
                    .take(10)
                    .collect();
                bail!("bucket key containing '{key_sub}' not found; first 10 keys: {actual:?}");
            }
        }

        // api_checks.agg_stats_has_keys: verify stats object fields.
        if !def.api_checks.agg_stats_has_keys.is_empty() {
            let any_has_stats = aggs.iter().any(|a| a.get("stats").is_some());
            if !any_has_stats {
                bail!("No agg result has a 'stats' object");
            }
            for key in &def.api_checks.agg_stats_has_keys {
                let found = aggs.iter().any(|a|
                    a.get("stats").and_then(|s| s.get(key.as_str())).is_some()
                );
                if !found { bail!("stats missing key '{key}'"); }
            }
            details.push("stats verified".into());
        }

        // api_checks.bucket_has_verified_true
        if def.api_checks.bucket_has_verified_true == Some(true) {
            let found = all_buckets.iter().any(|b|
                b.get("verified").and_then(|v| v.as_bool()) == Some(true)
            );
            if !found { bail!("No bucket has verified=true"); }
            details.push("verified=true found".into());
        }

        // api_checks.bucket_has_samples
        if def.api_checks.bucket_has_samples == Some(true) {
            let found = all_buckets.iter().any(|b|
                b.get("sample_rows").and_then(|s| s.as_array())
                    .map_or(false, |a| !a.is_empty())
            );
            if !found { bail!("No bucket has non-empty sample_rows"); }
            details.push("samples found".into());
        }

        // api_checks.bucket_has_sub_buckets
        if def.api_checks.bucket_has_sub_buckets == Some(true) {
            let found = all_buckets.iter().any(|b|
                b.get("sub_buckets").and_then(|s| s.as_array())
                    .map_or(false, |a| !a.is_empty())
            );
            if !found { bail!("No bucket has non-empty sub_buckets"); }
            details.push("sub_buckets found".into());
        }

        // api_checks.agg_has_cursor
        if def.api_checks.agg_has_cursor == Some(true) {
            let found = aggs.iter().any(|a| a.get("next_cursor").is_some());
            if !found { bail!("No agg result has next_cursor"); }
            details.push("cursor found".into());
        }

        // api_checks.total_count_min
        if let Some(min_tc) = def.api_checks.total_count_min {
            let tc = result.get("total_count").and_then(|v| v.as_u64()).unwrap_or(0);
            if tc < min_tc { bail!("total_count {tc} < expected min {min_tc}"); }
            details.push(format!("total_count={tc}"));
        }

        // api_checks.shmem_count_min — verify shmem transfer was used and
        // cross-check the on-disk row count against the JSON `shmem_count`.
        // Both accessors route through the `payload` enum for
        // post-v0.5.62 shapes while still accepting the legacy flat
        // fields.
        if let Some(min_sc) = def.api_checks.shmem_count_min {
            let shmem_path = payload_shmem_path(result);
            let shmem_count = payload_shmem_count(result).unwrap_or(0);
            if shmem_path.is_none() {
                bail!("Expected shmem_path in response (shmem_count_min={min_sc}) but not present — \
                       daemon returned inline rows instead of shmem");
            }
            if shmem_count < min_sc {
                bail!("shmem_count {shmem_count} < expected min {min_sc}");
            }
            // Read the shmem binary header to verify on-disk row count.
            let path = shmem_path.unwrap_or("");
            match std::fs::read(path) {
                Ok(data) if data.len() >= 48 => {
                    let magic  = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    let ver    = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                    let on_disk_rows = u64::from_le_bytes([
                        data[8], data[9], data[10], data[11],
                        data[12], data[13], data[14], data[15],
                    ]);
                    if magic != 0x5346_4655 {
                        bail!("shmem file bad magic: {magic:#x} (expected 0x53464655 = \"UFFS\")");
                    }
                    if ver == 0 || ver > 10 {
                        bail!("shmem file unlikely version: {ver} (expected 1–10)");
                    }
                    if on_disk_rows != shmem_count {
                        bail!("shmem header row_count={on_disk_rows} != JSON shmem_count={shmem_count}");
                    }
                    details.push(format!(
                        "shmem OK: {on_disk_rows} rows on disk, file={}, {}bytes",
                        path.rsplit('/').next().unwrap_or(path),
                        data.len(),
                    ));
                    // Clean up the shmem file so it doesn't leak.
                    let _ = std::fs::remove_file(path);
                }
                Ok(data) => {
                    bail!("shmem file too small: {} bytes (need >= 48)", data.len());
                }
                Err(e) => {
                    bail!("shmem file read failed: {e} (path={path})");
                }
            }
        }

        // api_checks.agg_exact
        if let Some(expected) = def.api_checks.agg_exact {
            let found = aggs.iter().any(|a|
                a.get("exact").and_then(|v| v.as_bool()) == Some(expected)
            );
            if !found { bail!("No agg result has exact={expected}"); }
            details.push(format!("exact={expected}"));
        }

        // ── Custom validator ─────────────────────────────────────────
        if let Some(ref name) = def.validator {
            let custom = run_rpc_custom_validator(name, result)?;
            details.push(custom);
        }

        Ok(details.join(", "))
    })
}

/// Locate the TOML test definitions file.
/// Return the directory containing per-group test definition files.
fn find_test_defs_dir() -> PathBuf {
    let ws = find_workspace_root();
    ws.join("scripts").join("tests").join("definitions")
}

/// Load all `[[test]]` entries from `definitions/*.toml` files, sorted
/// lexicographically (00-warmup first, 11-rpc last).
fn load_all_test_defs() -> Vec<TestDef> {
    let dir = find_test_defs_dir();
    if !dir.is_dir() {
        eprintln!("  ❌ Test definitions directory not found: {}", dir.display());
        std::process::exit(1);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| { eprintln!("  ❌ Cannot read {}: {e}", dir.display()); std::process::exit(1); })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map_or(false, |ext| ext == "toml"))
        .collect();
    files.sort();

    let mut all_defs: Vec<TestDef> = Vec::new();
    let mut file_count = 0;
    for path in &files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| { eprintln!("  ❌ Cannot read {}: {e}", path.display()); std::process::exit(1); });
        let defs: TestDefsFile = toml::from_str(&content)
            .unwrap_or_else(|e| { eprintln!("  ❌ Cannot parse {}: {e}", path.display()); std::process::exit(1); });
        all_defs.extend(defs.test);
        file_count += 1;
    }
    eprintln!("  Loaded {} test definitions from {} files in definitions/",
        all_defs.len(), file_count);
    all_defs
}

/// Load test definitions from TOML and convert to API `Vec<TestSpec>`.
///
/// Returns `(api_specs, cli_only_ids)` — the second vec contains test IDs
/// that are cli-only, so the caller can inform the user when their filter
/// matches one of these.
fn load_tests_from_toml() -> (Vec<TestSpec>, Vec<String>) {
    let all_defs = load_all_test_defs();

    let mut specs = Vec::with_capacity(all_defs.len());
    let mut cli_only_ids: Vec<String> = Vec::new();
    let mut skipped = 0;
    let mut filtered_out = 0;
    for def in &all_defs {
        if def.skip.unwrap_or(false) { skipped += 1; continue; }
        if !def.targets.iter().any(|t| t == "api") {
            filtered_out += 1;
            cli_only_ids.push(def.id.clone());
            continue;
        }

        // Determine RPC params: explicit rpc_params > auto-derived from cli_args.
        let params: Value = if let Some(ref p) = def.rpc_params {
            serde_json::from_str(p).unwrap_or_else(|_| cli_args_to_rpc_params(&def.cli_args))
        } else {
            cli_args_to_rpc_params(&def.cli_args)
        };

        let method = def.rpc_method.clone().unwrap_or_else(|| "search".to_string());
        // `def.name` already includes the test ID (e.g. "T08 --min/max-size").
        let display = def.name.clone();
        let validator = build_rpc_validator(def);

        specs.push(TestSpec {
            name: display,
            method,
            params: Some(params),
            check: validator,
            cli_args: def.cli_args.clone(),
            expect_error: def.expect_error.unwrap_or(false),
        });
    }
    if skipped > 0 || filtered_out > 0 {
        eprintln!("  ({} api tests, {} cli-only skipped, {} disabled)",
            specs.len(), filtered_out, skipped);
    }
    (specs, cli_only_ids)
}

// ── Test Definitions ─────────────────────────────────────────────────────────

fn ensure_daemon_ready(args: &ScriptArgs) -> bool {
    let bin = &args.bin;

    // Step 1: Check status.
    eprintln!("  Checking daemon status...");
    let status = Command::new(bin)
        .args(["--daemon", "status"])
        .output();
    match status {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let combined = format!("{stdout}{stderr}");
            let lower = combined.to_lowercase();

            if lower.contains("ready") {
                // Check whether drives are actually loaded.
                // Status output contains "Drives: (none loaded)" or "Drives: N".
                let has_drives = !lower.contains("none loaded")
                    && !lower.contains("drives:        0")
                    && !lower.contains("drives: 0");

                if has_drives {
                    eprintln!("  Daemon: {} ✓", "Ready".green().bold());
                    for line in combined.lines() {
                        eprintln!("    {line}");
                    }
                    return true;
                }

                // Daemon is Ready but has zero drives — stale/useless.
                eprintln!("  Daemon is {} but has {} — restarting with data source...",
                    "Ready".yellow(), "zero drives loaded".red().bold());
                for line in combined.lines() {
                    eprintln!("    {line}");
                }
                // Use `daemon kill` (synchronous PID kill + PID file + socket
                // cleanup) instead of `daemon stop` (fire-and-forget shutdown
                // RPC).  The stop RPC returns immediately while the daemon is
                // still shutting down; a follow-up `daemon start` races the
                // stop and often sees "already running" because `connect_raw`
                // succeeds against the still-alive socket.  `kill` is
                // synchronous by design and always leaves a clean slate.
                eprintln!("  Killing stale daemon...");
                let _ = Command::new(bin)
                    .args(["--daemon", "kill"])
                    .output();
                // Brief pause to let the socket slot fully release on macOS
                // (Darwin retains the inode ~50 ms after the process exits).
                std::thread::sleep(std::time::Duration::from_millis(250));
            } else if lower.contains("not running") {
                eprintln!("  Daemon is not running, starting...");
            } else {
                eprintln!("  Daemon status unclear, attempting start...");
                for line in combined.lines() {
                    eprintln!("    {line}");
                }
            }
        }
        Err(e) => {
            eprintln!("  Cannot check daemon status: {e}");
            eprintln!("  Attempting start...");
        }
    }

    // Step 2: Start daemon (blocks until ready).
    let mut start_args = vec!["--daemon".to_string(), "start".to_string()];
    if let Some(flag) = args.source_flag {
        start_args.push(flag.to_string());
        start_args.push(args.source_path.clone());
    }
    let cli_str = format!("{bin} {}", start_args.join(" "));
    eprintln!("    ↳ {}", cli_str.dimmed());
    let t0 = Instant::now();
    let result = Command::new(bin)
        .args(start_args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
        .output();
    let ms = t0.elapsed().as_millis();

    match result {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let combined = format!("{stdout}{stderr}");
            if o.status.success() {
                eprintln!("  Daemon: {} ({ms}ms)", "Started".green().bold());
                for line in combined.lines() {
                    eprintln!("    {line}");
                }
                true
            } else {
                eprintln!("  Daemon start {} (exit {}) — {ms}ms",
                    "FAILED".red().bold(), o.status);
                for line in combined.lines() {
                    eprintln!("    {line}");
                }
                false
            }
        }
        Err(e) => {
            eprintln!("  Daemon start {} — {e}", "FAILED".red().bold());
            false
        }
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

/// Extract the test ID (e.g. `"T04"`) from a test name like
/// `"T04 search foo bar"`.
///
/// The validation suites use space-prefixed test names where the first
/// whitespace-separated token is the canonical ID.  Mirrors
/// `cli-validation.rs::test_id` so the three suites stay in lock-step
/// on retest-command shape.
fn test_id(name: &str) -> String {
    name.split_whitespace()
        .next()
        .unwrap_or(name)
        .to_uppercase()
}

/// Find the longest common prefix of a set of strings.
///
/// Used by the failure-summary block to suggest a single
/// `--tests <prefix>` re-run command when every failed test ID shares
/// a common stem.  Mirrors `mcp-validation.rs::common_prefix` and
/// `cli-validation.rs::common_prefix` byte-for-byte.
fn common_prefix(strings: &[&str]) -> String {
    if strings.is_empty() { return String::new(); }
    let first = strings[0];
    let mut len = first.len();
    for s in &strings[1..] {
        len = len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b { len = len.min(i); break; }
        }
    }
    first[..len].to_string()
}

fn main() {
    let script_start = Instant::now();
    let _lock = ValidationLock::acquire();
    let args = parse_args();
    let sock = daemon_socket_path();

    eprintln!("╔═══════════════════════════════════════════════════════════════╗");
    eprintln!("║  UFFS API Validation Suite (JSON-RPC)                        ║");
    eprintln!("╚═══════════════════════════════════════════════════════════════╝");
    eprintln!("  Binary:  {}", args.bin.cyan());
    if let Some(flag) = args.source_flag {
        eprintln!("  Source:  {} {}", flag, args.source_path.cyan());
    } else {
        eprintln!("  Source:  {}", "(live NTFS drives)".cyan());
    }
    eprintln!("  Socket:  {}", sock.cyan());
    eprintln!();

    // Ensure daemon is running, started with correct data source, and ready.
    let t_daemon = Instant::now();
    if !ensure_daemon_ready(&args) {
        eprintln!("{}", "Cannot connect to daemon — aborting.".red().bold());
        std::process::exit(1);
    }
    let daemon_ms = t_daemon.elapsed().as_millis();

    // ── T-1: RPC gate check ─────────────────────────────────────────────
    // If the RPC channel doesn't work, abort immediately.
    // Don't waste time running 190+ tests that will all fail.
    eprintln!("  T-1  RPC connectivity (keepalive)...");
    let t0_rpc = Instant::now();
    match rpc_call(&sock, "keepalive", None) {
        Ok(resp) => {
            let ms = t0_rpc.elapsed().as_millis();
            let result = resp.get("result");
            if result.is_none() {
                eprintln!("  {} T-1 FAILED — keepalive returned no 'result': {}",
                    "❌".red(),
                    serde_json::to_string(&resp).unwrap_or_default());
                eprintln!("  {}", "RPC channel broken — aborting.".red().bold());
                std::process::exit(1);
            }
            let full_resp = serde_json::to_string_pretty(&resp).unwrap_or_default();
            eprintln!("  {}  T-1 RPC connectivity — {} ({ms}ms)",
                "✅".green(), "OK".green().bold());
            eprintln!("  Response:");
            for line in full_resp.lines() {
                eprintln!("    {}", line.dimmed());
            }
        }
        Err(e) => {
            let ms = t0_rpc.elapsed().as_millis();
            eprintln!("  {} T-1 FAILED ({ms}ms) — {e}", "❌".red());
            eprintln!("  {}", "RPC channel broken — aborting.".red().bold());
            eprintln!("  Socket: {}", sock);
            std::process::exit(1);
        }
    }
    eprintln!();

    // ── T-0.5: Data gate — verify daemon has loaded drives with data ────
    // A daemon with zero drives loaded will return 0 rows for every search,
    // causing tests with column_checks/sort_checks to pass vacuously.
    eprintln!("  T-0.5  Data gate (drives loaded)...");
    let t0_data = Instant::now();
    match rpc_call(&sock, "drives", None) {
        Ok(resp) => {
            let ms = t0_data.elapsed().as_millis();
            // Response shape: {"result": {"drives": [{"letter":"C","records":N,"source":"..."},...]}}
            let drive_arr = resp.get("result")
                .and_then(|r| r.get("drives"))
                .and_then(|d| d.as_array());
            let drive_count = drive_arr.map(|a| a.len()).unwrap_or(0);
            if drive_count == 0 {
                eprintln!("  {} T-0.5 FAILED ({ms}ms) — daemon has {} drives loaded",
                    "❌".red(), "ZERO".red().bold());
                eprintln!("  {}", "No data loaded — all search tests will return 0 rows.".red().bold());
                eprintln!("  {}", "Tests cannot validate anything without data. Aborting.".red().bold());
                eprintln!();
                eprintln!("  Hint: start the daemon with data:");
                eprintln!("    uffs --daemon start --data-dir ~/uffs_data");
                eprintln!("    uffs --daemon start --mft-file /path/to/C_mft.iocp");
                eprintln!();
                eprintln!("  Raw response: {}", serde_json::to_string_pretty(&resp).unwrap_or_default());
                std::process::exit(1);
            }
            // Log the drive details for debugging.
            let drive_info = drive_arr
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            let letter = d.get("letter").and_then(|v| v.as_str()).unwrap_or("?");
                            let records = d.get("records").and_then(|v| v.as_u64()).unwrap_or(0);
                            Some(format!("{}:({} records)", letter, records))
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            eprintln!("  {}  T-0.5 Data gate — {} ({ms}ms)",
                "✅".green(), "OK".green().bold());
            eprintln!("    {} drive(s) loaded: {}", drive_count, drive_info);
        }
        Err(e) => {
            let ms = t0_data.elapsed().as_millis();
            eprintln!("  {} T-0.5 FAILED ({ms}ms) — cannot query drives: {e}", "❌".red());
            eprintln!("  {}", "Cannot verify data is loaded — aborting.".red().bold());
            std::process::exit(1);
        }
    }
    eprintln!();

    // All tests are defined in test-definitions.toml.
    let (mut specs, cli_only_ids) = load_tests_from_toml();

    // Apply filter if specified.  Supports comma-separated IDs/prefixes.
    if let Some(ref filter) = args.filter {
        let filters: Vec<String> = filter
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        specs.retain(|s| {
            let lower = s.name.to_lowercase();
            filters.iter().any(|f| lower.contains(f))
        });
        eprintln!("  Filter: '{}' → {} tests", filter, specs.len());

        // Show which filtered tests are cli-only.
        let matched_cli: Vec<&String> = cli_only_ids.iter().filter(|id| {
            let upper = id.to_uppercase();
            filters.iter().any(|f| upper.to_lowercase().contains(f) || id.to_lowercase().contains(f))
        }).collect();
        if !matched_cli.is_empty() {
            for id in &matched_cli {
                eprintln!("  {} {} — cli-only test, run with: rust-script scripts/windows/cli-validation --tests {}",
                    "ℹ".cyan(), id, id);
            }
        }
    }

    eprintln!("  Running {} tests...\n", specs.len());

    let t0 = Instant::now();
    let results = run_tests(&sock, specs, &args);
    let elapsed = t0.elapsed();

    eprintln!();
    // Results already printed inline during execution.
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let total = passed + failed;
    let test_wall_ms = elapsed.as_millis();
    let test_sum_ms: u128 = results.iter().map(|r| r.elapsed_ms).sum();
    let test_avg_ms = if total > 0 { test_sum_ms / total as u128 } else { 0 };
    let test_count = total;
    let max_par = max_parallelism(); // matches chunk_size in run_tests
    let slowest = results.iter().max_by_key(|r| r.elapsed_ms);
    let fastest = results.iter().filter(|r| r.passed).min_by_key(|r| r.elapsed_ms);
    eprintln!();

    if failed > 0 {
        eprintln!("  {} {failed}/{total} FAILED",
            "❌".red());
        eprintln!();
        eprintln!("  ┌─ Failed Tests ──────────────────────────────────────────────────────┐");
        for r in &results {
            if !r.passed {
                eprintln!("  │");
                eprintln!("  │  {} {}", "❌".red(), r.name);
                eprintln!("  │  {}: {}", "Error".red().bold(), r.message);
                eprintln!("  │  {}:   {}", "CLI".yellow().bold(), r.cli_command);
                eprintln!("  │  {}:   {}", "RPC".cyan().bold(), r.rpc_call);
            }
        }
        eprintln!("  │");
        eprintln!("  └──────────────────────────────────────────────────────────────────────┘");
    } else {
        eprintln!("  {} {passed}/{total} passed",
            "✅".green());
    }

    // When running a small number of tests (e.g. --tests filter), show
    // full CLI + RPC details for every test — same info that is shown on
    // failure, so users can replay or inspect.  Skip when every test
    // already appeared in the failure box to avoid duplicate output.
    if total <= 10 && total > 0 && passed > 0 {
        eprintln!();
        eprintln!("  ┌─ Test Details ───────────────────────────────────────────────────────┐");
        for r in &results {
            let icon = if r.passed { "✅" } else { "❌" };
            eprintln!("  │");
            eprintln!("  │  {icon} {} ({}ms)", r.name, r.elapsed_ms);
            eprintln!("  │  {}: {}", "Result".bold(), r.message);
            eprintln!("  │  {}:    {}", "CLI".yellow().bold(), r.cli_command);
            eprintln!("  │  {}:    {}", "RPC".cyan().bold(), r.rpc_call);
        }
        eprintln!("  │");
        eprintln!("  └──────────────────────────────────────────────────────────────────────┘");
    }

    let script_total_ms = script_start.elapsed().as_millis();
    let daemon_version = capture_daemon_version(&args.bin);
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("  {} Timing Breakdown", "⏱".dimmed());
    eprintln!("  ─────────────────────────────────────────────────────");
    eprintln!("  Daemon version:       {daemon_version}");
    eprintln!("  Daemon ready:         {:>7}ms  (status check + start + drive load)", daemon_ms);
    eprintln!("  ─────────────────────────────────────────────────────");
    eprintln!("  Tests wall time:      {:>7}ms  ({test_count} tests, parallelism: {max_par})", test_wall_ms);
    eprintln!("  Tests sum time:       {:>7}ms  (total CPU across all tests)", test_sum_ms);
    eprintln!("  Tests avg time:       {:>7}ms  (per test)", test_avg_ms);
    if let Some(s) = slowest {
        eprintln!("  Slowest test:         {:>7}ms  {}", s.elapsed_ms, s.name.dimmed());
    }
    if let Some(f) = fastest {
        eprintln!("  Fastest test:         {:>7}ms  {}", f.elapsed_ms, f.name.dimmed());
    }
    eprintln!("  ─────────────────────────────────────────────────────");
    eprintln!("  Script total:         {:>7}ms", script_total_ms);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // ═══ Daemon STATUS + STATS — observe-only post-run snapshot ═════════
    //
    // The validation suite is a strict observer: it does not kill orphan
    // processes or mutate the host in any way.  Those concerns live in
    // `scripts/dev/orphan-cleanup.rs` (callable via `just orphan`).
    print_uffs_command_block(&args.bin, &["--daemon", "status"], "═══ Daemon STATUS ═══");
    print_uffs_command_block(&args.bin, &["--daemon", "status", "-v"],  "═══ Daemon STATUS -v (perf) ═══");

    if failed > 0 {
        // Build retest command with failed test IDs.  Same shape as
        // `mcp-validation.rs` and `cli-validation.rs` so the three
        // validation suites give the operator a one-line copy-pasteable
        // replay regardless of which suite produced the failure.
        let failed_ids: Vec<String> = results.iter()
            .filter(|r| !r.passed)
            .map(|r| test_id(&r.name))
            .collect();
        eprintln!();
        eprintln!("  Retest failed using:");
        let joined = failed_ids.join(",");
        eprintln!("    rust-script scripts/windows/api-validation.rs --tests {joined}");
        if failed_ids.len() > 1 {
            // Suggest a prefix shortcut when every failed ID shares one
            // (e.g. RPC.4 / RPC.7 → RPC.).
            let id_refs: Vec<&str> = failed_ids.iter().map(String::as_str).collect();
            let prefix = common_prefix(&id_refs);
            if !prefix.is_empty() && prefix.len() >= 2 {
                eprintln!();
                eprintln!("  Or by prefix:");
                eprintln!("    rust-script scripts/windows/api-validation.rs --tests {prefix}");
            }
        }
        std::process::exit(1);
    }
}

// ── Post-run STATUS / STATS rendering ───────────────────────────────────────

/// Run `<bin> <args...>` and render its stdout under a 2-space-indented
/// `<header>` so the validation summary embeds the daemon's own
/// `daemon status` / `daemon stats` output verbatim.
///
/// The CLI already formats these views nicely (Index heap, RSS, mimalloc,
/// per-drive breakdown, agg-cache hit-rate, etc.), so re-formatting here
/// would just diverge them over time.  Failures are surfaced inline
/// rather than aborting — the STATUS block is observability, not a gate.
fn print_uffs_command_block(bin: &str, args: &[&str], header: &str) {
    eprintln!();
    eprintln!("  {header}");
    match Command::new(bin).args(args).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                eprintln!("    {line}");
            }
        }
        Ok(out) => {
            eprintln!("    (command failed: exit {})", out.status.code().unwrap_or(-1));
            let stderr = String::from_utf8_lossy(&out.stderr);
            for line in stderr.lines() {
                eprintln!("    {line}");
            }
        }
        Err(e) => {
            eprintln!("    (failed to run: {e})");
        }
    }
}