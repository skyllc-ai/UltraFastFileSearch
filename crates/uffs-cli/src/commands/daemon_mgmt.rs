// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs --daemon` subcommand dispatch + the mutating handlers
//! (start/stop/kill/restart) and their elevation gate. The read-only
//! status/stats displays live in the sibling
//! [`crate::commands::daemon_status`].

use anyhow::{Context as _, Result};
use uffs_client::connect_sync::UffsClientSync;
use uffs_client::daemon_ctl::{pid_file_path, socket_path};
use uffs_client::protocol::response::DaemonStatus;

use crate::args::{self, DaemonAction};
use crate::commands::{daemon_load, daemon_status, daemon_tiering};

/// Handle `uffs --daemon <action> [flags...]`.
///
/// # Errors
///
/// Returns an error for an unparseable action or when the action's
/// handler fails.
pub(crate) fn run_daemon(args: &[String]) -> Result<()> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        args::print_daemon_help();
        return Ok(());
    }
    let action = args::parse_daemon_action(args)?;
    daemon(&action)
}

/// Suppress the user-facing progress prints of the daemon handlers while an
/// internal flow (the uninstall's background drive-coverage reload) runs them
/// behind a spinner. Read by the print sites in `daemon_start` / `daemon_kill`;
/// set only by [`daemon_quiet`] (RAII-reset, so it never sticks past that
/// call).
static QUIET: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// True while [`daemon_quiet`] is executing.
fn is_quiet() -> bool {
    QUIET.load(core::sync::atomic::Ordering::Relaxed)
}

/// RAII reset for the [`QUIET`] flag, so an early return or panic inside the
/// handler can never leave later daemon commands silenced.
struct QuietGuard;

