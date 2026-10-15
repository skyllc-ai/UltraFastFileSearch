// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! [`crate::snapshot_lease::VssProvider`] implementation backed by
//! `uffs-vss-requestor`: spawns one helper process per snapshot, keeps
//! it alive (assigned to a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` Job
//! Object) for the lease's entire lifetime, and drives it over a private
//! JSON-lines control pipe. See
//! `docs/dev/architecture/uffs-vss-rust-cpp-shim-implementation-guide.md`
//! for the full design.
//!
//! The wire shape here (`HelperEvent`/`BrokerCommand`) is a deliberate,
//! documented duplicate of `uffs-vss-requestor::protocol` — that crate
//! is bin-only (no library target) and this protocol is tiny, private,
//! and owned end-to-end by this same Broker↔helper pairing, so a
//! dedicated shared protocol crate (the pattern this workspace otherwise
//! uses for every cross-process wire boundary) would be overhead without
//! benefit here. Keep the two definitions in sync by hand if either side
//! changes.

use core::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::FromRawHandle as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uffs_broker_protocol::snapshot_manager::VolumeIdentity;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
};
use windows::core::PCWSTR;

use crate::snapshot_lease::{SnapshotHandle, VssError, VssProvider};

/// Mirrors `uffs_vss_requestor::protocol::HelperEvent` — see this
/// module's doc comment.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum HelperEvent {
    /// The snapshot was created.
    Ready {
        /// This specific snapshot's GUID, canonical `{...}` string form.
        snapshot_id: String,
        /// The snapshot's device path, if the helper reported one.
        snapshot_device_object: Option<String>,
    },
    /// The snapshot set was explicitly deleted.
    Released,
    /// A VSS requestor operation failed.
    Failed {
        /// Which step of the requestor sequence failed.
        stage: i32,
        /// The failing `HRESULT`.
        hresult: i32,
        /// Human-readable diagnostic message.
        message: String,
    },
    /// Reply to [`BrokerCommand::Ping`].
    Pong,
}

/// Mirrors `uffs_vss_requestor::protocol::BrokerCommand` — see this
/// module's doc comment.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum BrokerCommand {
    /// Delete the snapshot set and exit.
    Release,
    /// Liveness check; expects a [`HelperEvent::Pong`] reply.
    Ping,
}

/// One live helper-process session.
struct HelperSession {
    /// Reader half of the control pipe (a clone of `writer`'s handle).
    reader: BufReader<File>,
    /// Writer half of the control pipe.
    writer: File,
    /// The helper process, kept open so the Job Object's kill-on-close
    /// net stays armed until this session is dropped.
    process_handle: HANDLE,
    /// The Job Object the helper was assigned to.
    job_handle: HANDLE,
}

#[expect(
    unsafe_code,
    reason = "kernel HANDLEs have no thread affinity, so moving them between \
              threads is sound; `File`/`BufReader<File>` are themselves already \
              Send, so this only concerns the two raw HANDLEs"
)]
// SAFETY: `process_handle` and `job_handle` are process-wide kernel object
// handles with no thread affinity — moving a `HelperSession` between threads
// (e.g. into the `SnapshotLeaseManager`'s session map, itself behind a
// `Mutex`) is sound. Concurrent *use* of the same handle would still need
// external synchronization, exactly as with the raw Win32 API; `Send` only
// makes the *move* type-safe.
unsafe impl Send for HelperSession {}

impl Drop for HelperSession {
    #[expect(
        unsafe_code,
        reason = "CloseHandle is an FFI call; see the inline SAFETY comment"
    )]
    fn drop(&mut self) {
        // SAFETY: both handles were opened by `spawn_helper` and are
        // closed exactly once here. If the helper already exited
        // gracefully (the normal `Release` path), this is inert; if it
        // is still alive for any other reason, closing the job's last
        // handle triggers `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
        let _job_close_result = unsafe { CloseHandle(self.job_handle) };
        // SAFETY: see above.
        let _process_close_result = unsafe { CloseHandle(self.process_handle) };
    }
}

