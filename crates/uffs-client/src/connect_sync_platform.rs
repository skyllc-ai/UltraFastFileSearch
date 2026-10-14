// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Platform-specific `platform_connect` implementations for
//! [`crate::connect_sync::UffsClientSync`], plus the shared
//! `rpc_deadline` helper and its env regression tests.
//!
//! Extracted from `connect_sync.rs` for file-size policy compliance.
//! All items live on `UffsClientSync` via split `impl` blocks — no
//! public surface moves.

use std::io::{BufReader, Read, Write};

use crate::connect_sync::UffsClientSync;
#[cfg(unix)]
use crate::daemon_ctl::socket_path;
use crate::error::ClientError;

/// Default per-RPC deadline applied at connect time: 10 minutes.
///
/// Every blocking read / write on the IPC transport fails with
/// `ClientError::Timeout` after this duration.  A long wait is normal,
/// not an error: a search that touches a parked drive waits while the
/// daemon re-reads that drive's MFT, which takes minutes on a large HDD
/// volume, and a script that just wants the result must get it.  The
/// deadline exists only so a daemon that is truly stuck (hung kernel
/// I/O, deadlocked worker) eventually surfaces as an error instead of a
/// command that never returns.  It was 60 s until v0.6.46, which failed
/// ordinary parked-drive searches.
///
/// Overridable with the `UFFS_CLIENT_TIMEOUT_SECS` environment variable
/// (a shorter limit for callers that prefer an early error).
const DEFAULT_RPC_DEADLINE_SECS: u64 = 600;

/// Resolve the per-RPC deadline from env + default.
///
/// * `UFFS_CLIENT_TIMEOUT_SECS=0` → disables the timeout (useful when attaching
///   a debugger).
/// * `UFFS_CLIENT_TIMEOUT_SECS=N` → `N`-second deadline.
/// * unset or unparseable → `DEFAULT_RPC_DEADLINE_SECS` (600 s).
#[must_use]
pub fn rpc_deadline() -> Option<core::time::Duration> {
    let secs = std::env::var("UFFS_CLIENT_TIMEOUT_SECS")
        .ok()
        .and_then(|val| val.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RPC_DEADLINE_SECS);
    if secs == 0 {
        None
    } else {
        Some(core::time::Duration::from_secs(secs))
    }
}

#[cfg(unix)]
impl UffsClientSync {
    /// Connect via Unix domain socket (macOS/Linux).
    ///
    /// Applies the default per-RPC deadline (see [`rpc_deadline`]) via
    /// `SO_RCVTIMEO` / `SO_SNDTIMEO` — the kernel then enforces the
    /// deadline on every subsequent blocking read/write with zero
    /// additional cost in the client.
    pub(crate) fn platform_connect() -> Result<Self, ClientError> {
        Self::platform_connect_at(&socket_path())
    }

    /// Connect via Unix domain socket at an explicit path, bypassing the
    /// well-known per-user [`socket_path`] resolution — used by
    /// [`UffsClientSync::connect_at`] to reach an ephemeral daemon
    /// instance. Shares every other behavior (deadline, buffering) with
    /// [`Self::platform_connect`].
    pub(crate) fn platform_connect_at(sock_path: &std::path::Path) -> Result<Self, ClientError> {
        Self::platform_connect_at_with_deadline(sock_path, rpc_deadline())
    }

    /// [`Self::platform_connect`] with an explicit per-RPC `deadline`
    /// instead of the env-configured one (`None` disables it).
    pub(crate) fn platform_connect_with_deadline(
        deadline: Option<core::time::Duration>,
    ) -> Result<Self, ClientError> {
        Self::platform_connect_at_with_deadline(&socket_path(), deadline)
    }

    /// Shared body of the Unix connect paths.
    fn platform_connect_at_with_deadline(
        sock_path: &std::path::Path,
        deadline: Option<core::time::Duration>,
    ) -> Result<Self, ClientError> {
        let stream = std::os::unix::net::UnixStream::connect(sock_path)
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;

        // Set both read and write deadlines.  A hung daemon could stall
        // either direction — blocked kernel I/O on response, or a full
        // pipe buffer on request — and we want both cases to surface as
        // a `ClientError::Io(timed out)` instead of the CLI hanging.
        stream
            .set_read_timeout(deadline)
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;
        stream
            .set_write_timeout(deadline)
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;

        let writer = stream
            .try_clone()
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;

        Ok(Self::from_parts(
            BufReader::new(Box::new(stream) as Box<dyn Read + Send>),
            Box::new(writer) as Box<dyn Write + Send>,
        ))
    }
}