impl Drop for QuietGuard {
    fn drop(&mut self) {
        QUIET.store(false, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Run [`daemon`] with its user-facing progress prints suppressed — the same
/// handlers behind the same elevation gate, just silent. For internal flows
/// that reload the daemon in the background behind a spinner (the uninstall
/// deep-sweep coverage), where live "Starting daemon..." lines would garble an
/// interactive prompt on the main thread.
///
/// # Errors
///
/// Exactly [`daemon`]'s errors.
pub(crate) fn daemon_quiet(action: &DaemonAction) -> Result<()> {
    QUIET.store(true, core::sync::atomic::Ordering::Relaxed);
    let _guard = QuietGuard;
    daemon(action)
}

/// Execute a daemon management action.
///
/// Every command that mutates daemon state (stop, kill, restart, load,
/// preload, hibernate, forget) requires an elevated shell:
/// - **Windows**: Administrator (UAC-elevated) token.
/// - **Unix** (Linux, macOS, …): effective user ID 0 (root / sudo).
///
/// `uffsd` runs with elevated privileges to read raw filesystem data;
/// a non-privileged caller must not stop or restart it — doing so would
/// kill the running daemon with no safe path to bring it back.
///
/// Read-only queries (`status`, `stats`, `status_drives`) are always
/// permitted without elevation.  `daemon start --elevate` is also
/// permitted on Windows; it opts in to an explicit UAC prompt.  On Windows,
/// mutating commands are also permitted un-elevated while the Access Broker is
/// serving (the daemon then runs non-elevated and is safely restartable).
///
/// # Errors
///
/// Returns an error if the operation fails, or if a mutating command is
/// attempted from a non-elevated shell.
pub(crate) fn daemon(action: &DaemonAction) -> Result<()> {
    // Elevation gate — checked here, once, before any action is dispatched,
    // so no individual subcommand handler can accidentally bypass it.
    {
        // `Resident` is exempt: it manages a *per-user* login item
        // (HKCU Run key / LaunchAgent / systemd user unit) and never
        // touches a running daemon, so it must work without elevation —
        // that is the whole zero-UAC residency story.
        let is_read_only_or_uac_start = matches!(
            action,
            DaemonAction::Status { .. }
                | DaemonAction::StatusDrives
                | DaemonAction::ChangedSince { .. }
                | DaemonAction::Resident { .. }
                | DaemonAction::Start { elevate: true, .. }
        );
        if !is_read_only_or_uac_start
            && !uffs_mft::is_elevated()
            && mutating_management_needs_elevation()
        {
            #[cfg(windows)]
            anyhow::bail!(
                "Daemon management commands require an elevated (Administrator) shell.\n\n\
                 uffsd runs with admin privileges to read the NTFS Master File Table.\n\
                 A non-elevated process must not stop or restart it — doing so would\n\
                 kill the running daemon with no way to bring it back.\n\n\
                 To run this command, pick one:\n\
                 \x20 1. Relaunch PowerShell / cmd as Administrator\n\
                 \x20    (right-click \u{2192} \"Run as administrator\"), then retry.\n\
                 \x20 2. For `daemon start`, add --elevate to get a UAC prompt:\n\
                 \x20      uffs --daemon start --elevate\n\
                 \x20 3. Install the broker service (one-time setup, no future UAC):\n\
                 \x20      uffs-broker --install"
            );
            #[cfg(unix)]
            anyhow::bail!(
                "Daemon management commands require root privileges.\n\n\
                 uffsd runs as root to read raw filesystem data.\n\
                 A non-root process must not stop or restart it — doing so would\n\
                 kill the running daemon with no way to bring it back.\n\n\
                 To run this command, prefix it with sudo:\n\
                 \x20  sudo uffs --daemon <subcommand>"
            );
            // Fallback for platforms that are neither Windows nor Unix
            // (e.g. WASM, bare-metal targets — should not arise in practice).
            #[cfg(not(any(windows, unix)))]
            anyhow::bail!(
                "Daemon management commands require elevated privileges.\n\
                 Please run this command as a privileged user."
            );
        }
    }

    match action {
        DaemonAction::Start {
            mft_file,
            data_dir,
            drives,
            no_cache,
            log_level,
            log_file,
            elevate,
        } => daemon_start(
            mft_file,
            data_dir.as_deref(),
            drives,
            *no_cache,
            log_level,
            log_file.as_deref(),
            *elevate,
        ),
        DaemonAction::Status { verbose, json } => daemon_status::daemon_status(*verbose, *json),
        DaemonAction::Stop => daemon_stop(),
        DaemonAction::Kill => daemon_kill(),
        DaemonAction::Restart => daemon_restart(),
        DaemonAction::Load {
            mft_file,
            data_dir,
            drives,
            no_cache,
        } => daemon_load::daemon_load(mft_file, data_dir.as_deref(), drives, *no_cache),
        DaemonAction::Hibernate { drives } => daemon_tiering::daemon_hibernate(drives),
        DaemonAction::Preload {
            drives,
            pin_minutes,
        } => daemon_tiering::daemon_preload(drives, *pin_minutes),
        DaemonAction::Forget { drives, force } => daemon_tiering::daemon_forget(drives, *force),
        DaemonAction::Resident {
            mode,
            mft_file,
            data_dir,
            drives,
        } => super::resident::resident(*mode, mft_file, data_dir.as_deref(), drives),
        DaemonAction::StatusDrives => daemon_tiering::daemon_status_drives(),
        DaemonAction::ChangedSince {
            drive,
            journal_id,
            since_usn,
            max_records,
        } => super::daemon_journal::daemon_changed_since(
            *drive,
            *journal_id,
            *since_usn,
            *max_records,
        ),
    }
}

/// Whether a *mutating* daemon-management action (stop/kill/restart/load/…)
/// needs elevation, **given the running daemon's actual owner**.
///
/// The gate exists so a non-privileged caller cannot stop or restart a daemon
/// it could not bring back. On **Unix** that is only true when the running
/// daemon is owned by a *different* (typically root) user: a daemon we own —
/// same effective uid — is ours to stop and restart, so no `sudo` is needed
/// (the common macOS/Linux offline-capture workflow, and what `uffs --update
/// apply` relies on). The daemon writes its PID file, so the file's owner is
/// the daemon's uid; an unreadable/absent PID file means there is no daemon to
/// protect → no elevation required.
///
/// (Windows uses a broker-aware variant — see the `#[cfg(windows)]` impl
/// below.)
#[cfg(unix)]
pub(crate) fn mutating_management_needs_elevation() -> bool {
    daemon_owner_needs_elevation(&pid_file_path(), uffs_mft::current_euid())
}

/// Pure core of [`mutating_management_needs_elevation`] (Unix): elevation is
/// required iff the daemon's PID file exists and is owned by a uid other than
/// `caller_euid`. An absent/unreadable PID file → no daemon to protect → no
/// elevation. Split out so the owner comparison is unit-testable without a
/// live daemon.
#[cfg(unix)]
fn daemon_owner_needs_elevation(pid_file: &std::path::Path, caller_euid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    std::fs::metadata(pid_file).is_ok_and(|meta| meta.uid() != caller_euid)
}

/// Windows: mirror the Unix PID-owner gate as closely as the platform allows.
/// No elevation is needed when (in order):
///
/// 1. **No daemon to protect** — the PID file is absent, so stop/kill/restart
///    cannot break anything a non-elevated caller could not bring back.
/// 2. **The daemon itself runs non-elevated** — its launch-state sidecar
///    (`daemon.state.json`, written into the *caller's own* `%LOCALAPPDATA%`,
///    so it is this user's daemon by construction) records `"elevated": false`;
///    a same-user, non-elevated process is killable and restartable without
///    admin.
/// 3. **The Access Broker pipe is serving** — a restart adopts broker handles,
///    so a non-elevated caller can stop AND bring the daemon back (no UAC).
///
/// Otherwise (an elevated daemon, no broker) managing it needs Administrator.
#[cfg(windows)]
pub(crate) fn mutating_management_needs_elevation() -> bool {
    /// Short pipe probe — this gate runs once per management command.
    const BROKER_GATE_PROBE_MS: u32 = 600;

    let pid_path = pid_file_path();
    if !pid_path.exists() {
        return false;
    }
    if launch_state_says_non_elevated(&pid_path) {
        return false;
    }
    !uffs_winsvc::pipe_serving(uffs_broker_protocol::PIPE_NAME, BROKER_GATE_PROBE_MS)
}

/// Whether the daemon's launch-state sidecar (next to the PID file) records a
/// **non-elevated** launch. Absent file, unreadable JSON, or a pre-flag state
/// file all return `false` — the gate then falls back to the broker probe
/// (conservative: never *grants* user-level management on missing evidence).
#[cfg(windows)]
fn launch_state_says_non_elevated(pid_path: &std::path::Path) -> bool {
    let state_path = pid_path.with_file_name("daemon.state.json");
    let Ok(raw) = std::fs::read_to_string(&state_path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|state| state.get("elevated").and_then(serde_json::Value::as_bool))
        .is_some_and(|elevated| !elevated)
}

/// Other non-Unix targets (WASM, bare-metal — not real deployments): keep the
/// conservative default of always requiring elevation.
#[cfg(not(any(unix, windows)))]
pub(crate) const fn mutating_management_needs_elevation() -> bool {
    true
}

/// `uffs --daemon start` — start the daemon, forwarding data-source flags
/// as-is so the daemon resolves them internally (DRY).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
#[expect(
    clippy::use_debug,
    reason = "[diag] spawn-chain dump — gated behind --log-level debug/trace"
)]
fn daemon_start(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    drives: &[uffs_mft::platform::DriveLetter],
    no_cache: bool,
    log_level: &str,
    log_file: Option<&std::path::Path>,
    elevate: bool,
) -> Result<()> {
    // Already running?
    if UffsClientSync::connect_raw().is_ok() {
        if !is_quiet() {
            println!("Daemon is already running. Use `uffs --daemon restart` to reload.");
        }
        return Ok(());
    }

    // Build spawn args — forward raw, let daemon handle discovery.
    // Use `OsString` so non-UTF-8 / WTF-8 paths survive losslessly to the
    // spawned daemon's argv (WI-4.2).
    let mut spawn_args: Vec<std::ffi::OsString> = Vec::new();
    if let Some(dir) = data_dir {
        spawn_args.push(std::ffi::OsString::from("--data-dir"));
        spawn_args.push(dir.as_os_str().to_os_string());
    }
    for mft_path in mft_files {
        spawn_args.push(std::ffi::OsString::from("--mft-file"));
        spawn_args.push(mft_path.as_os_str().to_os_string());
    }
    for letter in drives {
        spawn_args.push(std::ffi::OsString::from("--drive"));
        spawn_args.push(std::ffi::OsString::from(letter.to_string()));
    }
    if no_cache {
        spawn_args.push(std::ffi::OsString::from("--no-cache"));
    }

    // ── Env-var forwarding ────────────────────────────────────────────────
    // The spawned daemon is a detached background process.  On Windows it is
    // often elevated via ShellExecuteW("runas"), which starts a new session
    // and does NOT reliably inherit the parent PowerShell's env block.
    // We therefore bake RUST_LOG / UFFS_LOG / UFFS_LOG_DIR into argv so the
    // daemon always receives them regardless of how it is elevated.

    // Probe env vars (read once so we can print them before forwarding).
    //
    // IMPORTANT: PowerShell (and other shells in long-lived sessions) often
    // leave variables set to the EMPTY STRING after a script unsets them —
    // `std::env::var("X")` then returns `Ok("")`, not `Err(NotPresent)`.
    // Treating an empty-string env var as a real value is what caused the
    // `--log-level "" --log-file uffsd.log` silent-failure regression: uffsd
    // received an empty EnvFilter (dropping all logs) and a relative log
    // path whose parent `""` tripped tracing_appender's `.expect(...)` panic.
    // So normalise `Some("")` to `None` at the source via `non_empty_env`.
    let env_rust_log = non_empty_env(std::env::var("RUST_LOG").ok());
    let env_uffs_log = non_empty_env(std::env::var("UFFS_LOG").ok());
    let env_uffs_log_dir = non_empty_env(std::env::var("UFFS_LOG_DIR").ok());

    // Effective log level: CLI arg wins; fall back to UFFS_LOG then RUST_LOG.
    let effective_log_level: String = if log_level == "info" {
        env_uffs_log
            .clone()
            .or_else(|| env_rust_log.clone())
            .unwrap_or_else(|| log_level.to_owned())
    } else {
        log_level.to_owned()
    };
    if effective_log_level != "info" {
        spawn_args.push(std::ffi::OsString::from("--log-level"));
        spawn_args.push(std::ffi::OsString::from(effective_log_level.clone()));
    }

    // Effective log file: CLI arg wins; fall back to $UFFS_LOG_DIR/uffsd.log.
    // The `non_empty` filter above guarantees `env_uffs_log_dir` is a real,
    // non-empty path — otherwise `PathBuf::from("").join("uffsd.log")` would
    // produce a relative `uffsd.log`, which in turn breaks the detached
    // daemon's file appender (empty parent dir → create_dir_all fails →
    // rolling-appender panics at startup, uffsd dies before binding IPC).
    let derived_log_file = env_uffs_log_dir
        .as_deref()
        .map(|dir| std::path::PathBuf::from(dir).join("uffsd.log"));
    let effective_log_file = log_file
        .map(std::path::Path::to_path_buf)
        .or(derived_log_file);
    if let Some(path) = &effective_log_file {
        spawn_args.push(std::ffi::OsString::from("--log-file"));
        spawn_args.push(path.as_os_str().to_os_string());
    }

    // [diag] Spawn-chain dump for tracing elevation/env-forwarding issues.
    // Gated behind an explicit debug/trace log level: on the default
    // `daemon start` happy path users see clean output, not internals
    // (2026-06-12 fresh-VM dry run flagged the unconditional version as
    // looking like leftover debug logging). Also silenced in quiet mode —
    // a background daemon reload must never print over an interactive
    // prompt or spinner (observed with UFFS_LOG=debug set).
    if matches!(effective_log_level.as_str(), "debug" | "trace") && !is_quiet() {
        println!(
            "[diag] daemon_start: drives={drives:?}  log_level={log_level:?}  log_file={log_file:?}"
        );
        println!("[diag] env  RUST_LOG    = {env_rust_log:?}");
        println!("[diag] env  UFFS_LOG    = {env_uffs_log:?}");
        println!("[diag] env  UFFS_LOG_DIR= {env_uffs_log_dir:?}");
        println!("[diag] eff  log_level   = {effective_log_level:?}");
        println!("[diag] eff  log_file    = {effective_log_file:?}");
        println!("[diag] full spawn_args  = {spawn_args:?}");
    }

    if !cfg!(windows) && spawn_args.is_empty() {
        anyhow::bail!(
            "No MFT data sources specified.\n\
             Provide --mft-file <path> or --data-dir <path>."
        );
    }

    // An explicit OPERATOR start revokes any earlier stop intent, so the
    // watchdog resumes supervising this service.
    //
    // A supervisor-driven restart must NOT clear it: the watchdog
    // respawns by invoking this very command, so clearing here would let
    // it erase the marker it is supposed to obey — the intent survives
    // exactly one tick and the service bounces back anyway. The watchdog
    // sets `UFFS_SUPERVISED_RESTART` to say "this start is mine".
    if std::env::var_os("UFFS_SUPERVISED_RESTART").is_none() {
        uffs_client::daemon_ctl::clear_stop_intent(uffs_client::daemon_ctl::ServiceKind::Daemon);
    }
    if !is_quiet() {
        println!("Starting daemon...");
    }

    // `--elevate` (or UFFS_ELEVATE=1) opts in to a UAC prompt on Windows
    // when the current shell is not elevated.  The default path refuses
    // to trigger UAC silently and returns DaemonNeedsElevation, which
    // `main.rs` formats into an actionable multi-option help message.
    let mut client = if elevate {
        UffsClientSync::connect_with_elevation(&spawn_args)
            .with_context(|| "Failed to start daemon (with elevation)")?
    } else {
        UffsClientSync::connect_with_args(&spawn_args).with_context(|| "Failed to start daemon")?
    };

    client
        .await_ready(core::time::Duration::from_mins(2))
        .with_context(|| "Daemon did not become ready in time")?;

    if !is_quiet() {
        println!("Daemon started and ready.");
    }
    Ok(())
}

/// `uffs --daemon stop` — graceful shutdown via RPC.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_stop() -> Result<()> {
    if let Ok(mut client) = UffsClientSync::connect_raw() {
        // Record the intent BEFORE the RPC, not after.
        //
        // `shutdown()` blocks until the daemon is actually gone, and on a
        // large index (24.9 M records, seven journal loops) that teardown
        // takes seconds. Writing the marker afterwards leaves a window in
        // which the daemon is already dead and the marker does not exist
        // yet — a watchdog tick landing there sees an unexplained death
        // and dutifully respawns it, so a deliberate stop bounces back.
        // Observed exactly that on a live box before this ordering fix.
        uffs_client::daemon_ctl::record_stop_intent(uffs_client::daemon_ctl::ServiceKind::Daemon);
        if let Err(err) = client
            .shutdown()
            .with_context(|| "Shutdown RPC failed — try `uffs --daemon kill` instead")
        {
            // The daemon is still up: an intent we never carried out must
            // not keep the watchdog from reviving a later genuine crash.
            uffs_client::daemon_ctl::clear_stop_intent(
                uffs_client::daemon_ctl::ServiceKind::Daemon,
            );
            return Err(err);
        }
        println!("Daemon shutdown requested.");
    } else {
        println!("Daemon is not running.");
    }
    Ok(())
}

