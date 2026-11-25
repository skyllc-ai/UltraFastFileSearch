// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `WinGet` upgrade orchestration for `uffs --update` (the "update flow,
//! winget-delegating").
//!
//! `winget upgrade` on a portable/zip package is an uninstall + reinstall, so
//! it fails with `remove_all: Access is denied` whenever a UFFS process runs
//! FROM the package dir (reproduced live: the daemon and the broker service
//! both ran from the winget root). The user verb stays `uffs --update`; winget
//! hears its own verb internally.
//!
//! Flow (Windows; a no-op elsewhere), quiesce-first so BOTH the hand-rolled
//! `~\bin` update and the winget upgrade run against a stopped install:
//!
//! 1. [`quiesce`] — stop the daemon (shutdown RPC; no privileges) and, when the
//!    broker service runs from the winget package, stop it via ONE elevated
//!    helper held open across the whole update. The helper reports its real
//!    outcome (and the measured stop/start durations) through result files, so
//!    a slow stop is surfaced honestly — never a bogus "UAC declined".
//! 2. The caller runs the hand-rolled update, then [`run_upgrade`] runs `winget
//!    upgrade` (deferred past process exit when the running uffs.exe is itself
//!    part of the package).
//! 3. [`resume`] — release the helper (it restarts the broker) and restart the
//!    daemon.
//!
//! Every wait shows a spinner: on an AV-throttled box service control legit-
//! imately takes a minute-plus (~90s broker start observed), and silent
//! minutes read as a hang.

use anyhow::Result;

use super::model::DetectionReport;

/// The `WinGet` package id UFFS publishes under. Cross-platform: the Windows
/// upgrade path uses it, and [`super::winget_discover`] matches the winget
/// package folder against it on every target.
pub(crate) const WINGET_PACKAGE_ID: &str = "SkyLLC.UFFS";

/// What [`quiesce`] stopped, so [`resume`] can restore exactly that. Opaque +
/// `Default` off Windows (nothing is ever quiesced there).
#[derive(Default)]
pub(crate) struct Quiesce {
    /// The Windows quiesce state (what was stopped); `None` when nothing was
    /// quiesced. Absent off Windows.
    #[cfg(windows)]
    inner: Option<windows_impl::QuiesceState>,
}

/// Whether [`run_upgrade`] ran winget now or deferred it past process exit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeOutcome {
    /// winget ran (or there was nothing to upgrade) — `resume` restores now.
    Ran,
    /// The upgrade was deferred to a post-exit script (the running uffs.exe is
    /// part of the package); that script restores the broker, so `resume` must
    /// NOT act. Only the Windows implementation constructs this.
    #[cfg_attr(
        not(windows),
        expect(dead_code, reason = "constructed only by the Windows implementation")
    )]
    Deferred,
}

/// Stop the daemon + (winget-package) broker before any file replacement.
/// No-op when there is no winget-managed root, or nothing is running from it.
///
/// # Errors
///
/// Propagates a failure to obtain elevation / stop the broker (the upgrade
/// would just fail locked); the caller aborts the winget leg and restores.
#[cfg(windows)]
pub(crate) fn quiesce(report: &DetectionReport) -> Result<Quiesce> {
    Ok(Quiesce {
        inner: windows_impl::quiesce(report)?,
    })
}

/// Non-Windows: nothing to quiesce.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the fallible Windows implementation"
)]
pub(crate) fn quiesce(_report: &DetectionReport) -> Result<Quiesce> {
    Ok(Quiesce::default())
}

/// Run `winget upgrade` for the winget-managed root, if any. No-op when the
/// root is already current or absent.
///
/// # Errors
///
/// Best-effort: winget failures are narrated, not propagated; only setup
/// failures (spawning the deferred script) propagate.
#[cfg(windows)]
pub(crate) fn run_upgrade(
    report: &DetectionReport,
    latest: &str,
    quiesce: &Quiesce,
) -> Result<UpgradeOutcome> {
    windows_impl::run_upgrade(report, latest, quiesce.inner.as_ref())
}

