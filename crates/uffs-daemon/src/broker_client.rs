// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Daemon-side broker client (D7.7).
//!
//! When running on Windows, the daemon can optionally use the Access Broker
//! to obtain elevated volume handles instead of requiring its own elevation.
//!
//! Flow:
//! 1. Check if the broker pipe exists (`uffs_broker_protocol::PIPE_NAME`)
//! 2. Connect to it
//! 3. Encode the drive letter via `uffs_broker_protocol::HandleRequest::encode`
//! 4. Decode the response via `uffs_broker_protocol::HandleResponse::parse`
//! 5. Use the handle for MFT reading
//!
//! The wire format used to be duplicated here as a `const BROKER_PIPE_NAME`
//! plus hand-rolled byte-slicing with a `// must match
//! uffs-broker/src/broker.rs` reviewer-comment as the only protection
//! against drift.  F5 (issue #205) promoted those shared symbols to
//! the dedicated `uffs_broker_protocol` crate, eliminating the
//! textual coupling.
//!
//! `uffs-broker-protocol` is scoped to
//! `[target.'cfg(windows)'.dependencies]` in `Cargo.toml`, so it isn't
//! an extern crate at all on non-Windows targets — no `use … as _;`
//! marker is needed.

// NOTE: there is intentionally no `broker_available()` probe.  A
// `GetFileAttributesW` existence check on the pipe *connects to* the broker's
// single instance and leaves it busy, starving the real handle request with
// ERROR_PIPE_BUSY (2026-06-13 VM finding).  Broker presence is established
// solely by `request_volume_handle` attempting the connection.

/// Request a volume handle from the broker for a drive letter.
///
/// Returns the raw handle value (as a `u64`) that can be used for MFT reading.
/// The handle is already duplicated into our process by the broker.
#[cfg(windows)]
pub(crate) fn request_volume_handle(
    drive_letter: uffs_mft::platform::DriveLetter,
) -> anyhow::Result<u64> {
    let response = broker_pipe_round_trip(drive_letter)?;
    interpret_handle_response(drive_letter, response)
}

/// Open the broker pipe, send the 1-byte drive request, and read the raw
/// 9-byte response.  Split from [`request_volume_handle`] to keep both under
/// the cognitive-complexity ceiling and to isolate the I/O failure points
/// (the diagnostic `tracing` calls pinpoint where a non-elevated daemon's
/// access to the broker pipe breaks).
#[cfg(windows)]
fn broker_pipe_round_trip(
    drive_letter: uffs_mft::platform::DriveLetter,
) -> anyhow::Result<[u8; uffs_broker_protocol::RESPONSE_WIRE_LEN]> {
    use std::io::{Read as _, Write as _};

    use uffs_broker_protocol::{HandleRequest, PIPE_NAME, RESPONSE_WIRE_LEN};

    tracing::debug!(drive = %drive_letter, pipe = PIPE_NAME, "Opening broker pipe");
    let mut pipe = open_broker_pipe(PIPE_NAME)?;
    tracing::debug!(drive = %drive_letter, "Broker pipe opened; sending request");

    let request_bytes = HandleRequest {
        drive: drive_letter.as_char(),
    }
    .encode();
    pipe.write_all(&request_bytes)?;
    pipe.flush()?;

    let mut response = [0_u8; RESPONSE_WIRE_LEN];
    pipe.read_exact(&mut response)?;
    tracing::debug!(drive = %drive_letter, "Broker response received");
    Ok(response)
}

/// Open `pipe_name`, retrying only `ERROR_PIPE_BUSY` (231).
///
/// The Broker keeps one listening instance and creates the next right
/// after a client connects, so for a few microseconds every instance can be
/// taken — by a Coordinator asking for a VSS lease, or by a second daemon
/// overlapping this one across a restart.  Retrying is safe because nothing
/// has been sent yet; without it the drive falls back to a direct open,
/// which a non-elevated daemon cannot make.  Backoff 10, 20, 40, 80 ms.
#[cfg(windows)]
fn open_broker_pipe(pipe_name: &str) -> anyhow::Result<std::fs::File> {
    /// `ERROR_PIPE_BUSY`: every instance is connected for the moment.
    const ERROR_PIPE_BUSY: i32 = 231;
    /// Open attempts before the busy pipe counts as a failure.
    const OPEN_ATTEMPTS: u32 = 5;

    let mut attempt = 1_u32;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(std::path::Path::new(pipe_name))
        {
            Ok(pipe) => return Ok(pipe),
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempt < OPEN_ATTEMPTS => {
                std::thread::sleep(core::time::Duration::from_millis(10_u64 << (attempt - 1)));
                attempt += 1;
            }
            Err(err) => anyhow::bail!("opening broker pipe: {err}"),
        }
    }
}