/// `uffs --daemon kill` — hard kill via PID file or socket discovery + cleanup.
///
/// The PID file / socket are removed **only after** the process is confirmed
/// gone. If the terminate did not take effect — almost always because the
/// daemon was started elevated and this is a non-elevated shell — the PID file
/// is left in place (so the daemon stays discoverable) and an error explains
/// how to finish. This closes the old "PID file deleted, process still
/// running" half-kill.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_kill() -> Result<()> {
    // A kill is as deliberate as a stop — same reasoning, same ordering:
    // record before the process actually dies.
    uffs_client::daemon_ctl::record_stop_intent(uffs_client::daemon_ctl::ServiceKind::Daemon);
    let pid_path = pid_file_path();

    let mut pid =
        uffs_client::daemon_ctl::parse_pid_file(&pid_path).map(|(file_pid, _, _, _)| file_pid);

    // No PID file → try discovering via live socket.
    if pid.is_none()
        && let Ok(mut client) = UffsClientSync::connect_raw()
        && let Ok(status) = client.status()
    {
        pid = Some(status.pid);
    }

    let Some(target_pid) = pid else {
        // Nothing running → sweep any stale files (no-op if already absent).
        drop(std::fs::remove_file(&pid_path));
        drop(std::fs::remove_file(socket_path()));
        if !is_quiet() {
            println!("No daemon found (no PID file, no socket connection).");
        }
        return Ok(());
    };

    // A PID file that outlived its process (crash / reboot / recycled PID): the
    // daemon is already gone, so there is nothing to kill — just clean up. This
    // avoids reporting a "kill" when nothing was running.
    if !uffs_client::daemon_ctl::is_pid_alive(target_pid) {
        drop(std::fs::remove_file(&pid_path));
        drop(std::fs::remove_file(socket_path()));
        if !is_quiet() {
            println!("No running daemon (stale PID {target_pid}); cleaned up PID file + socket.");
        }
        return Ok(());
    }

    if !is_quiet() {
        println!("Killing daemon (PID {target_pid})...");
    }

    if kill_pid(target_pid) {
        // Confirmed gone — safe to drop the discovery files.
        drop(std::fs::remove_file(&pid_path));
        drop(std::fs::remove_file(socket_path()));
        if !is_quiet() {
            println!("Daemon killed. PID file and socket cleaned up.");
        }
        return Ok(());
    }

    // Still alive → do NOT remove the PID file (no half-kill; keep it
    // discoverable). Almost always: an elevated daemon vs a non-elevated shell.
    #[cfg(windows)]
    anyhow::bail!(
        "Could not terminate the daemon (PID {target_pid}) — it is still running.\n\n\
         It was most likely started from an elevated (Administrator) terminal, so a\n\
         non-elevated process cannot stop it. The PID file was left in place so the\n\
         daemon stays discoverable.\n\n\
         Re-run from an elevated terminal:\n\
         \x20  uffs --daemon kill"
    );
    #[cfg(not(windows))]
    anyhow::bail!(
        "Could not terminate the daemon (PID {target_pid}) — it is still running.\n\n\
         It was most likely started as root, so a non-root process cannot stop it.\n\
         The PID file was left in place so the daemon stays discoverable.\n\n\
         Re-run with elevated privileges:\n\
         \x20  sudo uffs --daemon kill"
    );
}