/// Non-Windows: no winget.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    clippy::missing_const_for_fn,
    reason = "signature mirrors the fallible Windows implementation"
)]
pub(crate) fn run_upgrade(
    _report: &DetectionReport,
    _latest: &str,
    _quiesce: &Quiesce,
) -> Result<UpgradeOutcome> {
    Ok(UpgradeOutcome::Ran)
}

/// Restart whatever [`quiesce`] stopped. Skipped when the upgrade was deferred
/// (the post-exit script owns the restore then).
#[cfg(windows)]
pub(crate) fn resume(quiesce: Quiesce, outcome: UpgradeOutcome) {
    if let Some(state) = quiesce.inner {
        windows_impl::resume(state, outcome);
    }
}

/// Non-Windows: nothing was quiesced.
#[cfg(not(windows))]
#[expect(
    clippy::missing_const_for_fn,
    reason = "signature mirrors the stateful Windows implementation"
)]
pub(crate) fn resume(_quiesce: Quiesce, _outcome: UpgradeOutcome) {}

/// Elevated persistent helper behind `uffs --update broker-cycle-helper
/// <base-file>`: stop the broker, report the outcome + measured duration via
/// `<base>.stopped` / `<base>.error`, wait for `<base>.release`, restart the
/// broker, report via `<base>.started` / `<base>.error`, exit. Spawned via one
/// UAC prompt.
///
/// # Errors
///
/// Fails when not elevated. Service failures are written to the result file
/// (so the non-elevated orchestrator can surface them) as well as returned.
#[cfg(windows)]
pub(crate) fn run_broker_cycle_helper(base_file: &str) -> Result<()> {
    windows_impl::run_broker_cycle_helper(base_file)
}

/// Non-Windows stub for the helper entry point.
#[cfg(not(windows))]
pub(crate) fn run_broker_cycle_helper(_base_file: &str) -> Result<()> {
    anyhow::bail!("broker-cycle-helper is Windows-only (there is no broker service here)")
}

#[cfg(windows)]
/// The real Windows implementation (see the module docs); split so the
/// cross-platform facade stays free of Win32-only imports.
mod windows_impl {
    use core::time::Duration;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use anyhow::{Context as _, Result, bail};

    use super::{UpgradeOutcome, WINGET_PACKAGE_ID};
    use crate::commands::spinner::spinner_while;
    use crate::commands::update::model::{Channel, DetectionReport, InstallRoot, Scope};
    use crate::commands::update::strip_verbatim_prefix;

    /// Generous per-service-control ceiling: on an AV-throttled box (Norton /
    /// Defender re-scanning uffs-broker.exe on every access) a stop or start
    /// legitimately takes over a minute (~90s broker start observed) — the 45s
    /// library default was the original "broker did not stop" false failure.
    /// A ceiling only, not a fixed wait: the poll returns the instant the state
    /// is reached, so a fast box pays nothing.
    const SERVICE_TIMEOUT: Duration = Duration::from_secs(300);

    /// How long the orchestrator waits on a helper result file (stop or start)
    /// before giving up — above `SERVICE_TIMEOUT` so the helper's own timeout
    /// fires first with a specific error.
    const RESULT_WAIT: Duration = Duration::from_secs(360);

    /// How long the elevated helper waits for the release file before
    /// restarting anyway (an abandoned update must not leave the broker down).
    const RELEASE_WAIT: Duration = Duration::from_mins(30);

    /// Spinner + result-file poll interval.
    const POLL: Duration = Duration::from_millis(150);

    /// What was stopped, for [`resume`].
    pub(super) struct QuiesceState {
        /// The broker cycle helper (present iff the broker was stopped).
        broker: Option<BrokerHelper>,
        /// Whether the daemon was stopped (so `resume` restarts it).
        daemon_stopped: bool,
    }

