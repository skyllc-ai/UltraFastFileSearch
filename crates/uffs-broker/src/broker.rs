// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Windows Access Broker implementation.
//!
//! Runs as a Windows Service (or foreground process for debugging).
//! Listens on a named pipe, verifies client identity, and provides
//! read-only volume handles for MFT access.
//!
//! # Protocol
//!
//! See [`uffs_broker_protocol`] for the authoritative wire-format
//! definition (1-byte drive-letter request, 9-byte status + LE-u64
//! handle response).  Both this binary and
//! `uffs_daemon::broker_client` consume the same crate — there is no
//! second copy of the protocol constants or byte layout anywhere in
//! the tree.
//!
//! The broker opens `\\.\X:` with `FILE_READ_DATA` + `SeBackupPrivilege`,
//! then `DuplicateHandle`s it into the client process with read-only access.

#[cfg(windows)]
use uffs_broker_protocol::{HandleRequest, HandleResponse, PIPE_NAME, RESPONSE_WIRE_LEN};

// Windows Service registration helpers, split out to keep this file under the
// 800-LOC ceiling (see `broker/service.rs`).
#[path = "broker/service.rs"]
mod service;
#[cfg(windows)]
use service::{install_service, repair_service, uninstall_service};

// SCM failure-recovery configuration (`sc failure` / `failureflag`), used by
// `--install` and `--repair`. See `broker/recovery.rs`.
#[path = "broker/recovery.rs"]
mod recovery;

// Operator-facing `--status` / `--start` / `--stop` via native SCM
// (`uffs-winsvc`), split out to keep this file under the 800-LOC ceiling.
// Windows-only: `uffs-winsvc` is a `cfg(windows)` dependency of the broker.
#[cfg(windows)]
#[path = "broker/control.rs"]
mod control;

// Client process-handle acquisition + identity verification (WI-8.1), split
// out to keep this file under the 800-LOC ceiling. See
// `broker/process_handle.rs`.
#[path = "broker/process_handle.rs"]
mod process_handle;
#[cfg(windows)]
use process_handle::{OwnedProcessHandle, query_process_image_name, verify_client_handle};
// S5.2 Authenticode verification (WinVerifyTrust + per-image cache). The single
// implementation now lives in `uffs_security::authenticode`, shared with the
// self-updater (DRY) instead of a broker-local copy.
#[cfg(windows)]
use uffs_security::authenticode::verify_authenticode;

// `Send`-safe RAII handle wrapper (SBB-2) — lets a connected pipe instance move
// into a per-connection worker thread (FU-5). See `broker/owned_handle.rs`.
#[path = "broker/owned_handle.rs"]
mod owned_handle;
#[cfg(windows)]
use owned_handle::OwnedHandle;

// Named-pipe creation + SDDL security descriptor, split out to keep this file
// under the 800-LOC ceiling. See `broker/pipe.rs`.
#[path = "broker/pipe.rs"]
mod pipe;
#[cfg(windows)]
use pipe::{create_broker_pipe, wait_for_client};

// Snapshot Manager: the Coordinator-facing VSS-lease API (a separate pipe
// from the daemon's MFT-handle channel above), spawned from
// `serve_pipe_requests` so it runs under both `--run` (foreground) and the
// SCM-dispatched service path. See `broker/snapshot_manager/mod.rs`.
#[path = "broker/snapshot_manager/mod.rs"]
mod snapshot_manager;

/// Per-drive rate-limit state (`drive → last grant time`), shared across the
/// FU-5 per-connection worker threads behind a `Mutex`.
#[cfg(windows)]
type RateLimit = std::sync::Mutex<std::collections::HashMap<char, std::time::Instant>>;

/// Maximum concurrent named-pipe instances the broker serves (FU-5).
///
/// One instance is always listening in the accept loop; the rest can be
/// in-flight on worker threads.  Bounded (not `PIPE_UNLIMITED_INSTANCES`) so a
/// flood of clients can't exhaust threads/handles — excess clients simply wait
/// for a free instance.
#[cfg(windows)]
const MAX_PIPE_INSTANCES: u32 = 16;