/// Cleanup guard for a spawned-but-not-yet-adopted helper: closes the
/// process and Job Object handles (killing the helper via
/// kill-on-close) unless [`PendingSpawn::into_handles`] transfers
/// ownership to a [`HelperSession`] first.
struct PendingSpawn {
    /// The helper process, not yet confirmed ready.
    process_handle: HANDLE,
    /// The Job Object the helper was assigned to.
    job_handle: HANDLE,
}

impl PendingSpawn {
    /// Disarm cleanup and take ownership of the raw handles.
    fn into_handles(self) -> (HANDLE, HANDLE) {
        let this = core::mem::ManuallyDrop::new(self);
        (this.process_handle, this.job_handle)
    }
}

impl Drop for PendingSpawn {
    #[expect(
        unsafe_code,
        reason = "CloseHandle is an FFI call; see the inline SAFETY comment"
    )]
    fn drop(&mut self) {
        // SAFETY: both handles were opened by `spawn_helper`; closing
        // the job's last handle here (before the helper ever confirmed
        // readiness) kills the abandoned helper process.
        let _job_close_result = unsafe { CloseHandle(self.job_handle) };
        // SAFETY: see above.
        let _process_close_result = unsafe { CloseHandle(self.process_handle) };
    }
}

/// [`VssProvider`] backed by per-lease `uffs-vss-requestor` helper
/// processes.
pub(crate) struct WindowsVssProvider {
    /// Live sessions, keyed by snapshot ID bytes.
    sessions: Mutex<HashMap<Vec<u8>, HelperSession>>,
    /// Monotonic counter for unique per-lease pipe names.
    next_pipe_id: AtomicU64,
}

impl WindowsVssProvider {
    /// Construct an empty provider.
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_pipe_id: AtomicU64::new(1),
        }
    }

    /// Lock the session map, recovering from a poisoned mutex.
    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<Vec<u8>, HelperSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Send [`BrokerCommand::Ping`] to the live helper session for
    /// `snapshot_id` and wait for its [`HelperEvent::Pong`] reply — a
    /// real, wire-level round trip, not a bookkeeping-only check.
    ///
    /// `Ping` has no production caller yet (no periodic liveness check
    /// is wired up to a lease's lifetime); this exists purely so
    /// `--self-test-vss` can prove the Ping/Pong path actually works end
    /// to end the same way it already proves Ready/Release do —
    /// catching the exact class of named-pipe deadlock bug that hit
    /// `Release` before this path existed to exercise `Ping` too. Not
    /// part of the [`VssProvider`] trait: it has no cross-platform
    /// meaning for the `FakeVssProvider` unit tests, so it stays a
    /// `WindowsVssProvider`-only capability until something real needs
    /// it on the trait.
    ///
    /// # Errors
    /// Returns an error if there's no live session for `snapshot_id`,
    /// the write fails, or the helper doesn't reply with `Pong`.
    pub(crate) fn ping_lease(&self, snapshot_id: &[u8]) -> anyhow::Result<()> {
        let mut session = self
            .lock_sessions()
            .remove(snapshot_id)
            .ok_or_else(|| anyhow::anyhow!("no live session for this snapshot"))?;

        tracing::info!("vss: sending Ping to helper");
        let ping_result = send_ping_and_await_pong(&mut session);
        if ping_result.is_ok() {
            tracing::info!("vss: received Pong");
        }

        self.lock_sessions().insert(snapshot_id.to_vec(), session);
        ping_result
    }
}

