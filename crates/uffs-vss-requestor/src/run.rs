// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Top-level orchestration: parse arguments, create the snapshot,
//! report readiness, then wait for `Release`/`Cancel`/pipe-closed/
//! parent-death and tear down accordingly.

use std::io::BufReader;
use std::sync::mpsc;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

use crate::pipe;
use crate::protocol::{self, BrokerCommand, HelperEvent};
use crate::snapshot::{SnapshotDescriptor, VssRequestError, VssSnapshotSession};

/// Parsed command-line arguments.
struct Args {
    /// Name of the private control pipe to connect to.
    pipe_name: String,
    /// Canonical volume path to snapshot.
    volume_path: String,
    /// PID of the spawning Broker, watched for early exit.
    parent_pid: u32,
}

impl Args {
    /// Parse `std::env::args()`, skipping argv\[0\].
    ///
    /// # Errors
    /// Returns an error if a required flag is missing or a value fails
    /// to parse.
    fn parse() -> anyhow::Result<Self> {
        let mut pipe_name = None;
        let mut volume_path = None;
        let mut parent_pid = None;

        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--pipe-name" => pipe_name = Some(next_value(&mut args, "--pipe-name")?),
                "--volume-path" => volume_path = Some(next_value(&mut args, "--volume-path")?),
                "--parent-pid" => {
                    let value = next_value(&mut args, "--parent-pid")?;
                    parent_pid = Some(
                        value
                            .parse::<u32>()
                            .map_err(|err| anyhow::anyhow!("invalid --parent-pid: {err}"))?,
                    );
                }
                other => anyhow::bail!("unrecognized argument: {other}"),
            }
        }

        Ok(Self {
            pipe_name: pipe_name.ok_or_else(|| anyhow::anyhow!("--pipe-name is required"))?,
            volume_path: volume_path.ok_or_else(|| anyhow::anyhow!("--volume-path is required"))?,
            parent_pid: parent_pid.ok_or_else(|| anyhow::anyhow!("--parent-pid is required"))?,
        })
    }
}

/// Take the next positional value for `flag`, or an error if argv ran
/// out.
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

/// Total attempts [`create_snapshot_with_retry`] makes before giving up
/// (the first attempt plus this many retries).
const MAX_SNAPSHOT_ATTEMPTS: u32 = 3;

/// Backoff before retry attempt `N` (1-based): attempt 2 waits
/// [`RETRY_BACKOFF_BASE`], attempt 3 waits `RETRY_BACKOFF_BASE * 2`, and
/// so on — a short exponential backoff bounded by [`MAX_SNAPSHOT_ATTEMPTS`]
/// so a genuinely stuck volume fails in single-digit-second multiples of
/// this, not indefinitely.
const RETRY_BACKOFF_BASE: core::time::Duration = core::time::Duration::from_secs(2);

