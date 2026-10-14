// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Synchronous IPC client for the UFFS daemon.
//!
//! Zero async overhead — uses blocking `UnixStream` directly.
//! Designed for the CLI thin client where every invocation is a single
//! request-response round-trip.
//!
//! # Platform support
//!
//! | Platform    | Transport                                          |
//! |-------------|----------------------------------------------------|
//! | macOS/Linux | `std::os::unix::net::UnixStream`                   |
//! | Windows     | Named pipe via `std::fs::OpenOptions` (no Winsock) |

use std::io::{self, BufRead as _, BufReader, Read, Write};

use crate::connect_sync_autostart::auto_start_daemon;
use crate::daemon_ctl::{pid_file_path, socket_path};
use crate::daemon_spawn::{ElevationPolicy, resolve_elevation_policy};
use crate::error::ClientError;
use crate::protocol::response::DaemonStatus;

/// Synchronous thin client for the UFFS daemon.
///
/// One request, one response, no event loop.  Phase 3b decisions:
/// see [`crate::connect::UffsClient`] (`deadline_guard` is sync-only).
pub struct UffsClientSync {
    /// Buffered reader for the IPC socket.
    reader: BufReader<Box<dyn Read + Send>>,
    /// Writer half of the IPC socket.
    writer: Box<dyn Write + Send>,
    /// Monotonically increasing JSON-RPC request ID.
    next_id: u64,
    /// Cached `DaemonStatus` from the most recent `status` RPC.
    ///
    /// `deep_health_check` populates this, letting
    /// [`Self::await_ready`] short-circuit a redundant round-trip
    /// when the daemon is already `Ready` (~5–10 ms saving per CLI
    /// invocation on Windows named pipes — Run 10 Part B bisect in
    /// `docs/research/perf-phase2-measurement-plan.md`).  `None`
    /// means "no fresh status observed yet"; callers needing a
    /// ground-truth signal must issue a new [`Self::status`] RPC.
    cached_status: Option<DaemonStatus>,
    /// Windows-only: per-RPC deadline guard.
    ///
    /// Unix enforces the deadline via `SO_RCVTIMEO` / `SO_SNDTIMEO`
    /// directly on the stream at connect time; Windows named pipes
    /// have no equivalent, so we spawn a watchdog thread that calls
    /// `CancelSynchronousIo` when an RPC exceeds its deadline.  See
    /// [`crate::windows_deadline`] for the full rationale.
    ///
    /// `None` when the deadline is disabled
    /// (`UFFS_CLIENT_TIMEOUT_SECS=0`) — in that case no watchdog
    /// thread is spawned and [`Self::send_request`] skips the
    /// arm/disarm calls entirely.
    #[cfg(windows)]
    deadline_guard: Option<crate::windows_deadline::WindowsDeadlineGuard>,
}