/// Write [`BrokerCommand::Ping`] on `session`'s writer and block until
/// its [`HelperEvent::Pong`] reply — the body of
/// [`WindowsVssProvider::ping_lease`], split out so that function can
/// always reinsert `session` regardless of outcome.
fn send_ping_and_await_pong(session: &mut HelperSession) -> anyhow::Result<()> {
    let line = serde_json::to_string(&BrokerCommand::Ping)
        .map_err(|err| anyhow::anyhow!("failed to encode Ping: {err}"))?;
    writeln!(session.writer, "{line}")
        .and_then(|()| session.writer.flush())
        .map_err(|err| anyhow::anyhow!("failed to send Ping: {err}"))?;

    let event = read_helper_event(&mut session.reader)
        .map_err(|err| anyhow::anyhow!("failed to read helper response: {err}"))?;
    match event {
        Some(HelperEvent::Pong) => Ok(()),
        Some(HelperEvent::Failed {
            stage,
            hresult,
            message,
        }) => Err(anyhow::anyhow!(
            "stage={stage} hresult={hresult:#x}: {message}"
        )),
        Some(HelperEvent::Ready { .. } | HelperEvent::Released) => {
            Err(anyhow::anyhow!("unexpected event from helper after Ping"))
        }
        None => Err(anyhow::anyhow!(
            "helper closed the control pipe before replying to Ping"
        )),
    }
}

impl WindowsVssProvider {
    /// Register a newly-ready helper's session and build its
    /// [`SnapshotHandle`], or translate a `Failed`/unexpected event into
    /// a [`VssError`] — the tail half of [`Self::create_snapshot`],
    /// split out to keep that function's cognitive complexity down.
    fn finish_create_snapshot(
        &self,
        volume_path: &str,
        pending: PendingSpawn,
        reader: BufReader<File>,
        writer: File,
        event: HelperEvent,
    ) -> Result<SnapshotHandle, VssError> {
        match event {
            HelperEvent::Ready {
                snapshot_id,
                snapshot_device_object,
            } => {
                tracing::info!(
                    snapshot_id = %snapshot_id,
                    device = %snapshot_device_object.as_deref().unwrap_or("<none>"),
                    "vss: snapshot ready"
                );
                let (process_handle, job_handle) = pending.into_handles();
                let snapshot_id_bytes = snapshot_id.into_bytes();
                let session = HelperSession {
                    reader,
                    writer,
                    process_handle,
                    job_handle,
                };
                self.lock_sessions()
                    .insert(snapshot_id_bytes.clone(), session);
                Ok(SnapshotHandle {
                    snapshot_id: snapshot_id_bytes,
                    device_identity: snapshot_device_object.unwrap_or_default(),
                })
            }
            HelperEvent::Failed {
                stage,
                hresult,
                message,
            } => {
                // Kept at `warn!` (not silently propagated): without this,
                // an operator watching the Broker's own log sees "waiting
                // for Ready/Failed from helper" and then nothing for this
                // volume at all — the failure only became visible on the
                // Coordinator side, several process hops away. See
                // `uffs-content`'s `vss_orchestrator` for how
                // `VSS_E_VOLUME_NOT_SUPPORTED` specifically is handled
                // (skipped, not fatal) once it reaches that side.
                tracing::warn!(
                    volume = %volume_path,
                    stage,
                    hresult = format!("{hresult:#x}"),
                    message = %message,
                    "vss: snapshot creation failed"
                );
                Err(VssError::CreateFailed {
                    hresult: Some(hresult),
                    message: format!("stage={stage} hresult={hresult:#x}: {message}"),
                })
            }
            HelperEvent::Released | HelperEvent::Pong => {
                tracing::warn!(
                    volume = %volume_path,
                    ?event,
                    "vss: unexpected event from helper before Ready"
                );
                Err(VssError::CreateFailed {
                    hresult: None,
                    message: "unexpected event from helper before Ready".to_owned(),
                })
            }
        }
    }
}