/// Terminate `pid` (SIGKILL on Unix, `taskkill /F` on Windows), then
/// **confirm** it is actually gone. Returns `true` only when the process is no
/// longer alive.
///
/// The OS call's exit code is unreliable for our case — a non-elevated
/// `taskkill` of an elevated daemon is denied — so we verify via
/// [`uffs_client::daemon_ctl::is_pid_alive`], polling briefly (a `/F` kill is
/// near-instant but not synchronous).
fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    drop(
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output(),
    );
    #[cfg(windows)]
    drop(
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output(),
    );

    // Poll for the process to disappear (fast path returns on the first check).
    for _ in 0..10_u8 {
        if !uffs_client::daemon_ctl::is_pid_alive(pid) {
            return true;
        }
        std::thread::sleep(core::time::Duration::from_millis(100));
    }
    false
}

/// `uffs --daemon restart` — stop, capture data sources, then re-launch.
///
/// If the daemon is running, queries its loaded drives to extract the
/// original `--mft-file` paths, stops it, then re-spawns with the same
/// arguments.  If not running, prints a message and exits.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_restart() -> Result<()> {
    let spawn_args = if let Ok(mut client) = UffsClientSync::connect_raw() {
        let drives_resp = client
            .drives()
            .with_context(|| "Failed to query drives before restart")?;

        let mut args: Vec<std::ffi::OsString> = Vec::new();
        for dr in &drives_resp.drives {
            if let Some(path) = dr.source.strip_prefix("file:") {
                args.push(std::ffi::OsString::from("--mft-file"));
                args.push(std::ffi::OsString::from(path));
            }
        }

        let daemon_pid = client.status().map_or(0, |status_resp| status_resp.pid);
        println!("Stopping daemon (PID {daemon_pid})...");

        client.shutdown().with_context(|| {
            format!(
                "Graceful shutdown of PID {daemon_pid} failed.\n\
                 Run `uffs --daemon kill` first, then retry."
            )
        })?;

        std::thread::sleep(core::time::Duration::from_secs(1));
        args
    } else {
        println!("Daemon is not running — nothing to restart.");
        return Ok(());
    };

    drop(std::fs::remove_file(pid_file_path()));
    drop(std::fs::remove_file(socket_path()));

    println!(
        "Restarting daemon with {} data source(s)...",
        spawn_args
            .iter()
            .filter(|arg| arg.as_os_str() == "--mft-file" || arg.as_os_str() == "--data-dir")
            .count()
    );

    let mut client = UffsClientSync::connect_with_args(&spawn_args)
        .with_context(|| "Failed to start restarted daemon")?;

    client
        .await_ready(core::time::Duration::from_mins(2))
        .with_context(|| "Restarted daemon did not become ready in time")?;

    let status = client.status();
    if let Ok(resp) = status {
        let state = match &resp.status {
            DaemonStatus::Loading {
                drives_loaded,
                drives_total,
            } => format!("Loading ({drives_loaded}/{drives_total} drives)"),
            DaemonStatus::Ready => "Ready".to_owned(),
            DaemonStatus::Refreshing { .. } => "Refreshing".to_owned(),
        };
        println!("Daemon restarted (PID {}), status: {state}", resp.pid);
    } else {
        println!("Daemon restarted.");
    }

    Ok(())
}