    /// Handle to the elevated broker helper, coordinated by result files under
    /// a shared base path in the temp dir.
    struct BrokerHelper {
        /// `<temp>/uffs-broker-cycle-<pid>` — the `.stopped` / `.started` /
        /// `.error` / `.release` files hang off this stem.
        base: PathBuf,
    }

    impl BrokerHelper {
        /// Result file written when the broker stopped OK (carries the
        /// duration).
        fn stopped_file(&self) -> PathBuf {
            self.base.with_extension("stopped")
        }
        /// Result file written when the broker restarted OK (carries the
        /// duration).
        fn started_file(&self) -> PathBuf {
            self.base.with_extension("started")
        }
        /// Result file written on any stop/start failure (carries the message).
        fn error_file(&self) -> PathBuf {
            self.base.with_extension("error")
        }
        /// File the orchestrator writes to release the helper into its restart.
        fn release_file(&self) -> PathBuf {
            self.base.with_extension("release")
        }
        /// Remove any stale result files from a crashed prior run.
        fn clear(&self) {
            for path in [
                self.stopped_file(),
                self.started_file(),
                self.error_file(),
                self.release_file(),
            ] {
                let _cleared: Result<(), std::io::Error> = std::fs::remove_file(path);
            }
        }
    }

    /// See [`super::quiesce`].
    #[expect(clippy::print_stdout, reason = "CLI user-facing narration")]
    pub(super) fn quiesce(report: &DetectionReport) -> Result<Option<QuiesceState>> {
        let Some(root) = winget_root(report) else {
            return Ok(None);
        };
        // winget refuses user-scope changes in an elevated session; there is
        // nothing to quiesce because we cannot upgrade anyway. `run_upgrade`
        // prints the "use a normal terminal" guidance.
        if matches!(root.scope, Scope::User) && uffs_mft::is_elevated() {
            return Ok(None);
        }

        let root_dir = strip_verbatim_prefix(root.dir.clone());

        // Only quiesce when something actually runs from the package (else
        // winget can replace the files unimpeded and there is nothing to stop).
        let daemon_inside = report.running.iter().any(|proc| {
            proc.component.label() == "daemon" && image_under(proc.image_path.as_ref(), &root_dir)
        });
        let broker_inside = uffs_winsvc::is_installed(uffs_broker_protocol::SERVICE_NAME)
            && report.running.iter().any(|proc| {
                proc.component.label() == "broker"
                    && image_under(proc.image_path.as_ref(), &root_dir)
            });
        if !daemon_inside && !broker_inside {
            return Ok(None);
        }

        println!("\nPreparing the winget package update \u{2026}");
        let daemon_stopped = daemon_inside && stop_daemon();
        let broker = if broker_inside {
            start_broker_cycle(&root_dir)?
        } else {
            None
        };
        Ok(Some(QuiesceState {
            broker,
            daemon_stopped,
        }))
    }