/// Block until the helper connects to `pipe_handle`, then read its
/// first event — the middle third of `create_snapshot`, split out to
/// keep that function's cognitive complexity down.
#[expect(
    unsafe_code,
    reason = "wraps a freshly connected pipe HANDLE in a File; see the inline SAFETY comment"
)]
fn wait_for_helper_ready(
    pipe_handle: HANDLE,
) -> Result<(BufReader<File>, File, HelperEvent), VssError> {
    tracing::info!("vss: waiting for helper to connect to the control pipe");
    if let Err(err) = connect_pipe(pipe_handle) {
        close_pipe_handle(pipe_handle);
        return Err(VssError::CreateFailed {
            hresult: None,
            message: format!("helper did not connect to control pipe: {err}"),
        });
    }
    tracing::info!("vss: helper connected");

    // SAFETY: `pipe_handle` is a valid, connected, exclusively owned
    // duplex pipe HANDLE; `File` takes ownership and closes it on drop.
    let pipe_file = unsafe { File::from_raw_handle(pipe_handle.0.cast::<core::ffi::c_void>()) };
    let writer = pipe_file
        .try_clone()
        .map_err(|err| VssError::CreateFailed {
            hresult: None,
            message: format!("failed to clone pipe handle: {err}"),
        })?;
    let mut reader = BufReader::new(pipe_file);

    tracing::info!("vss: waiting for Ready/Failed from helper");
    let event = read_helper_event(&mut reader)
        .map_err(|err| VssError::CreateFailed {
            hresult: None,
            message: format!("failed to read helper event: {err}"),
        })?
        .ok_or_else(|| VssError::CreateFailed {
            hresult: None,
            message: "helper closed the control pipe before reporting readiness".to_owned(),
        })?;

    Ok((reader, writer, event))
}

impl VssProvider for WindowsVssProvider {
    fn create_snapshot(
        &self,
        _volume: &VolumeIdentity,
        requested_root: &[u8],
    ) -> Result<SnapshotHandle, VssError> {
        let volume_path = decode_utf16le(requested_root).ok_or_else(|| {
            VssError::InvalidVolume("requested_root is not valid UTF-16LE".to_owned())
        })?;

        let pipe_id = self.next_pipe_id.fetch_add(1, Ordering::Relaxed);
        let pipe_name = format!(r"\\.\pipe\uffs-vss-requestor-{pipe_id:016x}");
        tracing::info!(volume = %volume_path, pipe = %pipe_name, "vss: creating control pipe");

        let pipe_handle =
            create_control_pipe(&pipe_name).map_err(|err| VssError::CreateFailed {
                hresult: None,
                message: format!("failed to create control pipe: {err}"),
            })?;

        tracing::info!(volume = %volume_path, "vss: spawning uffs-vss-requestor");
        let pending =
            spawn_helper(&pipe_name, &volume_path).map_err(|err| VssError::CreateFailed {
                hresult: None,
                message: format!("failed to spawn uffs-vss-requestor: {err}"),
            })?;

        let (reader, writer, event) = wait_for_helper_ready(pipe_handle)?;
        self.finish_create_snapshot(&volume_path, pending, reader, writer, event)
    }

    fn delete_snapshot(&self, snapshot_id: &[u8]) -> Result<(), VssError> {
        tracing::info!(
            // AUDIT-OK(bytes): display-only log field; the actual lookup
            // just below keys off the raw `snapshot_id` bytes, not this
            // lossy string.
            snapshot_id = %String::from_utf8_lossy(snapshot_id),
            "vss: requesting snapshot deletion"
        );
        let mut session = self.lock_sessions().remove(snapshot_id).ok_or_else(|| {
            VssError::DeleteFailed("no live session for this snapshot".to_owned())
        })?;

        let command = BrokerCommand::Release;
        let write_result = serde_json::to_string(&command)
            .map_err(|err| VssError::DeleteFailed(format!("failed to encode Release: {err}")))
            .and_then(|line| {
                writeln!(session.writer, "{line}")
                    .and_then(|()| session.writer.flush())
                    .map_err(|err| VssError::DeleteFailed(format!("failed to send Release: {err}")))
            });
        write_result?;

        tracing::info!("vss: waiting for Released confirmation");
        let event = read_helper_event(&mut session.reader).map_err(|err| {
            VssError::DeleteFailed(format!("failed to read helper response: {err}"))
        })?;
        let result = match event {
            Some(HelperEvent::Released) | None => Ok(()),
            Some(HelperEvent::Failed {
                stage,
                hresult,
                message,
            }) => Err(VssError::DeleteFailed(format!(
                "stage={stage} hresult={hresult:#x}: {message}"
            ))),
            Some(HelperEvent::Ready { .. } | HelperEvent::Pong) => Err(VssError::DeleteFailed(
                "unexpected event from helper after Release".to_owned(),
            )),
        };
        if result.is_ok() {
            tracing::info!("vss: snapshot released");
        }
        result
        // `session` drops here regardless of outcome, closing the
        // process/job handles.
    }