impl UffsClientSync {
    /// Assemble a client from its reader/writer halves, with no
    /// Windows deadline guard.
    ///
    /// Crate-internal constructor used by the Unix
    /// `platform_connect` (living in
    /// [`crate::connect_sync_platform`]) — lets that split `impl`
    /// build a value without touching the private fields directly.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn from_parts(
        reader: BufReader<Box<dyn Read + Send>>,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            cached_status: None,
        }
    }

    /// Assemble a client from its parts and an optional Windows
    /// deadline guard.
    ///
    /// Crate-internal constructor used by the Windows
    /// `platform_connect` (living in
    /// [`crate::connect_sync_platform`]).
    #[cfg(windows)]
    #[must_use]
    pub(crate) fn from_parts_with_deadline_guard(
        reader: BufReader<Box<dyn Read + Send>>,
        writer: Box<dyn Write + Send>,
        deadline_guard: Option<crate::windows_deadline::WindowsDeadlineGuard>,
    ) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            cached_status: None,
            deadline_guard,
        }
    }

    /// Test-only constructor that accepts arbitrary reader/writer
    /// halves and skips the Windows deadline guard.
    ///
    /// Used by the `deep_health_check` and RPC-wire tests in
    /// [`crate::connect_sync_tests`] to drive a fully in-memory
    /// mock daemon without opening a real socket.  `#[cfg(test)]`
    /// keeps it out of production builds entirely.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_parts_for_test(
        reader: BufReader<Box<dyn Read + Send>>,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            cached_status: None,
            #[cfg(windows)]
            deadline_guard: None,
        }
    }

    /// Test-only: pre-seed the cached `DaemonStatus` so
    /// [`crate::connect_sync_tests`] can exercise the
    /// [`Self::await_ready`] short-circuit without round-tripping
    /// a real probe through the in-memory mock.
    #[cfg(test)]
    pub(crate) fn set_cached_status_for_test(&mut self, status: DaemonStatus) {
        self.cached_status = Some(status);
    }

    /// Connect to a running daemon, or auto-start one if not running.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` if the daemon cannot be reached after
    /// retries, or `DaemonStartFailed` if auto-start fails.
    pub fn connect() -> Result<Self, ClientError> {
        Self::connect_with_args(&[])
    }

    /// Connect without auto-starting the daemon.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` if no daemon is listening.
    pub fn connect_raw() -> Result<Self, ClientError> {
        Self::platform_connect()
            .map_err(|err| ClientError::ConnectionFailed(format!("No daemon is running: {err}")))
    }

    /// Connect without auto-starting the daemon, with an explicit
    /// per-RPC `deadline` in place of `UFFS_CLIENT_TIMEOUT_SECS`.
    ///
    /// For a short diagnostic probe — e.g. asking the daemon what it is
    /// still busy with after the main request outlived its own deadline —
    /// which must not inherit the long default and hang on a stuck daemon.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` if no daemon is listening.
    pub fn connect_probe(deadline: core::time::Duration) -> Result<Self, ClientError> {
        Self::platform_connect_with_deadline(Some(deadline))
            .map_err(|err| ClientError::ConnectionFailed(format!("No daemon is running: {err}")))
    }

    /// Connect to a daemon at an explicit, caller-supplied `endpoint`
    /// (a Unix socket path, or on Windows a named-pipe path) —
    /// bypassing the well-known per-user socket/pipe resolution,
    /// autostart, and PID-file identity verification entirely.
    ///
    /// For talking to an ephemeral, job-scoped `uffsd` instance the
    /// caller spawned itself with `--ephemeral-id <id>` (and therefore
    /// already trusts) — see [`crate::daemon_ctl::ephemeral_endpoint`]
    /// for computing the matching `endpoint` string. Not for the
    /// resident daemon: use [`Self::connect`] for that.
    ///
    /// # Errors
    /// Returns `ConnectionFailed` if the endpoint can't be opened.
    pub fn connect_at(endpoint: &str) -> Result<Self, ClientError> {
        #[cfg(unix)]
        {
            Self::platform_connect_at(std::path::Path::new(endpoint))
        }
        #[cfg(windows)]
        {
            Self::platform_connect_at(endpoint)
        }
    }

    /// Connect to a running daemon, or auto-start one with extra CLI args.
    ///
    /// Auto-start uses the default
    /// `ElevationPolicy::RequireExistingElevation`.  On Windows, if
    /// the daemon must be spawned and the current process is not
    /// elevated, this returns
    /// [`crate::error::ClientError::DaemonNeedsElevation`] instead of
    /// silently triggering UAC.  See
    /// [`Self::connect_with_elevation`] for the opt-in variant.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed`, `DaemonStartFailed`, or
    /// `DaemonNeedsElevation` (Windows, non-admin shell only).
    pub fn connect_with_args(spawn_args: &[std::ffi::OsString]) -> Result<Self, ClientError> {
        Self::connect_with_args_inner(spawn_args, resolve_elevation_policy(false))
    }

    /// Connect to a running daemon; if we must auto-start, request a
    /// UAC prompt on Windows when the current process is not elevated.
    ///
    /// Used by `uffs --daemon start --elevate`.  All other entry points
    /// default to `ElevationPolicy::RequireExistingElevation`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::connect_with_args`], minus
    /// `DaemonNeedsElevation` (converted into a UAC prompt).
    pub fn connect_with_elevation(spawn_args: &[std::ffi::OsString]) -> Result<Self, ClientError> {
        Self::connect_with_args_inner(spawn_args, ElevationPolicy::AllowUacPrompt)
    }

    /// Shared body for [`Self::connect_with_args`] and
    /// [`Self::connect_with_elevation`].
    ///
    /// Takes an explicit [`ElevationPolicy`] so each public entry
    /// point can decide whether a missing elevated context is a
    /// hard error (the default) or a prompt request.
    fn connect_with_args_inner(
        spawn_args: &[std::ffi::OsString],
        policy: ElevationPolicy,
    ) -> Result<Self, ClientError> {
        let sock = socket_path();

        // Try connecting first — fast path if daemon is already running.
        // Run the strict identity check before handing the client back:
        // a successful TCP/pipe connect only proves *something* was
        // listening on the endpoint, not that it was the daemon we
        // trust.  `verify_daemon_after_connect_strict` closes that
        // window (commit B), and `deep_health_check` then proves the
        // daemon is actually responsive to RPCs (commit C).
        if let Ok(mut client) = Self::platform_connect() {
            crate::daemon_ctl::verify_daemon_after_connect_strict()?;
            if crate::daemon_ctl::deep_health_check_enabled() {
                client.deep_health_check()?;
            }
            return Ok(client);
        }

        // On non-Windows, the daemon needs an explicit data source.
        // Fail fast with a helpful message instead of spawning a daemon
        // that will immediately exit (then spinning through 20 retries).
        #[cfg(not(windows))]
        {
            let has_data_source = spawn_args.iter().any(|arg| {
                // A data-source flag is always ASCII; a non-UTF-8 `OsString`
                // (e.g. a raw path arg) is by definition not one of these
                // flags, so `to_str()` → `None` → `false` is correct.
                arg.to_str().is_some_and(|flag| {
                    flag == "--data-dir"
                        || flag == "--mft-file"
                        || flag.starts_with("--data-dir=")
                        || flag.starts_with("--mft-file=")
                })
            });
            if !has_data_source {
                return Err(ClientError::ConnectionFailed(
                    "No daemon is running and no data source was provided.\n\
                     On macOS/Linux, start the daemon first:\n\n  \
                     uffs --daemon start --data-dir ~/uffs_data\n\n\
                     Or pass --data-dir inline:\n\n  \
                     uffs \"notepad.exe\" --data-dir ~/uffs_data"
                        .to_owned(),
                ));
            }
        }

        // Auto-start the daemon with the requested elevation policy.
        // Keep the returned child handle alive for the retry loop so we
        // can detect unexpected early exit (panic, clap parse error,
        // validate_data_sources bail, etc.) instead of spinning through
        // 20 retries with no diagnostic signal — see the `LOG/Output`
        // silent-failure scenario.
        let mut child_handle = auto_start_daemon(spawn_args, policy)?;

        // Retry with backoff.
        let mut delay_ms = 50_u64;
        let max_attempts = 20_usize;
        for attempt in 1..=max_attempts {
            std::thread::sleep(core::time::Duration::from_millis(delay_ms));

            if let Ok(mut client) = Self::platform_connect() {
                // Apply the strict identity check here too — even a
                // daemon we just spawned ourselves could have lost the
                // race to a rogue process that bound the endpoint
                // milliseconds earlier.  Commit B.  Commit C then
                // probes the IndexService with a cheap `drives` RPC so
                // a Ready-but-wedged daemon surfaces immediately.
                crate::daemon_ctl::verify_daemon_after_connect_strict()?;
                if crate::daemon_ctl::deep_health_check_enabled() {
                    client.deep_health_check()?;
                }
                return Ok(client);
            }

            // ── Early-exit detection ─────────────────────────────────
            // If we spawned the daemon this call, poll the child handle
            // once per attempt.  An exited child means uffsd failed to
            // reach IPC bind — surface the exit code immediately instead
            // of waiting out the full 31 s retry window with a generic
            // "could not connect after 20 attempts" error.
            if let Some(handle) = child_handle.as_mut() {
                match handle.try_wait() {
                    Ok(Some(code)) => {
                        return Err(ClientError::DaemonStartFailed(format!(
                            "Daemon (pid {pid}) exited with code {code} after {attempt} connect \
                             attempt(s) — the daemon died before it could bind IPC.  \
                             Check the daemon log (see --log-file or UFFS_LOG_DIR); common \
                             causes: code 2 = clap argv rejected (bad flag combo), code 101 = \
                             Rust panic, code 0 = graceful exit (validate_data_sources bailed).",
                            pid = handle.pid(),
                        )));
                    }
                    Ok(None) => {
                        // Still alive — keep retrying the connect.
                    }
                    Err(poll_err) => {
                        tracing::debug!(
                            error = %poll_err,
                            "connect_with_args: child liveness poll failed, ignoring"
                        );
                    }
                }
            }

            delay_ms = (delay_ms * 2).min(2000);

            // Logged, never printed: a waiting command writes nothing but
            // its result, so scripts reading stdout or stderr are not handed
            // progress lines they have no way to tell apart from output.
            if attempt <= 3 || attempt == max_attempts {
                let sock_state = if sock.exists() { "exists" } else { "missing" };
                tracing::info!(
                    attempt,
                    max_attempts,
                    socket = sock_state,
                    "auto-start: daemon not reachable yet, retrying connect"
                );
            }
        }

        Err(ClientError::ConnectionFailed(format!(
            "Could not connect to daemon after {max_attempts} attempts"
        )))
    }

    /// Send a JSON-RPC request and read the response (blocking).
    ///
    /// # Deadline
    ///
    /// On Windows, arms the `WindowsDeadlineGuard` (in
    /// `crate::windows_deadline`) before any I/O and disarms it on return
    /// (success or error). Using a `DisarmOnDrop` guard makes the disarm
    /// robust against early-return paths, including `?` bubbling from the
    /// read loop.
    ///
    /// On Unix, the deadline is enforced by `SO_RCVTIMEO` /
    /// `SO_SNDTIMEO` set at connect time and needs no per-call logic.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub(crate) fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ClientError> {
        // Arm the Windows deadline guard, if present.  `_disarmer`
        // guarantees disarm on every exit path, including `?`.
        #[cfg(windows)]
        let deadline_guard = self.deadline_guard.as_ref();
        #[cfg(windows)]
        let _disarmer = deadline_guard.map(|guard| {
            guard.arm();
            DisarmOnDrop { guard }
        });
        // Classify a failed read/write: the deadline firing is a
        // `Timeout`, everything else is a transport `Io` error.
        #[cfg(windows)]
        let transport_error =
            move |err: io::Error| crate::connect_sync_errors::transport_error(deadline_guard, &err);
        #[cfg(not(windows))]
        let transport_error = |err: io::Error| crate::connect_sync_errors::classify_io_error(&err);

        let id = self.next_id;
        self.next_id += 1;

        // Compose JSON-RPC request.
        let req = params.map_or_else(
            || format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#),
            |par| format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{par}}}"#),
        );

        self.writer
            .write_all(req.as_bytes())
            .map_err(transport_error)?;
        self.writer.write_all(b"\n").map_err(transport_error)?;
        self.writer.flush().map_err(transport_error)?;

        // Read lines until we get a response with matching id.
        // Skip notifications (no `id` field).
        loop {
            let mut raw_line = String::new();
            let bytes_read = self
                .reader
                .read_line(&mut raw_line)
                .map_err(transport_error)?;
            if bytes_read == 0 {
                return Err(ClientError::ConnectionClosed);
            }
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let val: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|err| ClientError::Protocol(err.to_string()))?;

            // Notification (no `id`) — skip.
            if val.get("id").is_none() {
                continue;
            }

            // Error response.
            if let Some(err_obj) = val.get("error") {
                let code = err_obj
                    .get("code")
                    .and_then(serde_json::Value::as_i64)
                    .map_or(-1_i32, |code_i64| i32::try_from(code_i64).unwrap_or(-1_i32));
                let message = err_obj
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned();
                return Err(ClientError::DaemonError { code, message });
            }

            // Success — return the `result` field.
            return val
                .get("result")
                .cloned()
                .ok_or_else(|| ClientError::Protocol("missing 'result' field".to_owned()));
        }
    }

    // ── Public API ──────────────────────────────────────────────────

    /// Search for files.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn search(
        &mut self,
        params: &crate::protocol::SearchParams,
    ) -> Result<crate::protocol::response::SearchResponse, ClientError> {
        let params_json =
            serde_json::to_value(params).map_err(|err| ClientError::Protocol(err.to_string()))?;
        let result = self.send_request("search", Some(params_json))?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Search using raw CLI arguments — the daemon parses them.
    ///
    /// This lets the CLI forward its argv directly to the daemon without
    /// locally parsing every flag into typed fields.  The daemon handles
    /// all sugar expansion (`--begins-with`, `--between`, etc.) and builds
    /// `SearchParams` internally.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn search_cli(
        &mut self,
        args: &[String],
    ) -> Result<crate::protocol::response::SearchResponse, ClientError> {
        let params = serde_json::json!({ "args": args });
        let result = self.send_request("search_cli", Some(params))?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Like [`search_cli`](Self::search_cli) but returns raw JSON instead of
    /// a typed `SearchResponse`.
    ///
    /// This avoids pulling `serde` derive codegen into callers that only
    /// need to read a few fields from the response (e.g. the thin CLI).
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn search_cli_raw(&mut self, args: &[String]) -> Result<serde_json::Value, ClientError> {
        let params = serde_json::json!({ "args": args });
        self.send_request("search_cli", Some(params))
    }

    /// Get daemon status.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn status(&mut self) -> Result<crate::protocol::response::StatusResponse, ClientError> {
        let result = self.send_request("status", None)?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Get daemon status as raw JSON.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn status_raw(&mut self) -> Result<serde_json::Value, ClientError> {
        self.send_request("status", None)
    }

    /// List loaded drives.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn drives(&mut self) -> Result<crate::protocol::response::DrivesResponse, ClientError> {
        let result = self.send_request("drives", None)?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Get performance stats.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn stats(&mut self) -> Result<crate::protocol::response::StatsResponse, ClientError> {
        let result = self.send_request("stats", None)?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Get file info by path.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn info(
        &mut self,
        path: &str,
    ) -> Result<crate::protocol::response::InfoResponse, ClientError> {
        let params = serde_json::json!({ "path": path });
        let result = self.send_request("info", Some(params))?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Request graceful daemon shutdown.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O or timeout failure.
    pub fn shutdown(&mut self) -> Result<(), ClientError> {
        let nonce = std::fs::read_to_string(pid_file_path())
            .ok()
            .and_then(|content| content.lines().nth(3).map(String::from))
            .unwrap_or_default();
        let params = serde_json::json!({ "nonce": nonce });
        let _result = self.send_request("shutdown", Some(params))?;
        Ok(())
    }

    /// Wait for the daemon to become ready (status == Ready).
    ///
    /// Short-circuits when `cached_status` is `Ready` (populated by
    /// `deep_health_check` at connect time), saving an RPC
    /// round-trip on the hot CLI path.  Falls back to the exponential
    /// poll loop when the cache is `None`, `Loading`, or `Refreshing`.
    ///
    /// `timeout` is an **idle** budget, not a hard wall-clock cutoff: every
    /// time the daemon reports forward progress (`DaemonStatus::Loading`'s
    /// `drives_loaded` advancing), the deadline resets to `now + timeout`.
    /// A big multi-drive load under heavy system load (contended disk I/O,
    /// CPU pressure from other processes) that keeps visibly making
    /// progress is never killed by an arbitrary fixed cutoff — only a
    /// daemon that stops progressing for a full `timeout` window is. A
    /// hard outer ceiling (`5 * timeout`) still bounds the total wait so a
    /// daemon stuck oscillating on the same drive count can't hang the
    /// caller forever.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Timeout` if not ready within the idle budget
    /// (or the hard ceiling, whichever is hit first).
    pub fn await_ready(&mut self, timeout: core::time::Duration) -> Result<(), ClientError> {
        // Run 10 Part B short-circuit: skip the RPC on cached `Ready`.
        if matches!(self.cached_status, Some(DaemonStatus::Ready)) {
            return Ok(());
        }

        let start = std::time::Instant::now();
        // Even under continuous progress, don't wait forever — 5x the
        // caller's own idle budget scales with however patient it already
        // asked to be, without introducing a load-independent magic number.
        let hard_ceiling = start + timeout.saturating_mul(5);
        let mut deadline = start + timeout;
        let mut poll_interval = core::time::Duration::from_millis(100);
        let mut last_drives_loaded: Option<usize> = None;

        while std::time::Instant::now() < deadline {
            match self.status() {
                Ok(resp) if resp.status == DaemonStatus::Ready => {
                    // Refresh cache so follow-up calls short-circuit.
                    self.cached_status = Some(DaemonStatus::Ready);
                    return Ok(());
                }
                Ok(resp) => {
                    if let DaemonStatus::Loading { drives_loaded, .. } = resp.status
                        && last_drives_loaded != Some(drives_loaded)
                    {
                        last_drives_loaded = Some(drives_loaded);
                        deadline = (std::time::Instant::now() + timeout).min(hard_ceiling);
                    }
                    // `Refreshing`, or `Loading` with an unchanged
                    // `drives_loaded`, falls through to the sleep below
                    // without touching the deadline — no progress, no
                    // extension.
                }
                // Any error outcome (I/O error, connection closed, RPC
                // timeout, transient protocol error) keeps polling without
                // extending the deadline. Mirrors the async sibling at
                // `connect.rs::await_ready` (`PollOutcome::OtherError`).
                // Pinned by the
                // `await_ready_retries_on_protocol_error_until_deadline`
                // regression test — see its docstring for the
                // 2026-05-07 Phase 7 soak background.
                Err(_) => {}
            }
            std::thread::sleep(poll_interval);
            poll_interval = (poll_interval * 2).min(core::time::Duration::from_secs(2));
        }
        Err(ClientError::Timeout)
    }

    /// Load a drive (MFT files).
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn load_drive(
        &mut self,
        mft_files: &[String],
        no_cache: bool,
    ) -> Result<crate::protocol::response::LoadDriveResponse, ClientError> {
        let params = serde_json::json!({
            "mft_files": mft_files,
            "no_cache": no_cache,
        });
        let result = self.send_request("load_drive", Some(params))?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Hot-load one or more drives by letter into the running daemon.
    ///
    /// On Windows this reads the live NTFS MFT; on non-Windows it discovers
    /// offline MFT files from the daemon's `data_dir`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn load_drive_letters(
        &mut self,
        drives: &[uffs_mft::platform::DriveLetter],
        no_cache: bool,
    ) -> Result<crate::protocol::response::LoadDriveResponse, ClientError> {
        let params = serde_json::json!({
            "drives": drives,
            "no_cache": no_cache,
        });
        let result = self.send_request("load_drive", Some(params))?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }

    /// Refresh drives.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure.
    pub fn refresh(
        &mut self,
        drives: &[uffs_mft::platform::DriveLetter],
    ) -> Result<(), ClientError> {
        let params = serde_json::json!({"drives": drives});
        let _result = self.send_request("refresh", Some(params))?;
        Ok(())
    }

    /// Send a keepalive ping.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O or timeout failure.
    pub fn keepalive(&mut self) -> Result<(), ClientError> {
        let _result = self.send_request("keepalive", None)?;
        Ok(())
    }

    /// Commit C — **deep health check**: round-trip a `status` RPC
    /// right after connect to prove the daemon's request/response
    /// loop is responsive, and cache the returned [`DaemonStatus`]
    /// so [`Self::await_ready`] can short-circuit on the hot path.
    ///
    /// Run 10 Part B (2026-04-19) consolidated the prior `drives`
    /// liveness probe + `await_ready` readiness probe into this
    /// single `status` call, saving one full RPC round-trip per CLI
    /// invocation (~5–10 ms on Windows named pipes).  Skippable via
    /// `UFFS_CLIENT_SKIP_HEALTH_CHECK=1` (see
    /// [`crate::daemon_ctl::deep_health_check_enabled`]).  Cost: ~200–600 µs
    /// local IPC.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::ConnectionFailed`] wrapping the
    /// underlying transport/protocol error.
    pub(crate) fn deep_health_check(&mut self) -> Result<(), ClientError> {
        match self.status() {
            Ok(resp) => {
                self.cached_status = Some(resp.status);
                Ok(())
            }
            Err(probe_err) => {
                // Torn probe — clear cache so await_ready can't lie.
                self.cached_status = None;
                Err(ClientError::ConnectionFailed(format!(
                    "Deep health check failed: the daemon accepted the connection but did \
                     not respond correctly to a probe `status` RPC ({probe_err}). The \
                     daemon may be wedged (deadlocked worker, stuck kernel I/O); consider \
                     `uffs --daemon kill` and restart.  Set UFFS_CLIENT_SKIP_HEALTH_CHECK=1 \
                     to bypass this probe."
                )))
            }
        }
    }
}

/// RAII helper: disarm a [`crate::windows_deadline::WindowsDeadlineGuard`]
/// when this value is dropped.
///
/// Wrapping the arm/disarm pair in an RAII sentinel rather than
/// manual disarm-before-return is crucial because `send_request`
/// uses `?` to propagate transport and protocol errors — with manual
/// disarm, every `?` would either have to be replaced with an
/// explicit match (ugly) or would leak an armed deadline that then
/// fires on the *next* RPC (subtle bug).  Dropping takes care of
/// both the success and error paths uniformly.
#[cfg(windows)]
struct DisarmOnDrop<'guard> {
    /// The deadline guard to disarm when this sentinel is dropped.
    guard: &'guard crate::windows_deadline::WindowsDeadlineGuard,
}

#[cfg(windows)]
impl Drop for DisarmOnDrop<'_> {
    fn drop(&mut self) {
        self.guard.disarm();
    }
}