/// Run the broker (called from main).
///
/// Scope is `pub(crate)` because `broker` is a private module of the
/// binary crate — only `main.rs` invokes this.
///
/// # Errors
///
/// Returns an error if service installation/uninstallation fails, or if the
/// foreground pipe-serving loop encounters an unrecoverable error.
#[cfg(windows)]
pub(crate) fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|arg| arg == "--install") {
        return install_service();
    }
    if args.iter().any(|arg| arg == "--uninstall") {
        return uninstall_service();
    }
    if args.iter().any(|arg| arg == "--repair") {
        return repair_service();
    }
    if args.iter().any(|arg| arg == "--status") {
        control::status();
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--start") {
        return control::start();
    }
    if args.iter().any(|arg| arg == "--stop") {
        return control::stop();
    }
    if args.iter().any(|arg| arg == "--run") {
        return run_foreground();
    }
    if let Some(test_dir) = self_test_vss_dir(&args) {
        return self_test_vss(&test_dir);
    }

    // No recognised flag: this is how the Service Control Manager launches the
    // service at boot.  Hand control to the dispatcher; when run interactively
    // (no SCM) it falls back to printing usage (FU-1).
    service::run_as_service()
}

// Usage banner + `--self-test-vss` argument lookup, split out to keep this
// file under the 800-LOC ceiling. See `broker/cli.rs`.
#[path = "broker/cli.rs"]
mod cli;
#[cfg(windows)]
use cli::{print_usage, self_test_vss_dir};

/// Run `snapshot_manager::self_test_round_trip` standalone (no service,
/// no pipe server) and print a PASS/FAIL result — a manual, elevated
/// smoke test proving the real VSS snapshot pipeline (native shim →
/// `uffs-vss-requestor` helper → Broker lease bookkeeping) works at
/// runtime on this machine.
///
/// # Errors
/// Returns an error (and prints `"FAIL: <cause>"`) if any stage of the
/// round trip fails.
#[cfg(windows)]
#[expect(
    clippy::print_stderr,
    reason = "one-shot CLI diagnostic invoked before any tracing subscriber exists"
)]
fn self_test_vss(test_dir: &std::path::Path) -> anyhow::Result<()> {
    init_tracing();
    warn_if_not_elevated();
    match snapshot_manager::self_test_round_trip(test_dir) {
        Ok(()) => {
            eprintln!(
                "PASS: VSS create/read/delete round trip succeeded ({})",
                test_dir.display()
            );
            Ok(())
        }
        Err(err) => {
            eprintln!("FAIL: {err:#}");
            Err(err)
        }
    }
}

/// Run the broker in foreground mode.
#[cfg(windows)]
fn run_foreground() -> anyhow::Result<()> {
    init_tracing();
    tracing::info!(
        pid = std::process::id(),
        version = %uffs_version::version_short!("uffs-broker"),
        "uffs-broker starting (foreground mode)"
    );
    warn_if_not_elevated();
    serve_pipe_requests()?;
    tracing::info!("uffs-broker stopped");
    Ok(())
}

/// Initialise the `tracing` subscriber if one isn't already installed.
///
/// Uses `try_init` so we don't panic when another subscriber (tests, embed)
/// is already in place.
#[cfg(windows)]
fn init_tracing() {
    let init_result = tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .try_init();
    drop(init_result);
}

/// Warn via `tracing` if the process is not running elevated.
#[cfg(windows)]
fn warn_if_not_elevated() {
    if !is_elevated() {
        tracing::warn!("Broker is NOT running elevated — volume access will fail");
        tracing::warn!("Run as Administrator or install as a Windows Service");
    }
}