    fn list_existing_snapshots(&self) -> Result<Vec<Vec<u8>>, VssError> {
        // `VSS_CTX_FILE_SHARE_BACKUP` is ephemeral and auto-release: a
        // snapshot's lifetime is tied to its helper process/Job Object,
        // which the OS itself tears down if the Broker dies (closing
        // every handle it held, including each Job Object — see
        // `docs/dev/architecture/uffs-vss-rust-cpp-shim-implementation-guide.
        // md` §6/§7). There is nothing left to reconcile at startup.
        Ok(Vec::new())
    }
}

/// Decode `bytes` as UTF-16LE, or `None` if the length is odd or the
/// units don't form valid UTF-16.
fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let (chunks, _remainder) = bytes.as_chunks::<2>();
    let units: Vec<u16> = chunks.iter().copied().map(u16::from_le_bytes).collect();
    String::from_utf16(&units).ok()
}

/// Read one [`HelperEvent`] line, or `Ok(None)` at EOF.
fn read_helper_event(reader: &mut BufReader<File>) -> std::io::Result<Option<HelperEvent>> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    let event = serde_json::from_str(line.trim_end())
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    Ok(Some(event))
}

/// Close a raw pipe `HANDLE` that was never adopted into a [`File`].
#[expect(unsafe_code, reason = "CloseHandle is an FFI call")]
fn close_pipe_handle(handle: HANDLE) {
    // SAFETY: `handle` was opened by `create_control_pipe` and has not
    // been wrapped in a `File` (which would otherwise double-close it).
    let _close_result = unsafe { CloseHandle(handle) };
}