/// Parse and interpret the broker's 9-byte response into a handle value.
///
/// Split out of [`request_volume_handle`] to keep that function under the
/// cognitive-complexity ceiling.
#[cfg(windows)]
fn interpret_handle_response(
    drive_letter: uffs_mft::platform::DriveLetter,
    response: [u8; uffs_broker_protocol::RESPONSE_WIRE_LEN],
) -> anyhow::Result<u64> {
    use uffs_broker_protocol::{HandleResponse, Status};

    let parsed = HandleResponse::parse(response).map_err(|parse_err| {
        anyhow::anyhow!("malformed broker response for drive {drive_letter}: {parse_err}")
    })?;
    match parsed.status {
        Status::Ok => {
            tracing::info!(
                drive = %drive_letter,
                handle = parsed.handle,
                "Received volume handle from broker"
            );
            Ok(parsed.handle)
        }
        Status::Error => {
            anyhow::bail!("Broker returned Status::Error for drive {drive_letter}")
        }
    }
}

/// Run the broker warm-up only when the daemon is **not** already elevated.
///
/// Gate on the daemon's OWN elevation, not on a broker probe.  An elevated
/// daemon opens volumes directly (`CreateFileW`), so a broker request would be
/// a futile per-drive pipe-open + WARN.  Crucially, `is_elevated()` is a token
/// query — it does NOT touch the broker pipe, so it has none of the race the
/// removed `broker_available()` probe had (that probe connected to and consumed
/// the broker's single pipe instance, starving the real request; 2026-06-13 VM
/// finding).  When NOT elevated, the handle request itself is the authoritative
/// broker-presence test: it succeeds when a broker is serving and fails fast
/// (WARN + direct-open fallback) when not.
///
/// Extracted from `crate::load_live_drives_if_windows` so that caller stays
/// under the `cognitive_complexity` ceiling.
#[cfg(windows)]
pub(crate) fn warm_up_broker_handles_unless_elevated(drives: &[uffs_mft::platform::DriveLetter]) {
    if uffs_mft::is_elevated() {
        tracing::debug!(
            pid = std::process::id(),
            "Daemon is elevated — skipping broker warm-up (direct volume open)"
        );
        return;
    }
    tracing::info!(
        pid = std::process::id(),
        drive_count = drives.len(),
        "Daemon not elevated — attempting broker warm-up"
    );
    warm_up_broker_handles(drives);
}

/// Best-effort broker pre-warm: ask the elevated broker for a volume
/// handle per drive so the subsequent `load_live_drives` skips the
/// per-drive elevation prompt.  Failures are debug-traced and
/// ignored — the direct-open path takes over transparently.
#[cfg(windows)]
fn warm_up_broker_handles(drives: &[uffs_mft::platform::DriveLetter]) {
    tracing::info!(
        daemon_pid = std::process::id(),
        drives = ?drives,
        "warm_up_broker_handles: requesting volume handles from the Access Broker"
    );
    for &drive_letter in drives {
        match request_volume_handle(drive_letter) {
            Ok(handle) => {
                // Deposit the broker's (elevated, overlapped) volume handle in
                // the uffs-mft registry; the subsequent `VolumeHandle::open`
                // for this drive adopts it instead of calling `CreateFileW`
                // (which a non-elevated daemon can't do).  This is what makes
                // the broker path actually load the MFT — previously the
                // handle was fetched and dropped, so the reader fell back to a
                // direct open and failed with access-denied.
                uffs_mft::register_broker_handle(drive_letter, handle);
                tracing::info!(drive = %drive_letter, handle, "Registered broker volume handle");
            }
            Err(broker_err) => {
                tracing::warn!(
                    drive = %drive_letter,
                    error = %broker_err,
                    "Access Broker handle request FAILED — falling back to direct (elevated) open"
                );
            }
        }
    }
}