    /// See [`super::run_upgrade`].
    #[expect(clippy::print_stdout, reason = "CLI user-facing narration")]
    pub(super) fn run_upgrade(
        report: &DetectionReport,
        latest: &str,
        quiesce: Option<&QuiesceState>,
    ) -> Result<UpgradeOutcome> {
        let Some(root) = winget_root(report) else {
            return Ok(UpgradeOutcome::Ran);
        };
        if root_is_current(root, latest) {
            return Ok(UpgradeOutcome::Ran);
        }
        if matches!(root.scope, Scope::User) && uffs_mft::is_elevated() {
            println!(
                "\nThe winget package updates from a NORMAL (non-admin) terminal — winget\n\
                 refuses user-scope changes in elevated sessions. Run there:\n\
                 \x20   uffs --update"
            );
            return Ok(UpgradeOutcome::Ran);
        }

        let root_dir = strip_verbatim_prefix(root.dir.clone());
        println!("\nUpdating the winget package \u{2192} {latest} (via winget) \u{2026}");

        // Deferred when the running uffs.exe is itself part of the package:
        // winget can't replace a running image, so a post-exit script runs the
        // upgrade AND (if a broker cycle is in flight) writes the release file.
        let self_inside = std::env::current_exe()
            .map(strip_verbatim_prefix)
            .is_ok_and(|exe| exe.starts_with(&root_dir));
        if self_inside {
            let release = quiesce
                .and_then(|state| state.broker.as_ref())
                .map(BrokerHelper::release_file);
            schedule_deferred_upgrade(root.scope, release.as_deref())?;
            println!(
                "\nwinget finishes the update once this process exits (the running uffs.exe\n\
                 is part of the winget package). The daemon restarts on the next search."
            );
            return Ok(UpgradeOutcome::Deferred);
        }

        match spinner_while("Running winget upgrade", || run_winget_upgrade(root.scope)) {
            Ok(WingetUpgrade::Updated) => println!("\u{2713} winget package updated to {latest}."),
            Ok(WingetUpgrade::NotAvailableYet) => println!(
                "note: WinGet doesn't have {latest} yet — a new release's package manifest\n\
                 \x20  usually publishes shortly after the GitHub release. Your install is\n\
                 \x20  updated; the winget-managed copy catches up on the next `uffs --update`\n\
                 \x20  once it's live (or `winget upgrade {WINGET_PACKAGE_ID}` then)."
            ),
            Err(err) => println!(
                "\u{26a0} winget upgrade did not complete ({err:#}).\n\
                 \x20  Often this is just the new manifest not being on WinGet yet — re-run\n\
                 \x20  `uffs --update` in a bit, or `winget upgrade {WINGET_PACKAGE_ID}` directly."
            ),
        }
        Ok(UpgradeOutcome::Ran)
    }