/// Serve handle requests on the named pipe.
///
/// Hardened with S5 security controls:
/// - S5.1: Pipe created with Administrators-only default DACL (elevated
///   process)
/// - S5.2: Client exe path + Authenticode verification
/// - S5.3: Audit logging for every request
/// - S5.4: Rate limiting (1 request per drive per 10s)
/// - S5.5: Read-only handles only (enforced in `handle_pipe_request`)
#[cfg(windows)]
fn serve_pipe_requests() -> anyhow::Result<()> {
    use alloc::sync::Arc;
    use core::time::Duration;

    tracing::info!(
        pipe = PIPE_NAME,
        max_instances = MAX_PIPE_INSTANCES,
        "Listening for handle requests"
    );

    // Snapshot Manager (Coordinator-facing VSS-lease API) runs on its own
    // pipe, in its own accept loop, on a dedicated thread — independent of
    // the MFT-handle serve loop below. A failure here is logged, not fatal
    // to the daemon-facing handle service.
    std::thread::spawn(|| {
        if let Err(err) = snapshot_manager::run() {
            tracing::error!(error = %err, "Snapshot Manager stopped unexpectedly");
        }
    });

    // S5.4: rate-limit state, shared across per-connection workers.
    let rate_limit: Arc<RateLimit> =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    // Only the very first instance gets FILE_FLAG_FIRST_PIPE_INSTANCE; the rest
    // must omit it or CreateNamedPipeW fails ERROR_ACCESS_DENIED against it.
    let mut first_instance = true;

    loop {
        // FU-1: exit cleanly when the service control handler requests a stop.
        if service::stop_requested() {
            return Ok(());
        }

        // Create the next listening instance.  If all instances are busy this
        // fails transiently — back off briefly and retry rather than exit.
        let pipe = match create_broker_pipe(first_instance) {
            Ok(pipe) => {
                first_instance = false;
                pipe
            }
            Err(err) => {
                tracing::warn!(error = %err, "pipe instance unavailable; retrying shortly");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if let Err(err) = wait_for_client(pipe) {
            tracing::warn!(error = %err, "wait_for_client failed; dropping instance");
            disconnect_pipe(pipe);
            close_pipe(pipe);
            continue;
        }

        // FU-1: a connection arriving after a stop was requested is the control
        // handler's wake-up `CreateFile` — drop it and exit.
        if service::stop_requested() {
            disconnect_pipe(pipe);
            close_pipe(pipe);
            return Ok(());
        }

        // Hand the connected instance to a worker and immediately loop to
        // create the next listener, so there's always an instance accepting.
        let owned = OwnedHandle::new(pipe);
        let worker_rate_limit = Arc::clone(&rate_limit);
        std::thread::spawn(move || {
            handle_one_connection(owned.raw(), &worker_rate_limit);
            disconnect_pipe(owned.raw());
            // `owned` drops here → CloseHandle frees the pipe instance, even if
            // `handle_one_connection` panicked.
        });
    }
}

/// Handle a single connected pipe client: verify identity, run the
/// drive-handle request, and emit the appropriate S5.3 audit log.
///
/// Audit logging is performed for every terminal outcome (REJECTED /
/// GRANTED / FAILED) inside this function so the caller only needs to
/// disconnect and loop.
#[cfg(windows)]
fn handle_one_connection(pipe: windows::Win32::Foundation::HANDLE, rate_limit: &RateLimit) {
    let Some(pid) = get_pipe_client_pid(pipe) else {
        audit_log("REJECTED", 0, None, None, "could not determine client PID");
        tracing::warn!("Could not determine client PID — rejecting");
        return;
    };

    // WI-8.1: open the client process exactly ONCE here. The same handle is
    // used to read the image name, make the identity decision, AND serve as
    // the `DuplicateHandle` target — so a PID-reuse race cannot redirect the
    // grant to a different (unverified) process between verify and duplicate.
    let Some(client_process) = OwnedProcessHandle::open_client(pid) else {
        audit_log("REJECTED", pid, None, None, "could not open client process");
        tracing::warn!(pid, "Could not open client process — rejecting");
        return;
    };

    // Read the exe path from the SAME handle we just opened (not a fresh
    // PID→handle resolution). The identity decision uses the lossless path
    // inside `verify_client_handle`; this `String` form is for the audit log
    // only (display), so `to_string_lossy` is acceptable here.
    let exe_path: Option<String> =
        query_process_image_name(client_process.raw()).map(|os| os.to_string_lossy().into_owned());
    if !check_client_identity(&client_process, pid, exe_path.as_deref()) {
        return;
    }

    process_drive_request(pipe, &client_process, pid, exe_path.as_deref(), rate_limit);
}

/// Verify the client's identity (`verify_client_handle` name allowlist +
/// Authenticode signature check).  Emits the appropriate REJECTED audit log on
/// failure.
///
/// Takes the already-open `client_process` handle (WI-8.1) and verifies the
/// image name read from **that** handle — the same handle the grant will
/// duplicate into — rather than re-resolving the PID.
///
/// Returns `true` when identity is confirmed, `false` otherwise.
#[cfg(windows)]
fn check_client_identity(client_process: &OwnedProcessHandle, pid: u32, exe: Option<&str>) -> bool {
    if !verify_client_handle(client_process.raw()) {
        audit_log("REJECTED", pid, exe, None, "identity verification failed");
        tracing::warn!(pid, "Rejected broker client — not uffsd");
        return false;
    }

    if let Some(path) = exe
        && !verify_authenticode(path)
    {
        audit_log(
            "REJECTED",
            pid,
            exe,
            None,
            "Authenticode verification failed",
        );
        tracing::warn!(pid, exe = path, "Rejected: invalid Authenticode signature");
        return false;
    }

    tracing::debug!(pid, "Broker client verified");
    true
}

/// Run the rate-limited drive-handle request and emit the GRANTED /
/// FAILED audit log for the terminal outcome.
#[cfg(windows)]
fn process_drive_request(
    pipe: windows::Win32::Foundation::HANDLE,
    client_process: &OwnedProcessHandle,
    pid: u32,
    exe: Option<&str>,
    rate_limit: &RateLimit,
) {
    match handle_pipe_request_with_rate_limit(pipe, client_process, pid, rate_limit) {
        Ok(drive) => {
            audit_log("GRANTED", pid, exe, Some(drive), "handle issued");
        }
        Err(err) => {
            audit_log("FAILED", pid, exe, None, &err.to_string());
            tracing::debug!(error = %err, "Pipe request failed");
        }
    }
}

/// S5.4: Handle request with per-drive rate limiting.
#[cfg(windows)]
fn handle_pipe_request_with_rate_limit(
    pipe: windows::Win32::Foundation::HANDLE,
    client_process: &OwnedProcessHandle,
    client_pid: u32,
    rate_limit: &RateLimit,
) -> anyhow::Result<char> {
    // Peek at the 1-byte request via the shared protocol parser.
    // `HandleRequest::parse` rejects non-ASCII bytes and non-alphabetic
    // ASCII bytes with structured errors — replaces the two-step
    // `is_ascii_alphabetic` validation we used to do here.
    let mut req_buf = [0_u8; uffs_broker_protocol::REQUEST_WIRE_LEN];
    read_pipe(pipe, &mut req_buf)?;
    let drive_letter = match HandleRequest::parse(req_buf[0]) {
        Ok(req) => req.drive,
        Err(parse_err) => {
            write_pipe(pipe, &HandleResponse::error().encode())?;
            return Err(anyhow::anyhow!(
                "invalid drive-letter request byte: {parse_err}"
            ));
        }
    };

    // S5.4: Rate limit — 1 request per drive per 10 seconds.  Decide under the
    // lock (recovering a poisoned mutex), then act without holding it so no
    // pipe I/O happens while the shared map is locked.
    let now = std::time::Instant::now();
    let rate_limited = {
        let mut guard = rate_limit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let limited = guard
            .get(&drive_letter)
            .is_some_and(|last| now.duration_since(*last).as_secs() < 10);
        if !limited {
            guard.insert(drive_letter, now);
        }
        limited
    };
    if rate_limited {
        write_pipe(pipe, &HandleResponse::error().encode())?;
        anyhow::bail!("Rate limited: drive {drive_letter} requested too recently");
    }

    // Delegate to actual handle brokering (drive letter already read)
    handle_pipe_request_inner(pipe, client_process, client_pid, drive_letter)?;
    Ok(drive_letter)
}

/// S5.3: Audit log entry to tracing (and Windows Event Log if available).
#[cfg(windows)]
fn audit_log(action: &str, pid: u32, exe: Option<&str>, drive: Option<char>, detail: &str) {
    let exe_str = exe.unwrap_or("<unknown>");
    let drive_str = drive.map_or_else(|| "-".to_owned(), |drive_char| drive_char.to_string());
    tracing::info!(
        target: "uffs_broker::audit",
        action,
        pid,
        exe = exe_str,
        drive = %drive_str,
        detail,
        "AUDIT"
    );
    // Future: also write to Windows Event Log via ReportEventW
}

// ── D7.3: Named Pipe Operations ─────────────────────────────────────────

/// Disconnect any connected client from a pipe instance (the handle itself is
/// closed separately — by [`close_pipe`] or the worker's [`OwnedHandle`] drop).
#[cfg(windows)]
#[expect(unsafe_code, reason = "DisconnectNamedPipe is an FFI call")]
fn disconnect_pipe(pipe: windows::Win32::Foundation::HANDLE) {
    use windows::Win32::System::Pipes::DisconnectNamedPipe;

    // SAFETY: `pipe` is a valid HANDLE created by create_broker_pipe; the
    // disconnect is a no-op if no client is currently connected.
    if let Err(err) = unsafe { DisconnectNamedPipe(pipe) } {
        tracing::debug!(err = ?err, "DisconnectNamedPipe failed (may be already disconnected)");
    }
}

/// Close a pipe instance handle.  Used on the accept-loop error path where the
/// instance isn't wrapped in an [`OwnedHandle`]; the worker path closes via the
/// `OwnedHandle` drop instead.
#[cfg(windows)]
#[expect(unsafe_code, reason = "CloseHandle is an FFI call")]
fn close_pipe(pipe: windows::Win32::Foundation::HANDLE) {
    use windows::Win32::Foundation::CloseHandle;

    // SAFETY: `pipe` is about to be discarded; CloseHandle releases its OS
    // kernel-object reference.  Failure is logged but non-fatal.
    if let Err(err) = unsafe { CloseHandle(pipe) } {
        tracing::debug!(err = ?err, "CloseHandle failed for pipe");
    }
}

// ── D7.4: Client Process Verification ───────────────────────────────────

/// Get the PID of the connected pipe client.
#[cfg(windows)]
#[expect(unsafe_code, reason = "GetNamedPipeClientProcessId is an FFI call")]
fn get_pipe_client_pid(pipe: windows::Win32::Foundation::HANDLE) -> Option<u32> {
    use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;

    let mut pid: u32 = 0;

    // SAFETY: `pipe` is a valid server-side pipe HANDLE with an active
    // connection; `pid` is a stack-allocated u32 whose exclusive mutable
    // pointer is passed.
    let result = unsafe { GetNamedPipeClientProcessId(pipe, &raw mut pid) };

    (result.is_ok() && pid != 0).then_some(pid)
}

// ── D7.5: Handle Brokering ──────────────────────────────────────────────

/// Open the NTFS volume for a drive letter with read-only backup semantics.
#[cfg(windows)]
#[expect(unsafe_code, reason = "CreateFileW is an FFI call")]
fn open_volume_read_only(drive_letter: char) -> anyhow::Result<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_FLAG_SEQUENTIAL_SCAN,
        FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let volume_path = format!("\\\\.\\{drive_letter}:");
    let wide_path: Vec<u16> = volume_path.encode_utf16().chain(Some(0)).collect();

    // SAFETY: `wide_path` is a NUL-terminated UTF-16 buffer owned for the
    // duration of this call; all other arguments are plain integers or None.
    //
    // The handle is duplicated into the (non-elevated) daemon, which reads
    // the MFT through it via overlapped/IOCP I/O — so it must be opened
    // `FILE_FLAG_OVERLAPPED` (and `SEQUENTIAL_SCAN`, matching the reader's
    // direct-open flags in `uffs-mft::VolumeHandle`).  Without OVERLAPPED the
    // daemon's IOCP reads on the vended handle fail.
    let create_file_result = unsafe {
        CreateFileW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED | FILE_FLAG_SEQUENTIAL_SCAN,
            None,
        )
    };
    create_file_result.map_err(|err| anyhow::anyhow!("CreateFileW failed for {volume_path}: {err}"))
}