/// Create the Broker-side control pipe instance for one snapshot lease.
///
/// Uses default security: the helper runs under the same identity as
/// the Broker (`CreateProcessW` inherits the parent's token unless told
/// otherwise), so no custom SDDL is needed the way the Coordinator- and
/// daemon-facing pipes require.
#[expect(unsafe_code, reason = "CreateNamedPipeW is an FFI call")]
fn create_control_pipe(pipe_name: &str) -> anyhow::Result<HANDLE> {
    let wide_name: Vec<u16> = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect();

    // SAFETY: `wide_name` is a NUL-terminated UTF-16 buffer valid for the
    // duration of this call; `None` security attributes fall back to the
    // creating thread's default DACL, which already permits the
    // same-identity helper process to connect.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide_name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            8192,
            8192,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        anyhow::bail!(
            "CreateNamedPipeW failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(handle)
}

/// Block until the helper connects to `pipe_handle`.
///
/// The helper is launched before this wait starts, so it can connect first;
/// the shared Broker accept wait treats that (`ERROR_PIPE_CONNECTED`) as the
/// success it is.
fn connect_pipe(pipe_handle: HANDLE) -> anyhow::Result<()> {
    super::super::wait_for_client(pipe_handle)
}

/// Locate `uffs-vss-requestor.exe`.
///
/// In production this lives alongside the Broker binary (same install
/// directory) — `current_exe()`'s parent. `uffs-vss-requestor` is a
/// bin-only crate (Cargo refuses to add it as a dependency of any kind:
/// "ignoring invalid dependency ... missing a lib target", the same
/// restriction the root `Cargo.toml` documents for why bin-only crates
/// get no workspace-dependency alias), so there is no
/// `CARGO_BIN_EXE_*` env var to fall back on under `cargo test` either.
/// Test binaries run one directory deeper than production binaries
/// (`target/<triple>/<profile>/deps/`, not
/// `target/<triple>/<profile>/`), so also check the parent's parent —
/// where `cargo build -p uffs-vss-requestor` actually places the `.exe`
/// — before giving up and returning the production guess for the caller
/// to fail against with a clear `CreateProcessW` error.
fn helper_exe_path() -> anyhow::Result<PathBuf> {
    let current_exe = std::env::current_exe()
        .map_err(|err| anyhow::anyhow!("failed to resolve current_exe: {err}"))?;
    let parent = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current_exe has no parent directory"))?;
    let sibling = parent.join("uffs-vss-requestor.exe");
    if sibling.is_file() {
        return Ok(sibling);
    }
    if let Some(profile_dir) = parent.parent() {
        let candidate = profile_dir.join("uffs-vss-requestor.exe");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(sibling)
}

/// Spawn `uffs-vss-requestor.exe`, suspended, assign it to a fresh
/// kill-on-close Job Object, then resume it.
#[expect(
    unsafe_code,
    reason = "CreateProcessW, CreateJobObjectW, SetInformationJobObject, \
              AssignProcessToJobObject, and ResumeThread are FFI calls"
)]
fn spawn_helper(pipe_name: &str, volume_path: &str) -> anyhow::Result<PendingSpawn> {
    let exe_path = helper_exe_path()?;
    let parent_pid = std::process::id();
    tracing::info!(exe = %exe_path.display(), parent_pid, "vss: launching helper process");
    let mut command_line = build_command_line(&exe_path, pipe_name, volume_path, parent_pid);

    let startup_info = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(0),
        ..Default::default()
    };
    let mut process_information = PROCESS_INFORMATION::default();

    // SAFETY: `command_line` is a mutable, NUL-terminated UTF-16 buffer
    // (required: `CreateProcessW` may write into it); `startup_info` and
    // `process_information` are stack-owned and exclusively borrowed for
    // the call.
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(windows::core::PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED,
            None,
            PCWSTR::null(),
            &raw const startup_info,
            &raw mut process_information,
        )
    }
    .map_err(|err| anyhow::anyhow!("CreateProcessW failed: {err}"))?;

    let process_handle = process_information.hProcess;
    let thread_handle = process_information.hThread;
    tracing::info!(
        helper_pid = process_information.dwProcessId,
        "vss: helper process created (suspended)"
    );

    // SAFETY: `None` name creates an anonymous Job Object; the returned
    // handle is owned by this function until transferred via
    // `PendingSpawn`.
    let job_handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|err| anyhow::anyhow!("CreateJobObjectW failed: {err}"))?;

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `job_handle` is a valid, freshly created Job Object handle;
    // `limits` is a stack-owned, correctly sized structure for
    // `JobObjectExtendedLimitInformation`.
    let set_info_result = unsafe {
        SetInformationJobObject(
            job_handle,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast::<core::ffi::c_void>(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
        )
    };
    if let Err(err) = set_info_result {
        close_pipe_handle(job_handle);
        close_pipe_handle(process_handle);
        close_pipe_handle(thread_handle);
        anyhow::bail!("SetInformationJobObject failed: {err}");
    }

    // SAFETY: `job_handle` and `process_handle` are both valid; assigning
    // a suspended process to the job before it ever runs means it can
    // never escape the job's kill-on-close net.
    let assign_result = unsafe { AssignProcessToJobObject(job_handle, process_handle) };
    if let Err(err) = assign_result {
        close_pipe_handle(job_handle);
        close_pipe_handle(process_handle);
        close_pipe_handle(thread_handle);
        anyhow::bail!("AssignProcessToJobObject failed: {err}");
    }

    // SAFETY: `thread_handle` is the valid main-thread handle from
    // `CreateProcessW`, still suspended.
    let resume_result = unsafe { ResumeThread(thread_handle) };
    close_pipe_handle(thread_handle);
    if resume_result == u32::MAX {
        close_pipe_handle(job_handle);
        close_pipe_handle(process_handle);
        anyhow::bail!("ResumeThread failed: {}", std::io::Error::last_os_error());
    }
    tracing::info!("vss: helper process resumed");

    Ok(PendingSpawn {
        process_handle,
        job_handle,
    })
}