/// Create a `VSS_CTX_FILE_SHARE_BACKUP` snapshot of `volume_path`,
/// retrying with backoff on the small set of `HRESULT`s VSS documents as
/// transient ([`uffs_broker_protocol::vss_hresult::RETRYABLE`]). Each attempt
/// is a fully fresh [`VssSnapshotSession::create`] call — a brand-new
/// `IVssBackupComponents` session — which is exactly what Microsoft's own
/// guidance requires: retrying a transient VSS failure means restarting the
/// whole sequence, never resuming mid-sequence.
///
/// # Errors
/// Returns the last attempt's [`VssRequestError`] if every attempt
/// failed, or immediately on the first attempt that fails with a
/// non-retryable `HRESULT`.
fn create_snapshot_with_retry(
    volume_path: &str,
) -> Result<(VssSnapshotSession, SnapshotDescriptor), VssRequestError> {
    let mut backoff = RETRY_BACKOFF_BASE;
    let mut attempt = 1_u32;
    loop {
        match VssSnapshotSession::create(volume_path) {
            Ok(created) => return Ok(created),
            Err(err)
                if attempt < MAX_SNAPSHOT_ATTEMPTS
                    && uffs_broker_protocol::vss_hresult::is_retryable(err.hresult) =>
            {
                debug_log(&format!(
                    "snapshot creation attempt {attempt}/{MAX_SNAPSHOT_ATTEMPTS} failed with \
                     retryable hresult={:#x} (stage={}); retrying in {backoff:?}",
                    err.hresult, err.stage
                ));
                std::thread::sleep(backoff);
                backoff *= 2;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

/// An event the main loop reacts to — a decoded command from the
/// Broker, the pipe closing, or the parent process dying (a second,
/// independent safety net alongside the Job Object the Broker assigns
/// this process to).
#[derive(Debug)]
enum MainEvent {
    /// A decoded command arrived from the Broker.
    Command(BrokerCommand),
    /// The control pipe closed (EOF) or a read failed.
    PipeClosed,
    /// The watched parent process exited.
    ParentDied,
}

/// Env var gating [`debug_log`] — unset by default. A real deployment
/// spawns one helper per content-scan job; without this gate, millions
/// of runs over time would grow an unbounded log file. Set to any value
/// (e.g. `UFFS_VSS_DEBUG_LOG=1`) while troubleshooting; the Broker
/// inherits its environment to this helper automatically (`spawn_helper`
/// passes `None` for `CreateProcessW`'s environment block), so setting
/// it on the Broker process before it spawns a helper is enough.
const DEBUG_LOG_ENV_VAR: &str = "UFFS_VSS_DEBUG_LOG";

/// Once the debug log exceeds this size, it's truncated before the next
/// append — a safety net in case [`DEBUG_LOG_ENV_VAR`] is left set for
/// an extended troubleshooting session rather than a single repro.
const DEBUG_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Append a timestamped line to `%TEMP%\uffs-vss-requestor-debug.log`,
/// silently doing nothing on failure or when [`DEBUG_LOG_ENV_VAR`]
/// isn't set.
///
/// Exists purely for troubleshooting: the Broker's own tracing has
/// visibility only up to "helper connected"/"waiting for Released
/// confirmation" — nothing inside this process. Never load-bearing;
/// this is diagnostic infrastructure for a hang that survived multiple
/// hypotheses (command-line quoting, `DeleteSnapshots` vs. auto-release,
/// stuck VSS writers) before finding the real one, not a permanent
/// feature that runs unconditionally.
pub(crate) fn debug_log(message: &str) {
    use std::io::Write as _;

    if std::env::var_os(DEBUG_LOG_ENV_VAR).is_none() {
        return;
    }
    let path = std::env::temp_dir().join("uffs-vss-requestor-debug.log");
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > DEBUG_LOG_MAX_BYTES) {
        drop(std::fs::remove_file(&path));
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let millis_since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let _write_result = writeln!(file, "[{millis_since_epoch}] {message}");
}

/// Run the helper end to end.
///
/// # Errors
/// Returns an error if arguments are invalid, the pipe can't be
/// connected, or the initial snapshot creation fails (after reporting
/// [`HelperEvent::Failed`] to the Broker).
pub(crate) fn run() -> anyhow::Result<()> {
    debug_log("run() started");
    let args = Args::parse()?;
    let mut writer = pipe::connect(&args.pipe_name)?;
    debug_log("connected to control pipe");
    let reader_file = writer
        .try_clone()
        .map_err(|err| anyhow::anyhow!("failed to clone pipe handle for reading: {err}"))?;

    let session = match create_snapshot_with_retry(&args.volume_path) {
        Ok((session, descriptor)) => {
            debug_log("snapshot created; writing Ready event");
            protocol::write_event(&mut writer, &HelperEvent::Ready {
                snapshot_set_id: descriptor.snapshot_set_id,
                snapshot_id: descriptor.snapshot_id,
                provider_id: descriptor.provider_id,
                original_volume_name: descriptor.original_volume_name,
                snapshot_device_object: descriptor.snapshot_device_object,
                created_at_unix_ms: descriptor.created_at_unix_ms,
            })?;
            session
        }
        Err(err) => {
            let stage = err.stage;
            let hresult = err.hresult;
            // A permanent failure with a known remedy (e.g. the volume's
            // shadow-copy limit) carries it, so the Broker's caller can act
            // instead of seeing a generic failure.
            let message = match uffs_broker_protocol::vss_hresult::failure_hint(hresult) {
                Some(hint) => format!("{} — {hint}", err.message),
                None => err.message,
            };
            protocol::write_event(&mut writer, &HelperEvent::Failed {
                stage,
                hresult,
                message,
            })?;
            anyhow::bail!("snapshot creation failed: stage={stage} hresult={hresult:#x}");
        }
    };

    let (event_tx, event_rx) = mpsc::channel::<MainEvent>();

    let mut ping_writer = writer
        .try_clone()
        .map_err(|err| anyhow::anyhow!("failed to clone pipe handle for Ping replies: {err}"))?;
    let reader_tx = event_tx.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader_file);
        loop {
            let Ok(Some(command)) = protocol::read_command(&mut reader) else {
                drop(reader_tx.send(MainEvent::PipeClosed));
                return;
            };
            if matches!(command, BrokerCommand::Ping) {
                // Handled entirely on this thread — Ping/Pong needs no
                // session access, and replying here (rather than
                // routing through the main thread, which would write on
                // a *different* clone of this same non-overlapped pipe
                // handle) avoids ever having two threads perform I/O on
                // it at once. See below for why that matters: it was a
                // real, 100%-reproducible hang on real hardware.
                debug_log("received Ping; writing Pong");
                drop(protocol::write_event(&mut ping_writer, &HelperEvent::Pong));
                debug_log("wrote Pong");
                continue;
            }
            // Only `Release`/`Cancel` reach here, and both are
            // terminal: the main thread is about to tear down the
            // session and write a final reply on its own clone of this
            // same pipe handle. Windows serializes synchronous I/O
            // across duplicate handles of one file object from
            // different threads, so leaving this thread's read pending
            // past this point would deadlock that final write against
            // a read that will never be satisfied — the Broker never
            // sends anything after Release/Cancel. Proved via
            // `debug_log`: the write's `writeln!` call never returned
            // while this thread's next read sat pending, independent of
            // VSS/COM, AV, or VSS writer health, all ruled out first.
            drop(reader_tx.send(MainEvent::Command(command)));
            return;
        }
    });

    let watchdog_tx = event_tx.clone();
    let parent_pid = args.parent_pid;
    std::thread::spawn(move || {
        wait_for_process_exit(parent_pid);
        drop(watchdog_tx.send(MainEvent::ParentDied));
    });
    drop(event_tx);

    debug_log("entering main event loop");
    for event in event_rx {
        debug_log(&format!("received event: {event:?}"));
        match event {
            MainEvent::Command(BrokerCommand::Ping) => {
                drop(protocol::write_event(&mut writer, &HelperEvent::Pong));
            }
            MainEvent::Command(BrokerCommand::Release | BrokerCommand::Cancel)
            | MainEvent::PipeClosed
            | MainEvent::ParentDied => {
                // `VSS_CTX_FILE_SHARE_BACKUP` is an auto-release context:
                // dropping `session` releases the last
                // `IVssBackupComponents` reference, which is where the
                // actual deletion happens. This used to be a separate
                // path (explicit `DeleteSnapshots` on `Release`, drop-only
                // on every other exit) — `DeleteSnapshots` was observed
                // to hang indefinitely on real hardware when called on a
                // `VSS_CTX_FILE_SHARE_BACKUP` snapshot set, so `Release`
                // now uses the exact same drop-based teardown the other
                // three paths already relied on.
                let is_release = matches!(event, MainEvent::Command(BrokerCommand::Release));
                debug_log("dropping session (releases IVssBackupComponents)");
                drop(session);
                debug_log("session dropped");
                if is_release {
                    debug_log("writing Released event");
                    drop(protocol::write_event(&mut writer, &HelperEvent::Released));
                    debug_log("wrote Released event");
                }
                debug_log("run() returning Ok");
                return Ok(());
            }
        }
    }

    drop(session);
    Ok(())
}

/// Block until the process identified by `pid` exits, or return
/// immediately if it can't be opened (already gone, or never existed —
/// either way, "wait for it to die" is trivially satisfied).
#[expect(
    unsafe_code,
    reason = "OpenProcess, WaitForSingleObject, and CloseHandle are FFI calls"
)]
fn wait_for_process_exit(pid: u32) {
    // SAFETY: `pid` is a plain integer; a failed open (process already
    // gone) is handled by returning immediately, never dereferenced.
    let handle_result = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    };
    let Ok(handle) = handle_result else {
        return;
    };
    // SAFETY: `handle` is the valid handle just opened above; waiting
    // indefinitely is intentional — this whole thread's job is to block
    // until the parent exits.
    let _wait_result = unsafe { WaitForSingleObject(handle, u32::MAX) };
    close_handle(handle);
}

/// Close `handle`, logging nothing on failure (this is best-effort
/// cleanup in a thread that's about to signal process exit anyway).
#[expect(unsafe_code, reason = "CloseHandle is an FFI call")]
fn close_handle(handle: HANDLE) {
    // SAFETY: `handle` was opened by this module and is not used again
    // after this call.
    drop(unsafe { CloseHandle(handle) });
}