/// Duplicate `volume_handle` into the client process with read-only access.
///
/// **WI-8.1:** the duplicate target is the **same** `client_process` handle
/// that was verified in `check_client_identity` — passed in by the caller, not
/// re-opened from the PID. This closes the verify-then-reopen race: there is no
/// window in which a recycled PID could point the grant at a different process.
///
/// The caller retains ownership of both `volume_handle` and `client_process`.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetCurrentProcess + DuplicateHandle are FFI calls"
)]
fn duplicate_volume_handle_to_client(
    volume_handle: windows::Win32::Foundation::HANDLE,
    client_process: &OwnedProcessHandle,
) -> anyhow::Result<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::FILE_GENERIC_READ;
    use windows::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: GetCurrentProcess is a Win32 pseudo-handle getter (never fails).
    let current_process = unsafe { GetCurrentProcess() };

    let mut client_handle = HANDLE::default();
    // SAFETY: `current_process`, `volume_handle`, and `client_process.raw()`
    // are all valid handles (the client handle is kept alive by the caller's
    // `OwnedProcessHandle`); `client_handle` is a stack-owned HANDLE passed via
    // exclusive &raw mut.
    let dup_ok = unsafe {
        windows::Win32::Foundation::DuplicateHandle(
            current_process,
            volume_handle,
            client_process.raw(),
            &raw mut client_handle,
            FILE_GENERIC_READ.0,
            false,
            windows::Win32::Foundation::DUPLICATE_HANDLE_OPTIONS(0),
        )
    };

    dup_ok.map_err(|err| anyhow::anyhow!("DuplicateHandle failed: {err}"))?;
    Ok(client_handle)
}