/// Build the helper's command line: `"<exe>" --pipe-name "<name>"
/// --volume-path "<path>" --parent-pid <pid>`, NUL-terminated UTF-16.
fn build_command_line(
    exe_path: &Path,
    pipe_name: &str,
    volume_path: &str,
    parent_pid: u32,
) -> Vec<u16> {
    let command = format!(
        "{} --pipe-name {} --volume-path {} --parent-pid {parent_pid}",
        quote_windows_arg(&exe_path.display().to_string()),
        quote_windows_arg(pipe_name),
        quote_windows_arg(volume_path),
    );
    command.encode_utf16().chain(Some(0)).collect()
}

/// Quote `arg` for a `CreateProcessW` command line, following the
/// escaping rules `CommandLineToArgvW` (and the C-runtime argv parser
/// `std::env::args()` also uses) expect: a run of backslashes is only
/// literal if followed by something other than a quote, so any run
/// immediately preceding an embedded or closing quote must be doubled.
///
/// This matters here because a volume root (e.g. `C:\`) always ends in
/// exactly one backslash — naively wrapping it as `"C:\"` makes that
/// single trailing backslash escape the closing quote instead of
/// terminating the argument, silently merging every argument after it
/// (`--parent-pid <pid>`) into this one. The helper then never receives
/// a `--parent-pid`, fails to parse its own arguments, and exits before
/// ever connecting to the control pipe — which left `create_snapshot`
/// blocked in `connect_pipe` forever, waiting on a process that had
/// already exited.
fn quote_windows_arg(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut pending_backslashes = 0_usize;
    for ch in arg.chars() {
        if ch == '\\' {
            pending_backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.extend(core::iter::repeat_n('\\', pending_backslashes * 2 + 1));
            quoted.push('"');
        } else {
            quoted.extend(core::iter::repeat_n('\\', pending_backslashes));
            quoted.push(ch);
        }
        pending_backslashes = 0;
    }
    // Any backslashes still pending are immediately followed by the
    // closing quote we're about to append, so they must be doubled too.
    quoted.extend(core::iter::repeat_n('\\', pending_backslashes * 2));
    quoted.push('"');
    quoted
}

/// Encode `path` as the lossless UTF-16LE `requested_root` wire format
/// [`WindowsVssProvider::create_snapshot`] expects. `pub(crate)` since
/// `vss_self_test` (the `--self-test-vss` implementation) also needs it.
pub(crate) fn utf16le_bytes(path: &str) -> Vec<u8> {
    path.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[cfg(test)]
mod tests {
    /// A drive root's trailing backslash must be doubled, not passed
    /// through as-is — a single backslash immediately before the
    /// closing quote escapes the quote instead of terminating the
    /// argument, which is exactly the bug that let `--parent-pid`
    /// silently vanish (see `quote_windows_arg`'s doc comment).
    #[test]
    fn quote_windows_arg_doubles_a_trailing_backslash() {
        assert_eq!(super::quote_windows_arg(r"C:\"), r#""C:\\""#);
    }

    #[test]
    fn quote_windows_arg_passes_plain_text_through() {
        assert_eq!(super::quote_windows_arg("plain"), r#""plain""#);
    }

    #[test]
    fn quote_windows_arg_escapes_embedded_quotes() {
        assert_eq!(super::quote_windows_arg(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn quote_windows_arg_doubles_backslashes_before_an_embedded_quote() {
        assert_eq!(super::quote_windows_arg(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn quote_windows_arg_leaves_interior_backslashes_alone() {
        assert_eq!(
            super::quote_windows_arg(r"C:\Users\rnio\bin\uffs-vss-requestor.exe"),
            r#""C:\Users\rnio\bin\uffs-vss-requestor.exe""#
        );
    }

    #[test]
    fn quote_windows_arg_handles_empty_string() {
        assert_eq!(super::quote_windows_arg(""), r#""""#);
    }
}