/// Normalise an env-var probe so `Some("")` becomes `None`.
///
/// `std::env::var("X")` returns `Ok("")` when a shell has left `X` set to the
/// empty string (common in PowerShell after a sub-script unsets a variable
/// via assignment rather than `Remove-Item Env:\X`).  Treating that as a real
/// value is what caused the silent `uffs --daemon start` failure documented in
/// `LOG/Output`: the CLI forwarded `--log-level ""` and
/// `--log-file uffsd.log` (relative path, from `""+"/uffsd.log"`) to uffsd,
/// uffsd's `tracing_appender::rolling::never("", "uffsd.log")` then panicked
/// via `.expect("initializing rolling file appender failed")`, the panic
/// hook called `process::exit(101)` before IPC could bind, and the client
/// timed out after 20 retries with no diagnostic signal.
#[must_use]
fn non_empty_env(value: Option<String>) -> Option<String> {
    value.filter(|val| !val.is_empty())
}

#[cfg(test)]
mod tests {
    use super::non_empty_env;

    /// Missing env var → `None` flows through unchanged.
    #[test]
    fn non_empty_env_passes_none_through() {
        assert_eq!(non_empty_env(None), None);
    }

    /// **Regression (silent-start bug, `LOG/Output`):** PowerShell leaving
    /// `RUST_LOG=""` / `UFFS_LOG_DIR=""` set to the empty string must be
    /// treated exactly like "unset".  Before this fix, the CLI forwarded the
    /// empty string to uffsd as `--log-level ""` / `--log-file uffsd.log`,
    /// uffsd panicked in the tracing appender, and the client spun through
    /// 20 retries with no diagnostic signal.
    #[test]
    fn non_empty_env_collapses_empty_string_to_none() {
        assert_eq!(non_empty_env(Some(String::new())), None);
    }