/// Write the success response (`status=Ok` + LE-u64 handle) to the
/// pipe via the shared protocol encoder.  Returns the serialised handle
/// value on success.
#[cfg(windows)]
fn write_success_response(
    pipe: windows::Win32::Foundation::HANDLE,
    client_handle: windows::Win32::Foundation::HANDLE,
) -> anyhow::Result<u64> {
    // Win32 `HANDLE.0` is `isize`; reinterpret its bit pattern as `u64`
    // for IPC.  The receiving daemon does the inverse via
    // `HANDLE(handle_value as isize)` so the kernel-object pointer
    // round-trips unchanged.  No clippy expect needed here — the
    // workspace's cast lints don't fire on the cast in this context.
    let handle_value = client_handle.0 as u64;
    let response: [u8; RESPONSE_WIRE_LEN] = HandleResponse::ok(handle_value).encode();
    write_pipe(pipe, &response)?;
    Ok(handle_value)
}

/// Handle a pipe request after rate limiting (drive letter already read).
///
/// S5.5: Only issues read-only handles (`FILE_GENERIC_READ`), never write
/// access.
#[cfg(windows)]
#[expect(unsafe_code, reason = "CloseHandle is an FFI call")]
fn handle_pipe_request_inner(
    pipe: windows::Win32::Foundation::HANDLE,
    client_process: &OwnedProcessHandle,
    client_pid: u32,
    drive_letter: char,
) -> anyhow::Result<()> {
    use windows::Win32::Foundation::CloseHandle;

    tracing::info!(drive = %drive_letter, client_pid, "Opening volume for client");

    let volume_handle = match open_volume_read_only(drive_letter) {
        Ok(handle) => handle,
        Err(err) => {
            write_pipe(pipe, &HandleResponse::error().encode())?;
            return Err(err);
        }
    };

    let dup_result = duplicate_volume_handle_to_client(volume_handle, client_process);

    // Close our copy of the volume handle regardless of whether the duplicate
    // succeeded — the client has its own handle now, or the whole request
    // failed.
    // SAFETY: `volume_handle` came from `open_volume_read_only` above.
    if let Err(close_err) = unsafe { CloseHandle(volume_handle) } {
        tracing::debug!(err = ?close_err, "CloseHandle(volume_handle) failed after dup");
    }

    let client_handle = match dup_result {
        Ok(handle) => handle,
        Err(err) => {
            write_pipe(pipe, &HandleResponse::error().encode())?;
            return Err(err);
        }
    };

    let handle_value = write_success_response(pipe, client_handle)?;
    tracing::info!(
        drive = %drive_letter,
        client_pid,
        handle = handle_value,
        "Volume handle brokered successfully"
    );
    Ok(())
}