#[cfg(windows)]
impl UffsClientSync {
    /// Construct a fresh [`crate::windows_deadline::WindowsDeadlineGuard`]
    /// for this client with `deadline`, or return `None` if the deadline
    /// is disabled (`None`, e.g. `UFFS_CLIENT_TIMEOUT_SECS=0`).
    ///
    /// Called from `platform_connect` right after the pipe handle
    /// is opened.  The guard captures the *current* thread as its
    /// cancellation target, so `platform_connect` must be called on
    /// the thread that will subsequently own the client — which is
    /// the contract today (CLI, MCP stdio loop both call `connect`
    /// then immediately use the result on the same thread).
    fn build_deadline_guard(
        deadline: Option<core::time::Duration>,
    ) -> Option<crate::windows_deadline::WindowsDeadlineGuard> {
        let duration = deadline?;
        match crate::windows_deadline::WindowsDeadlineGuard::new(duration) {
            Ok(guard) => Some(guard),
            Err(err) => {
                // The guard is a best-effort safety net — if we can
                // not spawn the watchdog (e.g. process near thread
                // limit), fall back to un-guarded I/O rather than
                // failing the connect outright.  The upstream
                // robustness defenses (DaemonChildHandle early-exit
                // detection, strict identity verification, deep
                // health check) still apply.
                tracing::warn!(
                    %err,
                    "Failed to spawn Windows deadline watchdog — proceeding without per-RPC timeout",
                );
                None
            }
        }
    }

    /// Connect via Windows named pipe.
    ///
    /// This is the CLI hot path — opens the pipe with blocking
    /// `std::fs::OpenOptions`, avoiding the `ws2_32.dll` import that
    /// `AF_UNIX` pulled in (~54 ms per launch).
    ///
    /// Handles `ERROR_PIPE_BUSY` (231) by sleep-retrying a few times:
    /// the daemon creates the next server instance immediately after
    /// accept, but there is a tiny window where all instances are
    /// connected and the next one hasn't been spun up yet.
    ///
    /// # Per-RPC deadline
    ///
    /// Unlike the Unix path (which uses `SO_RCVTIMEO` / `SO_SNDTIMEO`
    /// for kernel-enforced read/write deadlines at zero cost), Windows
    /// named pipes opened via `OpenOptions` do **not** expose a
    /// blocking read timeout.  The deadline is therefore enforced by a
    /// dedicated watchdog thread (see [`crate::windows_deadline`])
    /// that cancels synchronous I/O on the owning thread when an
    /// RPC exceeds its deadline.
    ///
    /// The watchdog is constructed once per client in
    /// `build_deadline_guard` and shared across all RPCs on this
    /// connection.  Per-RPC overhead is two atomic stores (arm +
    /// disarm, nanoseconds); per-client overhead is one long-lived
    /// thread with a 50 ms poll period.
    pub(crate) fn platform_connect() -> Result<Self, ClientError> {
        let name = crate::daemon_ctl::pipe_name()
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;
        Self::platform_connect_at(name.as_str())
    }

    /// Connect via named pipe at an explicit path, bypassing the
    /// well-known per-user [`crate::daemon_ctl::pipe_name`] resolution —
    /// used by [`UffsClientSync::connect_at`] to reach an ephemeral
    /// daemon instance. Shares every other behavior (busy-retry,
    /// deadline guard) with [`Self::platform_connect`].
    pub(crate) fn platform_connect_at(pipe_path: &str) -> Result<Self, ClientError> {
        Self::platform_connect_at_with_deadline(pipe_path, rpc_deadline())
    }

    /// [`Self::platform_connect`] with an explicit per-RPC `deadline`
    /// instead of the env-configured one (`None` disables it).
    pub(crate) fn platform_connect_with_deadline(
        deadline: Option<core::time::Duration>,
    ) -> Result<Self, ClientError> {
        let name = crate::daemon_ctl::pipe_name()
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;
        Self::platform_connect_at_with_deadline(name.as_str(), deadline)
    }

    /// Shared body of the Windows connect paths.
    fn platform_connect_at_with_deadline(
        pipe_path: &str,
        deadline: Option<core::time::Duration>,
    ) -> Result<Self, ClientError> {
        use std::fs::{File, OpenOptions};

        /// `ERROR_PIPE_BUSY` — transient, retry with backoff.
        const ERROR_PIPE_BUSY: i32 = 231;

        let pipe: File = {
            let mut last_err: Option<std::io::Error> = None;
            let mut pipe_file: Option<File> = None;
            for attempt in 0..5_u32 {
                match OpenOptions::new().read(true).write(true).open(pipe_path) {
                    Ok(file) => {
                        pipe_file = Some(file);
                        break;
                    }
                    Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                        // Pipe saturated for a moment — back off briefly.
                        std::thread::sleep(core::time::Duration::from_millis(u64::from(
                            10_u32 << attempt,
                        )));
                        last_err = Some(err);
                    }
                    Err(err) => {
                        return Err(ClientError::ConnectionFailed(err.to_string()));
                    }
                }
            }
            pipe_file.ok_or_else(|| {
                ClientError::ConnectionFailed(last_err.map_or_else(
                    || "pipe busy after retries".to_owned(),
                    |err| format!("pipe busy after retries: {err}"),
                ))
            })?
        };