    /// See [`super::resume`].
    #[expect(clippy::print_stdout, reason = "CLI user-facing narration")]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "resume consumes the quiesce — by-value expresses it is finished"
    )]
    pub(super) fn resume(state: QuiesceState, outcome: UpgradeOutcome) {
        if outcome == UpgradeOutcome::Deferred {
            // The post-exit script releases the broker helper; nothing to do
            // here (this process is about to exit).
            return;
        }
        if let Some(broker) = &state.broker {
            // Release the helper → it restarts the broker service.
            let _released: Result<(), std::io::Error> =
                std::fs::write(broker.release_file(), b"go");
            match wait_for_result(
                broker,
                &broker.started_file(),
                "Restarting the broker service",
            ) {
                Ok(()) => {}
                Err(err) => {
                    println!("\u{26a0} the broker service did not restart ({err:#}).");
                    println!(
                        "\x20  Start it from an elevated terminal: `uffs-broker --install` \
                         re-registers + starts it."
                    );
                }
            }
            broker.clear();
        }
        if state.daemon_stopped {
            // The daemon start blocks until Ready (measured ~7s warm, up to
            // ~69s on a cold cache), so animate a spinner — a silent minute is
            // the very "looks hung" symptom this rework set out to remove.
            let restarted = spinner_while("Restarting the index daemon", restart_daemon);
            if let Err(err) = &restarted {
                println!("\u{26a0} the index daemon did not restart ({err:#}).");
            }
            // A daemon that needs elevation to read the MFT (no broker) cannot
            // come back from this non-elevated restart. Say so with the
            // one-time fix instead of leaving a silently-stopped
            // daemon.
            if !daemon_running() {
                println!(
                    "\u{26a0} The index daemon was stopped for the update and couldn't restart\n\
                     \x20 non-elevated (no broker). Run `uffs-broker --install` for zero-UAC\n\
                     \x20 restarts, or start it from an elevated terminal."
                );
            }
        }
    }

    /// See [`super::run_broker_cycle_helper`] — the elevated child.
    pub(super) fn run_broker_cycle_helper(base_file: &str) -> Result<()> {
        if !uffs_mft::is_elevated() {
            bail!(
                "broker-cycle-helper must run elevated (it is spawned via a UAC prompt by \
                 `uffs --update`)"
            );
        }
        let helper = BrokerHelper {
            base: PathBuf::from(base_file),
        };
        let service = uffs_broker_protocol::SERVICE_NAME;

        // Stop → report (with the measured duration, so the real number lands
        // in the log/output instead of a guess).
        let t_stop = Instant::now();
        match uffs_winsvc::stop_with(service, SERVICE_TIMEOUT) {
            Ok(()) => write_result(
                &helper.stopped_file(),
                &format!("stopped in {}ms", t_stop.elapsed().as_millis()),
            ),
            Err(err) => {
                write_result(
                    &helper.error_file(),
                    &format!(
                        "stop failed after {}ms: {err:#}",
                        t_stop.elapsed().as_millis()
                    ),
                );
                bail!("broker stop failed: {err:#}");
            }
        }

        // Wait for the orchestrator to finish the update.
        let release = helper.release_file();
        let deadline = Instant::now() + RELEASE_WAIT;
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }

        // Restart → report (with the measured duration).
        let t_start = Instant::now();
        match uffs_winsvc::start_with(service, SERVICE_TIMEOUT) {
            Ok(()) => {
                write_result(
                    &helper.started_file(),
                    &format!("started in {}ms", t_start.elapsed().as_millis()),
                );
                Ok(())
            }
            Err(err) => {
                write_result(
                    &helper.error_file(),
                    &format!(
                        "restart failed after {}ms: {err:#}",
                        t_start.elapsed().as_millis()
                    ),
                );
                bail!("broker restart failed: {err:#}")
            }
        }
    }

    /// The first winget-managed root, if any.
    fn winget_root(report: &DetectionReport) -> Option<&InstallRoot> {
        report
            .roots
            .iter()
            .find(|root| matches!(root.channel, Channel::WinGet))
    }

    /// Whether every version-parseable binary in the winget root already
    /// matches `latest` (nothing for winget to do).
    fn root_is_current(root: &InstallRoot, latest: &str) -> bool {
        let want = latest.strip_prefix('v').unwrap_or(latest);
        let versions: Vec<&str> = root
            .binaries
            .iter()
            .filter_map(|bin| bin.version.as_deref())
            .collect();
        !versions.is_empty() && versions.iter().all(|have| *have == want)
    }

    /// Stop the daemon via the graceful shutdown RPC (no privileges; also stops
    /// an elevated daemon), falling back to the kill handler. Returns whether
    /// either of them stopped it, so a daemon that is still running is not
    /// "restarted" over itself after the update.
    fn stop_daemon() -> bool {
        let stopped = uffs_client::connect_sync::UffsClientSync::connect_raw()
            .is_ok_and(|mut client| client.shutdown().is_ok())
            || crate::commands::daemon_mgmt::daemon_quiet(&crate::args::DaemonAction::Kill).is_ok();
        std::thread::sleep(Duration::from_millis(750));
        stopped
    }

    /// Whether the resident daemon is up: a fresh PID file naming a live
    /// process. Used after the best-effort restart to tell the user when a
    /// no-broker elevated daemon could not come back non-elevated.
    fn daemon_running() -> bool {
        let path = uffs_client::daemon_ctl::pid_file_path();
        uffs_client::daemon_ctl::parse_pid_file(&path)
            .is_some_and(|(pid, ..)| uffs_client::daemon_ctl::is_pid_alive(pid))
    }

    /// Restart the daemon after the upgrade. A failure is reported, not
    /// fatal: the next search auto-starts it.
    ///
    /// # Errors
    ///
    /// The start's own error.
    fn restart_daemon() -> Result<()> {
        crate::commands::daemon_mgmt::daemon_quiet(&crate::args::DaemonAction::Start {
            mft_file: Vec::new(),
            data_dir: None,
            drives: Vec::new(),
            no_cache: false,
            log_level: "info".to_owned(),
            log_file: None,
            elevate: false,
        })
    }

    /// Spawn the elevated helper (one UAC) and wait for it to report the broker
    /// stopped. `Err` aborts the update (the upgrade would just fail locked).
    #[expect(clippy::print_stdout, reason = "CLI user-facing narration")]
    fn start_broker_cycle(_root_dir: &Path) -> Result<Option<BrokerHelper>> {
        let helper = BrokerHelper {
            base: std::env::temp_dir().join(format!("uffs-broker-cycle-{}", std::process::id())),
        };
        helper.clear();

        println!(
            "  the broker service runs from the winget package — cycling it around the\n\
             \x20 update (Windows shows one UAC prompt)\u{2026}"
        );
        spawn_broker_cycle_helper(&helper.base)?;
        wait_for_result(
            &helper,
            &helper.stopped_file(),
            "Stopping the broker service",
        )
        .context("the broker service could not be stopped for the update")?;
        Ok(Some(helper))
    }

    /// Whether `image` (a running process's path) lies under `root_dir`.
    fn image_under(image: Option<&PathBuf>, root_dir: &Path) -> bool {
        image
            .cloned()
            .map(strip_verbatim_prefix)
            .is_some_and(|img| img.starts_with(root_dir))
    }

    /// Spinner-poll for `success_file` OR the helper's `.error` file. `Ok` when
    /// the success file appears (its measured-duration text is echoed); `Err`
    /// when the error file appears (with its message) or the wait times out.
    #[expect(clippy::print_stdout, reason = "interactive progress spinner")]
    fn wait_for_result(helper: &BrokerHelper, success_file: &Path, label: &str) -> Result<()> {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let error_file = helper.error_file();
        let deadline = Instant::now() + RESULT_WAIT;
        let mut frame = 0_usize;
        loop {
            if success_file.exists() {
                clear_line();
                if let Ok(raw) = std::fs::read_to_string(success_file) {
                    let note = raw.trim();
                    if !note.is_empty() {
                        println!("  \u{2713} {label} \u{2014} {note}.");
                    }
                }
                return Ok(());
            }
            if error_file.exists() {
                clear_line();
                let msg = std::fs::read_to_string(&error_file)
                    .unwrap_or_else(|_| "unknown error".to_owned());
                bail!("{}", msg.trim());
            }
            if Instant::now() >= deadline {
                clear_line();
                bail!("timed out after {}s", RESULT_WAIT.as_secs());
            }
            let glyph = FRAMES.get(frame % FRAMES.len()).copied().unwrap_or("*");
            print!("\r  {glyph} {label}\u{2026}      ");
            let _flushed = std::io::stdout().flush();
            std::thread::sleep(POLL);
            frame = frame.wrapping_add(1);
        }
    }

    /// Erase the spinner line.
    #[expect(clippy::print_stdout, reason = "interactive progress spinner")]
    fn clear_line() {
        print!("\r{:60}\r", "");
        let _flushed = std::io::stdout().flush();
    }

    /// Write a one-line result file for the orchestrator to read.
    fn write_result(path: &Path, text: &str) {
        let _written: Result<(), std::io::Error> = std::fs::write(path, text.as_bytes());
    }

    /// Spawn this uffs.exe elevated (one UAC) in the hidden broker-cycle mode.
    fn spawn_broker_cycle_helper(base: &Path) -> Result<()> {
        let raw_exe = std::env::current_exe().context("locating uffs.exe for the UAC helper")?;
        let exe = strip_verbatim_prefix(raw_exe);
        let exe_escaped = exe.display().to_string().replace('\'', "''");
        let base_escaped = base.display().to_string().replace('\'', "''");
        let script = format!(
            "Start-Process -FilePath '{exe_escaped}' \
             -ArgumentList '--update','broker-cycle-helper','{base_escaped}' \
             -Verb RunAs -WindowStyle Hidden"
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("spawning the elevated broker-cycle helper")?;
        if !status.success() {
            bail!("could not launch the elevated broker-cycle helper (UAC declined?)");
        }
        Ok(())
    }

    /// winget's "no applicable upgrade found" exit code. Returned when the
    /// installed version already matches the catalog's newest — the common case
    /// right after a release, before the new version's manifest has published
    /// to `WinGet`. It is a benign "nothing newer yet", not a real failure,
    /// so it is narrated as a note. (Value observed from a live `uffs
    /// --update` run whose target release was not yet on `WinGet`.)
    const WINGET_NO_APPLICABLE_UPGRADE: i32 = 0x8A15_002B_u32.cast_signed();

    /// The outcome of a `winget upgrade` worth distinguishing to the user.
    enum WingetUpgrade {
        /// The winget package was upgraded.
        Updated,
        /// `WinGet` had no newer version to install (typically the new
        /// release's manifest has not published yet).
        NotAvailableYet,
    }

    /// Whether `code` is winget's benign "no applicable upgrade" exit code.
    const fn is_no_applicable_upgrade(code: Option<i32>) -> bool {
        matches!(code, Some(WINGET_NO_APPLICABLE_UPGRADE))
    }

    /// Run `winget upgrade` for the package, scope-aware, silent. Distinguishes
    /// a real failure from "`WinGet` has nothing newer yet" (the publish-lag
    /// case).
    fn run_winget_upgrade(scope: Scope) -> Result<WingetUpgrade> {
        let mut command = Command::new("winget");
        command.args([
            "upgrade",
            "--id",
            WINGET_PACKAGE_ID,
            "--silent",
            "--accept-source-agreements",
        ]);
        match scope {
            Scope::Machine => {
                command.args(["--scope", "machine"]);
            }
            Scope::User => {
                command.args(["--scope", "user"]);
            }
            Scope::Unknown => {}
        }
        let status = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("spawning winget upgrade")?;
        if status.success() {
            return Ok(WingetUpgrade::Updated);
        }
        if is_no_applicable_upgrade(status.code()) {
            return Ok(WingetUpgrade::NotAvailableYet);
        }
        bail!("winget upgrade exited with {status}")
    }

    /// Defer the upgrade past process exit (the running uffs.exe is part of the
    /// package): a detached `cmd` waits ~2s, runs `winget upgrade`, and — when
    /// a broker cycle is in flight — writes the release file so the
    /// elevated helper restarts the service after the upgrade.
    fn schedule_deferred_upgrade(scope: Scope, release_file: Option<&Path>) -> Result<()> {
        use std::os::windows::process::CommandExt as _;

        let scope_arg = match scope {
            Scope::Machine => " --scope machine",
            Scope::User => " --scope user",
            Scope::Unknown => "",
        };
        let release_leg = release_file.map_or_else(String::new, |path| {
            format!(" & echo go > \"{}\"", path.display())
        });
        let script = format!(
            "ping 127.0.0.1 -n 3 >nul & winget upgrade --id {WINGET_PACKAGE_ID} --silent \
             --accept-source-agreements{scope_arg}{release_leg} & rem deferred winget upgrade"
        );
        Command::new("cmd")
            .raw_arg("/c")
            .raw_arg(&script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("scheduling the deferred winget upgrade")?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{WINGET_NO_APPLICABLE_UPGRADE, is_no_applicable_upgrade};

        #[test]
        fn classifies_the_no_applicable_upgrade_code() {
            assert!(is_no_applicable_upgrade(Some(WINGET_NO_APPLICABLE_UPGRADE)));
            assert!(!is_no_applicable_upgrade(Some(0_i32)));
            assert!(!is_no_applicable_upgrade(Some(1_i32)));
            assert!(!is_no_applicable_upgrade(None));
        }
    }
}