/// Read exact bytes from the pipe.
#[cfg(windows)]
#[expect(unsafe_code, reason = "ReadFile is an FFI call")]
fn read_pipe(pipe: windows::Win32::Foundation::HANDLE, buf: &mut [u8]) -> anyhow::Result<()> {
    use windows::Win32::Storage::FileSystem::ReadFile;

    let mut bytes_read = 0_u32;

    // SAFETY: `pipe` is a valid open pipe HANDLE; `buf` is a caller-owned
    // mutable slice; `bytes_read` is a stack-owned u32 accessed exclusively.
    let result = unsafe { ReadFile(pipe, Some(buf), Some(&raw mut bytes_read), None) };

    if let Err(win_err) = result {
        anyhow::bail!("ReadFile failed: {win_err}");
    }
    if (bytes_read as usize) < buf.len() {
        // u32→usize lossless on 64-bit
        anyhow::bail!("Short read: got {bytes_read}, expected {}", buf.len());
    }
    Ok(())
}

/// Write bytes to the pipe.
///
/// Flushes via `FlushFileBuffers` after a successful `WriteFile`, before
/// returning — the caller (`serve_pipe_requests`) disconnects the pipe
/// immediately once this returns, and `WriteFile` succeeding only means
/// the bytes reached the pipe's kernel buffer, not that the client has
/// read them. Without the flush, `DisconnectNamedPipe` can discard a
/// buffered-but-unread response out from under the client. Same fix as
/// `snapshot_manager::write_framed_message`, applied here for the same
/// reason even though this pipe's fixed 9-byte response makes the race
/// far narrower in practice.
#[cfg(windows)]
#[expect(unsafe_code, reason = "WriteFile/FlushFileBuffers are FFI calls")]
fn write_pipe(pipe: windows::Win32::Foundation::HANDLE, buf: &[u8]) -> anyhow::Result<()> {
    use windows::Win32::Storage::FileSystem::{FlushFileBuffers, WriteFile};

    let mut bytes_written = 0_u32;

    // SAFETY: `pipe` is a valid open pipe HANDLE; `buf` is a caller-owned
    // immutable slice; `bytes_written` is a stack-owned u32.
    let result = unsafe { WriteFile(pipe, Some(buf), Some(&raw mut bytes_written), None) };

    if let Err(win_err) = result {
        anyhow::bail!("WriteFile failed: {win_err}");
    }
    // SAFETY: `pipe` is the same valid, still-open pipe HANDLE written to
    // above.
    if let Err(win_err) = unsafe { FlushFileBuffers(pipe) } {
        anyhow::bail!("FlushFileBuffers failed: {win_err}");
    }
    Ok(())
}