    /// A legitimate non-empty value is preserved verbatim — the filter must
    /// not accidentally strip real log levels or directory paths.
    #[test]
    fn non_empty_env_preserves_real_values() {
        assert_eq!(
            non_empty_env(Some("debug".to_owned())),
            Some("debug".to_owned())
        );
        assert_eq!(
            non_empty_env(Some(r"C:\Users\rnio\bin".to_owned())),
            Some(r"C:\Users\rnio\bin".to_owned())
        );
    }

    /// Whitespace-only values are NOT treated as empty.  If someone genuinely
    /// wants `RUST_LOG=" "` we pass it through — our only concern is the
    /// `""` trap created by PowerShell's assignment-to-empty behaviour.
    /// This pins the contract so a future refactor doesn't over-trim.
    #[test]
    fn non_empty_env_keeps_whitespace_only_values() {
        assert_eq!(non_empty_env(Some(" ".to_owned())), Some(" ".to_owned()));
    }

    // ── Privilege-aware daemon-management gate (Unix) ────────────────
    //
    // A daemon we own (same uid) is ours to stop/restart, so no `sudo` is
    // needed — this is what unblocks `uffs --update apply` against a
    // user-owned offline daemon on macOS/Linux.

    #[cfg(unix)]
    #[test]
    fn own_daemon_pid_file_needs_no_elevation() {
        use std::os::unix::fs::MetadataExt as _;

        // A file this test created is owned by the test's own euid; managing a
        // daemon we own must NOT require elevation.
        let dir = std::env::temp_dir().join(format!("uffs-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pid_file = dir.join("daemon.pid");
        std::fs::write(&pid_file, "1234\n").expect("write pid file");

        let our_uid = std::fs::metadata(&pid_file).expect("stat").uid();
        assert!(
            !super::daemon_owner_needs_elevation(&pid_file, our_uid),
            "a daemon owned by the caller must be manageable without sudo"
        );

        // A PID file owned by *root* (uid 0) while we run as non-root DOES
        // require elevation — the daemon is not ours to restart.
        if our_uid != 0 {
            assert!(
                super::daemon_owner_needs_elevation(&pid_file, 0),
                "a root-owned daemon must require elevation for a non-root caller"
            );
        }

        // Best-effort cleanup; bound (not a non-binding `let _`) so the
        // must-use Result is consumed without tripping clippy either way.
        let _cleanup = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn absent_pid_file_needs_no_elevation() {
        // No PID file → no daemon to protect → no elevation required (so
        // `stop`/`start` against a stopped daemon never demands sudo).
        let missing = std::env::temp_dir().join("uffs-gate-does-not-exist-xyz.pid");
        assert!(!super::daemon_owner_needs_elevation(&missing, 4242));
    }
}