        // A second handle to the same pipe instance for the writer side.
        // Named-pipe handles are duplicable via `DuplicateHandle` (which
        // is what `File::try_clone` does internally on Windows).
        let writer_handle = pipe
            .try_clone()
            .map_err(|err| ClientError::ConnectionFailed(err.to_string()))?;

        // Build the deadline guard *before* constructing `Self` so
        // the watchdog captures the caller's thread (not some future
        // thread the client might be sent to).  A failure to spawn
        // the watchdog is non-fatal — see `build_deadline_guard`.
        let deadline_guard = Self::build_deadline_guard(deadline);

        Ok(Self::from_parts_with_deadline_guard(
            BufReader::new(Box::new(pipe) as Box<dyn Read + Send>),
            Box::new(writer_handle) as Box<dyn Write + Send>,
            deadline_guard,
        ))
    }
}

#[cfg(test)]
mod rpc_deadline_tests {
    use super::{DEFAULT_RPC_DEADLINE_SECS, rpc_deadline};

    /// Serialise env-mutating tests — `std::env::set_var` is process-
    /// global and tests run in parallel by default, so two tests can
    /// otherwise race on the same variable.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_timeout_env<F: FnOnce()>(value: Option<&str>, body: F) {
        let guard = ENV_LOCK.lock().expect("env mutex poisoned");
        let previous = std::env::var("UFFS_CLIENT_TIMEOUT_SECS").ok();
        if let Some(val) = value {
            // SAFETY: the mutex above serialises all test-time env writes.
            #[expect(unsafe_code, reason = "std::env::set_var is unsafe in Rust 2024")]
            unsafe {
                std::env::set_var("UFFS_CLIENT_TIMEOUT_SECS", val);
            }
        } else {
            // SAFETY: same as above.
            #[expect(unsafe_code, reason = "std::env::remove_var is unsafe in Rust 2024")]
            unsafe {
                std::env::remove_var("UFFS_CLIENT_TIMEOUT_SECS");
            }
        }
        body();
        match previous {
            Some(prev) => {
                // SAFETY: same as above — restore original value under the same
                // lock.
                #[expect(unsafe_code, reason = "std::env::set_var is unsafe in Rust 2024")]
                unsafe {
                    std::env::set_var("UFFS_CLIENT_TIMEOUT_SECS", prev);
                }
            }
            None => {
                // SAFETY: same as above.
                #[expect(unsafe_code, reason = "std::env::remove_var is unsafe in Rust 2024")]
                unsafe {
                    std::env::remove_var("UFFS_CLIENT_TIMEOUT_SECS");
                }
            }
        }
        drop(guard);
    }

    /// Unset env → default deadline applies.  Locks the 600 s default
    /// in place; changing the constant should require a deliberate
    /// test update so nobody silently shrinks or removes it.
    #[test]
    fn unset_env_returns_default() {
        with_timeout_env(None, || {
            assert_eq!(
                rpc_deadline(),
                Some(core::time::Duration::from_secs(DEFAULT_RPC_DEADLINE_SECS)),
            );
        });
    }

    /// Explicit `0` disables the deadline — we want `None`, not
    /// `Some(Duration::ZERO)`, because `set_read_timeout(Some(0))` is
    /// an error on Unix ("ZERO is not a valid argument").  This
    /// contract is what makes `UFFS_CLIENT_TIMEOUT_SECS=0` safe as a
    /// debugger-attach escape hatch.
    #[test]
    fn zero_disables_deadline() {
        with_timeout_env(Some("0"), || {
            assert_eq!(rpc_deadline(), None);
        });
    }

    /// A positive integer overrides the default.
    #[test]
    fn positive_integer_overrides_default() {
        with_timeout_env(Some("5"), || {
            assert_eq!(rpc_deadline(), Some(core::time::Duration::from_secs(5)));
        });
        // 3600 s == 1 h; use `from_hours` for readability per
        // clippy::duration_suboptimal_units.
        with_timeout_env(Some("3600"), || {
            assert_eq!(rpc_deadline(), Some(core::time::Duration::from_hours(1)));
        });
    }

    /// Unparseable / malformed values fall back to the default — a
    /// typo in `UFFS_CLIENT_TIMEOUT_SECS=30s` (trailing letter) or a
    /// negative value must not silently remove the deadline.
    #[test]
    fn malformed_value_falls_back_to_default() {
        for token in ["not-a-number", "-5", "3.14", "30s", " "] {
            with_timeout_env(Some(token), || {
                assert_eq!(
                    rpc_deadline(),
                    Some(core::time::Duration::from_secs(DEFAULT_RPC_DEADLINE_SECS)),
                    "token {token:?} should fall back to the default",
                );
            });
        }
    }
}