// ── Elevation Check ─────────────────────────────────────────────────────

/// Return `true` when the current process is running with an elevated
/// (administrator) token.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "OpenProcessToken + GetTokenInformation + CloseHandle are FFI calls"
)]
fn is_elevated() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess is a Win32 pseudo-handle getter (never fails).
    let current_process = unsafe { GetCurrentProcess() };
    // SAFETY: `current_process` is a valid pseudo-handle; `token` is a stack-
    // owned HANDLE passed via exclusive &raw mut.
    if unsafe { OpenProcessToken(current_process, TOKEN_QUERY, &raw mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = 0_u32;
    // SAFETY: `token` is a valid token handle from OpenProcessToken above;
    // `elevation` and `size` are stack-owned locals accessed via &raw mut.
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast::<core::ffi::c_void>()),
            // size_of::<TOKEN_ELEVATION>() is 4 bytes — always fits u32.
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(u32::MAX),
            &raw mut size,
        )
    };
    // SAFETY: `token` came from OpenProcessToken above; closing it releases
    // the kernel-object reference.  Failure is debug-logged, not fatal.
    if let Err(close_err) = unsafe { windows::Win32::Foundation::CloseHandle(token) } {
        tracing::debug!(err = ?close_err, "CloseHandle(token) failed in is_elevated");
    }
    result.is_ok() && elevation.TokenIsElevated != 0
}

// ── Non-Windows stub ────────────────────────────────────────────────────
