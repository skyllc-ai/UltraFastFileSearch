// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs --update [<action>]` — self-update (design in
//! `docs/dev/architecture/UFFS-Self-Update-Feasibility-and-Design.md`; CLI
//! grammar in `docs/architecture/cli-grammar.md`).
//!
//! **No action** is the ordinary-user command: update UFFS end-to-end. It runs
//! Phase A (detect & capture) — discovering every install *root* from the live
//! anchors (invoking CLI + running daemon / MCP / broker), enumerating each
//! root's binaries + versions — then compares against the latest release; if a
//! newer release is available (or the install is version-skewed) it
//! acquires + applies (journaled, auto-rollback), otherwise it reports the
//! install is current and touches nothing.
//!
//! The actions expose the phases for inspection / scripting: `check` (is an
//! update available? — non-mutating), `snapshot` (freeze the detected state),
//! `acquire` (download + verify into staging), `apply` (the full mutating
//! swap), `doctor` (health check), `recover` (finish an interrupted update).
//!
//! Entry point: `run_update` (dispatched from `--update` in `main`).

mod acquire;
mod apply;
pub(crate) mod binaries;
pub(crate) mod channel;
mod doctor;
pub(crate) mod model;
mod plan;
pub(crate) mod procinfo;
mod report;
mod self_heal;
mod snapshot;
mod winget;
mod winget_discover;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use model::{Anchor, Channel, Component, DetectionReport, InstallRoot, RunningProcess, Scope};

use crate::commands::elevation;

/// Run `uffs --update [<action>]` — uniform `--<command> [action] [--options]`
/// grammar (design: `docs/architecture/cli-grammar.md`). The action is the
/// first positional token:
///
/// - *(none)* → **update end-to-end if one is needed** (the ordinary-user
///   command): detect → compare to the latest release → acquire + apply, or
///   report "already up to date" and touch nothing.
/// - `check`    → is an update available? detect + compare (non-mutating).
/// - `snapshot` → detect + freeze a snapshot (Phase B).
/// - `acquire`  → + download + SHA-verify into staging (Phase C).
/// - `apply`    → + the full mutating update (stop/swap/smoke/commit/restore).
/// - `doctor`   → end-to-end health check (`--repair` / `--offline`).
/// - `recover`  → finish/roll back an interrupted update in the foreground.
///
/// Options (`--version`, `--repair`, `--offline`) follow the action.
pub(crate) fn run_update(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }

    // The action is the first *positional* token (a leading flag or nothing
    // means bare detect). Validate up front, before any detection/output.
    let action = args
        .first()
        .map(String::as_str)
        .filter(|tok| !tok.starts_with('-'));

    // Hidden elevated-child mode (see `winget::run_broker_cycle_helper`):
    // stop the broker service, wait for the release file, restart it. Spawned
    // via UAC by the winget orchestration; never part of the interactive flow
    // and deliberately absent from the visible action list below.
    if action == Some("broker-cycle-helper") {
        let release_file = args.get(1).map_or_default(String::as_str).to_owned();
        if release_file.is_empty() {
            bail!("broker-cycle-helper needs the release-file path argument");
        }
        return winget::run_broker_cycle_helper(&release_file);
    }
    if let Some(act) = action
        && !matches!(
            act,
            "check" | "snapshot" | "acquire" | "apply" | "doctor" | "repair" | "recover" | "bins"
        )
    {
        bail!(
            "unknown `--update` action `{act}` — expected: check | snapshot | acquire | apply | doctor | repair | recover | bins"
        );
    }

    // `-v` / `--verbose`: show the full per-binary + per-process breakdown
    // (and forward verbosity to the spawned `uffs-update` helper). Default is
    // a few plain-language lines for someone who just wants the gist.
    let verbose = args.iter().any(|arg| arg == "-v" || arg == "--verbose");

    // `bins` prints the canonical core binary stems (the single source of
    // truth in `binaries::KNOWN_BINARIES`) for tooling — e.g. the `just`
    // deploy recipes read the set from here instead of hardcoding it.
    // Pure + non-mutating: no detection needed.
    if action == Some("bins") {
        binaries::print_core_stems();
        return Ok(());
    }

    // `recover` finishes (or rolls back) an interrupted update in the
    // foreground — the on-demand twin of the startup best-effort self-heal.
    if action == Some("recover") {
        return self_heal::run_foreground();
    }

    // `repair` is the first-class verb for `doctor --repair`; a bare `--repair`
    // flag (with no action) means the same. Both route to the doctor health
    // check with self-heal on — so the user never has to remember whether
    // repair is a verb or a flag.
    let repair = action == Some("repair") || args.iter().any(|arg| arg == "--repair");

    // `doctor` (diagnose) and `repair` (diagnose + self-heal) share one flow:
    // detect → freeze a snapshot → hand off to the helper's health check
    // (which prints its own report). A health-check snapshot has no target.
    if action == Some("doctor") || repair {
        let report = detect();
        let snapshot_path = snapshot::write_snapshot(&report, None)?;
        let mut forwarded: Vec<String> = args.to_vec();
        if repair && !forwarded.iter().any(|arg| arg == "--repair") {
            forwarded.push("--repair".to_owned());
        }
        // Helper health check (+ local self-heal when --repair): journal,
        // backups, services, broker, release reach. Captured (not
        // `?`-propagated) so a reported failure doesn't pre-empt the
        // update-flow fix below.
        let health = doctor::spawn(&snapshot_path, &forwarded);

        // Update-class issues — out-of-date, version-skewed, or **missing a
        // core binary** — are fixed by the update flow itself, which
        // already owns the core set. So doctor *redirects* there rather
        // than teaching the health-check helper that set. `--offline`
        // skips this (assess needs the release feed). With `--repair`
        // we run it; interactively we ask; piped we just point.
        if !args.iter().any(|arg| arg == "--offline")
            && matches!(assess(&report), UpdateAssessment::Available { .. })
        {
            if repair || prompt_yes_no("Run `uffs --update` now to fix this?") {
                return run_automatic_update(&report, verbose);
            }
            print_update_redirect_hint();
        }
        return health;
    }

    let report = detect();
    report::print_human(&report, verbose);
    if verbose {
        print_phase_a_footer();
    }

    match action {
        // Bare `uffs --update` — the ordinary-user command: update end to end,
        // but only if one is actually needed (never stop/restart services when
        // already current).
        None => run_automatic_update(&report, verbose),
        // `check` — non-mutating: is an update available?
        Some("check") => {
            report_assessment(&assess(&report));
            Ok(())
        }
        Some("snapshot") => {
            write_and_report_snapshot(&report);
            Ok(())
        }
        // `acquire` only stages + verifies; `apply` implies acquire then swaps.
        Some("acquire" | "apply") => {
            // The target this snapshot is for: an explicit `--version`, else
            // the resolved latest — so the journal stamps `to_version`
            // faithfully instead of "unknown".
            let target = flag_value(args, "--version").or_else(acquire::latest_version);
            let snapshot_path = snapshot::write_snapshot(&report, target.as_deref())?;
            acquire::spawn(&snapshot_path, target.as_deref(), verbose)?;
            if action == Some("apply") {
                apply::spawn(&snapshot_path, verbose)?;
            }
            Ok(())
        }
        // `doctor`/`recover` returned earlier; any other token was rejected.
        _ => Ok(()),
    }
}

/// Whether an update is warranted, and the target tag.
enum UpdateAssessment {
    /// Installed matches the latest release and there is no version skew.
    UpToDate {
        /// The latest release tag (e.g. `v0.6.5`).
        latest: String,
    },
    /// An update is available (a newer release, or a skewed install to
    /// realign).
    Available {
        /// The latest release tag to move to.
        latest: String,
    },
    /// The release feed could not be reached (offline) — cannot decide.
    Offline,
}

/// Compare the detected install against the latest release (one non-mutating
/// metadata fetch via the helper) to decide whether an update is warranted.
fn assess(report: &DetectionReport) -> UpdateAssessment {
    let installed = report::distinct_versions(report);
    let skewed = installed.len() > 1;
    // A core binary missing from a real install root makes it *incomplete* —
    // an update reconciles the full core set back into place.
    let incomplete = has_missing_core(report);
    let Some(latest) = acquire::latest_version() else {
        return UpdateAssessment::Offline;
    };
    let newer = match installed.as_slice() {
        // A single clean version: update only if the release is different.
        [only] => normalize_tag(&latest) != *only,
        // Zero or mixed versions → an update realigns the install.
        _ => true,
    };
    if skewed || newer || incomplete {
        UpdateAssessment::Available { latest }
    } else {
        UpdateAssessment::UpToDate { latest }
    }
}

/// True when any **unmanaged** install root is missing a core binary — i.e.
/// the install is incomplete relative to the canonical set
/// (`binaries::KNOWN_BINARIES`, the single source of truth). `WinGet` roots are
/// delegated to `winget upgrade`, so they are not reconciled here.
///
/// `uffs-broker` is **excluded** from the required set: it is a Windows
/// `LocalSystem` service installed *once* (via the winget package or
/// `uffs-broker --install`), so it legitimately does not live in every
/// hand-placed CLI root. Requiring it would mark a perfectly current install
/// "incomplete" and trigger a pointless update + UAC prompt every time the
/// broker sits in a different (e.g. winget) root than the CLI.
fn has_missing_core(report: &DetectionReport) -> bool {
    report
        .roots
        .iter()
        .filter(|root| root.channel.label() == "unmanaged")
        .any(|root| root_missing_core(root, &binaries::KNOWN_BINARIES))
}

/// Binary stem of the Access Broker — excluded from [`root_missing_core`]
/// because it is a once-installed Windows service, not a per-root binary.
const BROKER_STEM: &str = "uffs-broker";

/// Whether `root` lacks any *required* core binary, treating the broker as
/// optional (see [`has_missing_core`]). Split out as a pure function so the
/// broker-exclusion is testable off Windows, where `KNOWN_BINARIES` omits the
/// broker anyway.
fn root_missing_core(root: &InstallRoot, required: &[&str]) -> bool {
    required
        .iter()
        .filter(|stem| **stem != BROKER_STEM)
        .any(|stem| !root.binaries.iter().any(|bin| bin.name == *stem))
}

/// Point the user at the update flow — the fix for an out-of-date, skewed, or
/// incomplete install (doctor detects; `uffs --update` repairs).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_update_redirect_hint() {
    println!(
        "\n\u{2192} Run `uffs --update` to bring the install up to date and complete the core set."
    );
}

/// Ask a yes/no question on an interactive terminal. Returns `false` **without
/// prompting** when stdin is not a TTY (scripts / pipes / CI), so callers fall
/// back to a printed hint instead of blocking on a read that can't be answered.
#[expect(clippy::print_stdout, reason = "interactive CLI prompt")]
fn prompt_yes_no(question: &str) -> bool {
    use std::io::{IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("\n{question} [y/N] ");
    if std::io::stdout().flush().is_err() {
        return false;
    }
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Run the full end-to-end update when one is needed; otherwise report the
/// install is current. Journaled + auto-rollback (delegated to `apply`).
fn run_automatic_update(report: &DetectionReport, verbose: bool) -> Result<()> {
    let latest = match assess(report) {
        UpdateAssessment::UpToDate { latest } => {
            print_already_current(&latest);
            return Ok(());
        }
        UpdateAssessment::Offline => {
            print_offline_notice();
            return Ok(());
        }
        UpdateAssessment::Available { latest } => latest,
    };

    // Mirror `uffs --uninstall`: build the per-root plan and run the SHARED
    // elevation gate up front — surface admin-only roots, decide once
    // (continue-without / abort) — so nothing fails mid-swap. The per-root
    // elevation model lives in `plan`; the gate in `commands::elevation`.
    let mut plan = plan::UpdatePlan::build(report, &latest);
    // Update has no in-flow elevation (winget refuses a user package elevated),
    // so `offer_inflow_elevation = false` → the gate is the binary
    // continue-without / abort.
    if let elevation::ElevationChoice::ContinueWithout(dropped) =
        elevation::elevation_gate(&mut plan, false, false, false, &elevation::GateWording {
            action: "update",
            rerun_cmd: "uffs --update",
        })?
    {
        plan::print_skipped_elevation(&dropped);
    }

    // Elevated shells cannot run `winget upgrade` for a user-scope package
    // (winget refuses) — split those out as a delegation note instead of a
    // mid-flow failure.
    let delegated = plan.split_off_non_elevated_when_elevated(uffs_mft::platform::is_elevated());
    plan::print_winget_delegation(&delegated);

    if !plan.has_work() {
        plan::print_no_local_work();
        return Ok(());
    }

    // Feed the journaled execute a report PRUNED to just the roots the gated
    // plan will actually touch, so quiesce/apply/winget never operate on a
    // dropped (elevation-required or delegated) root — this removes the old
    // stop-timeout → rollback on an install the caller could never update.
    let doable = plan::prune_report(report, &plan);

    // The `uffs-update` helper (acquire + apply) only handles hand-placed
    // (unmanaged) roots — winget roots are upgraded separately by
    // `winget::run_upgrade`. So when the gate/prune left only winget roots,
    // skip BOTH acquire and apply: the helper errors with "no unmanaged
    // binaries to acquire", and apply would needlessly stop the daemon. This is
    // the other half of the elevated-daemon fix.
    let has_unmanaged = doable
        .roots
        .iter()
        .any(|root| matches!(root.channel, Channel::Unmanaged));

    print_updating(&latest);
    let snapshot_path = snapshot::write_snapshot(&doable, Some(&latest))?;
    if has_unmanaged {
        acquire::spawn(&snapshot_path, None, verbose)?;
    }
    // Quiesce-first: on a winget-managed install, stop the daemon + (package)
    // broker up front — ONE UAC — so BOTH the hand-rolled ~\bin update and the
    // winget upgrade run against a stopped install (a bare `winget upgrade`
    // fails on locked images). No-op for a plain unmanaged install (apply keeps
    // its own daemon stop).
    let quiesce = winget::quiesce(&doable)?;
    if has_unmanaged {
        apply::spawn(&snapshot_path, verbose)?;
    }
    let outcome = winget::run_upgrade(&doable, &latest, &quiesce)?;
    winget::resume(quiesce, outcome);
    print_updated(&latest);
    Ok(())
}

/// Print the verdict for the non-mutating `uffs --update check`.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn report_assessment(plan: &UpdateAssessment) {
    match plan {
        UpdateAssessment::UpToDate { latest } => println!("\n\u{2713} Up to date ({latest})."),
        UpdateAssessment::Offline => print_offline_notice(),
        UpdateAssessment::Available { latest } => {
            println!("\n\u{2b06} Update available: {latest} — run `uffs --update` to install.");
        }
    }
}

/// Strip a leading `v` from a release tag so `v0.6.5` compares to `0.6.5`.
pub(crate) fn normalize_tag(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// "Already up to date" (bare update, nothing to do).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_already_current(latest: &str) {
    println!("\n\u{2713} UFFS is already up to date ({latest}).");
}

/// "Updating …" banner before the mutating flow.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_updating(latest: &str) {
    println!("\nUpdating UFFS \u{2192} {latest} …");
}

/// "Updated" confirmation after a successful apply.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_updated(latest: &str) {
    println!("\n\u{2713} UFFS updated to {latest}.");
}

/// Couldn't reach the release feed — can't download, so can't update.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_offline_notice() {
    println!(
        "\nCouldn't check for updates (offline?). Reconnect and re-run, \
         or `uffs --update apply` to force."
    );
}

/// Phase H self-heal entry: spawn `uffs-update recover` if a live update
/// journal is present. Best-effort and non-blocking, so a crash mid-update
/// is healed on the next `uffs` invocation. Called once at CLI startup.
pub(crate) fn maybe_self_heal() {
    self_heal::trigger();
}

/// Return the value following `name` in `args` (`--name value`).
fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}

/// Write a Phase-B snapshot and report where it landed.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn write_and_report_snapshot(report: &DetectionReport) {
    match snapshot::write_snapshot(report, None) {
        Ok(path) => println!("\nSnapshot written: {}", path.display()),
        Err(err) => {
            #[expect(clippy::print_stderr, reason = "CLI user-facing error")]
            {
                eprintln!("\nSnapshot failed: {err}");
            }
        }
    }
}

/// Phase A orchestration: anchors → roots → channel + versions, plus the
/// running-process map.
pub(crate) fn detect() -> DetectionReport {
    let mut roots: Vec<InstallRoot> = Vec::new();
    let mut running: Vec<RunningProcess> = Vec::new();

    // A.1 anchor #1 — the invoking CLI.
    if let Some(dir) = current_exe_dir() {
        upsert_root(&mut roots, dir, Anchor::Cli);
    }
    // A.1 anchors #1b + #3 — the watchdog, then the MCP gateway(s), BEFORE the
    // daemon: stops follow this order, and the watchdog would revive a daemon
    // stopped before it.
    for (stem, component, anchor) in [
        ("uffs-watchdog", Component::Watchdog, Anchor::Cli),
        ("uffsmcp", Component::Mcp, Anchor::Mcp),
    ] {
        for pid in procinfo::find_pids_by_name(stem) {
            capture_native(&mut roots, &mut running, component, anchor, pid);
        }
    }
    // A.1 anchor #2 — the running daemon. Prefer its persisted launch
    // state (reliable command line); fall back to native introspection.
    capture_daemon(&mut roots, &mut running);
    // A.1 anchor #4 — the broker service (Windows-only).
    capture_broker(&mut roots, &mut running);
    // A.1b — a dormant WinGet install has no live anchor (nothing running from
    // it, not the invoking exe), so find it at its well-known package location
    // too; otherwise `uffs --update` can neither see nor reconcile it.
    winget_discover::discover(&mut roots);

    // A.2 + A.3 + A.4 — per root: enumerate binaries, classify, version.
    for root in &mut roots {
        root.binaries = binaries::enumerate(&root.dir);
        let (chan, scope) = channel::classify(&root.dir);
        root.channel = chan;
        root.scope = scope;
    }

    DetectionReport { roots, running }
}

/// Directory of the currently-running `uffs` executable.
fn current_exe_dir() -> Option<PathBuf> {
    let parent = std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)?;
    Some(strip_verbatim_prefix(parent))
}

/// Strip the Windows `\\?\` verbatim prefix from a (typically canonicalized)
/// path so it matches plain `PATH` entries and displays cleanly
/// (`\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\sh` -> `\\srv\sh`). No-op off Windows
/// and on already-plain paths. `std::fs::canonicalize` on Windows always
/// returns the verbatim form, which otherwise never matches a bare `C:\…` PATH
/// entry — the cause of the resolution table mislabeling the active copy
/// `off-path`.
pub(crate) fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    // Runs on every platform: a non-Windows path never carries a `\\?\` prefix,
    // so [`strip_verbatim_str`] returns `None` and the path is left untouched.
    let stripped = path.to_str().and_then(strip_verbatim_str);
    stripped.map_or(path, PathBuf::from)
}

/// Pure verbatim-prefix strip for [`strip_verbatim_prefix`], split out so it is
/// testable on every platform. Returns `None` when `text` has no `\\?\` prefix.
fn strip_verbatim_str(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\").map(ToOwned::to_owned)
}

/// Resolve the running daemon's pid — PID file first, then a name scan.
fn daemon_pid() -> Option<u32> {
    procinfo::daemon_pid_from_file()
        .or_else(|| procinfo::find_pids_by_name("uffsd").into_iter().next())
}

/// Insert `dir` as an install root (deduplicated by canonical path) and
/// record that `anchor` surfaced it.
fn upsert_root(roots: &mut Vec<InstallRoot>, dir: PathBuf, anchor: Anchor) {
    let key = strip_verbatim_prefix(std::fs::canonicalize(&dir).unwrap_or(dir));
    if let Some(existing) = roots.iter_mut().find(|root| root.dir == key) {
        existing.note_anchor(anchor);
        return;
    }
    roots.push(InstallRoot {
        dir: key,
        channel: Channel::Unknown,
        scope: Scope::Unknown,
        anchored_by: vec![anchor],
        binaries: Vec::new(),
    });
}

/// Capture the daemon anchor: prefer its persisted launch state (which
/// carries a reliable command line); otherwise fall back to native
/// introspection of the pid from the PID file.
fn capture_daemon(roots: &mut Vec<InstallRoot>, running: &mut Vec<RunningProcess>) {
    if let Some((pid, state)) = procinfo::daemon_launch_state() {
        let image_path = state.image_path;
        if let Some(dir) = image_path.as_deref().and_then(Path::parent) {
            upsert_root(roots, dir.to_path_buf(), Anchor::Daemon);
        }
        let version = state
            .version
            .or_else(|| image_path.as_deref().and_then(binaries::probe_version));
        running.push(RunningProcess {
            component: Component::Daemon,
            pid,
            image_path,
            command_line: state.command_line,
            version,
        });
    } else if let Some(pid) = daemon_pid() {
        capture_native(roots, running, Component::Daemon, Anchor::Daemon, pid);
    }
}

/// Capture a live process via native introspection: contribute its image
/// directory as a root and record it in the running-process map.
fn capture_native(
    roots: &mut Vec<InstallRoot>,
    running: &mut Vec<RunningProcess>,
    component: Component,
    anchor: Anchor,
    pid: u32,
) {
    let image_path = procinfo::image_path(pid);
    if let Some(dir) = image_path.as_deref().and_then(Path::parent) {
        upsert_root(roots, dir.to_path_buf(), anchor);
    }
    let version = image_path.as_deref().and_then(binaries::probe_version);
    running.push(RunningProcess {
        component,
        pid,
        image_path,
        command_line: procinfo::command_line(pid),
        version,
    });
}

/// Surface the broker service's install root + running process.
///
/// Cross-platform: `procinfo::broker_service()` returns `None` off
/// Windows, so this is an early no-op there — but the `Anchor::Broker` /
/// `Component::Broker` constructions stay in-source on every target
/// (no platform-conditional dead code).
fn capture_broker(roots: &mut Vec<InstallRoot>, running: &mut Vec<RunningProcess>) {
    let Some((bin_path, service_pid)) = procinfo::broker_service() else {
        return;
    };
    if let Some(dir) = bin_path.parent() {
        upsert_root(roots, dir.to_path_buf(), Anchor::Broker);
    }
    let version = binaries::probe_version(&bin_path);
    if let Some(pid) = service_pid {
        running.push(RunningProcess {
            component: Component::Broker,
            pid,
            image_path: Some(bin_path),
            command_line: procinfo::command_line(pid),
            version,
        });
    }
}

/// Print the `uffs --update` help text.
#[expect(clippy::print_stdout, reason = "intentional help output")]
fn print_help() {
    println!(
        "uffs --update — update UFFS to the latest release\n\n\
         USAGE:\n\
         \x20 uffs --update [<action>] [--options]\n\n\
         With no action, updates UFFS end-to-end: if a newer release is\n\
         available (or the install is version-skewed) it downloads, verifies,\n\
         and swaps every binary in place (journaled + auto-rollback); if you\n\
         are already current it does nothing. The actions below expose the\n\
         individual phases for inspection or scripting.\n\n\
         ACTIONS:\n\
         \x20 (none)              Update end-to-end (the default) — only if one\n\
         \x20                     is needed; never touches services when current.\n\
         \x20 check               Is an update available? Detect + compare to the\n\
         \x20                     latest release. Non-mutating.\n\
         \x20 snapshot            Detect + persist the state to JSON.\n\
         \x20 acquire             + download + SHA-256-verify the release into\n\
         \x20                     staging (via the uffs-update helper). No replace.\n\
         \x20 apply               + the FULL mutating update: stop services,\n\
         \x20                     atomically swap + smoke-test, commit, restart.\n\
         \x20                     Journaled + auto-rollback on failure.\n\
         \x20 doctor              End-to-end health check (versions, dirs, journal,\n\
         \x20                     backups, services, broker pipe, release reach). If\n\
         \x20                     out of date / skewed / missing a core binary, it\n\
         \x20                     points to `uffs --update` (asks first on a TTY).\n\
         \x20 repair              Diagnose + self-heal (= doctor --repair): resume/\n\
         \x20                     roll back an interrupted update, sweep stale\n\
         \x20                     backups, restart stopped services — and run the\n\
         \x20                     update flow if the install is out of date.\n\
         \x20 recover             Finish or roll back an interrupted update now\n\
         \x20                     (foreground; the on-demand self-heal).\n\
         \x20 bins                Print the core binary stems (one per line) —\n\
         \x20                     the canonical set, for scripts/tooling.\n\n\
         OPTIONS:\n\
         \x20 -v, --verbose       Show the full breakdown — per-binary versions,\n\
         \x20                     PIDs, launch commands, every doctor check.\n\
         \x20 --version <tag>     Acquire/apply a specific release tag (default: latest).\n\
         \x20 --repair            (doctor) self-heal what can be fixed (or use the\n\
         \x20                     `repair` action above).\n\
         \x20 --offline           (doctor) skip the network checks.\n\n\
         EXAMPLES:\n\
         \x20 uffs --update                 update now (if needed)\n\
         \x20 uffs --update check           is a new release available?\n\
         \x20 uffs --update doctor          health-check the update flow\n\
         \x20 uffs --update repair          self-heal the update flow\n\
         \x20 uffs --update apply --version v0.6.3   pin a specific release\n"
    );
}

/// Footer clarifying which mutating phases are not yet implemented.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_phase_a_footer() {
    println!(
        "\n(Detect + snapshot + acquire are non-mutating. Stop / replace / restore \
         land in the apply phase; nothing on a live install was changed.)"
    );
}

#[cfg(test)]
mod tests {
    use super::model::{Anchor, BinaryInfo, Channel, InstallRoot, Scope};
    use super::{normalize_tag, root_missing_core, strip_verbatim_str, upsert_root};

    /// An unmanaged root holding exactly the given binary stems.
    fn unmanaged_root(stems: &[&str]) -> InstallRoot {
        InstallRoot {
            dir: "/nonexistent/bin".into(),
            channel: Channel::Unmanaged,
            scope: Scope::Unknown,
            anchored_by: vec![Anchor::Cli],
            binaries: stems
                .iter()
                .map(|name| BinaryInfo {
                    name: (*name).to_owned(),
                    version: Some("0.6.20".to_owned()),
                })
                .collect(),
        }
    }

    // The Windows core set (broker included) — the case the exclusion fixes.
    const CORE_WITH_BROKER: [&str; 6] = [
        "uffs",
        "uffsd",
        "uffsmcp",
        "uffs-update",
        "uffs-mft",
        "uffs-broker",
    ];

    #[test]
    fn absent_broker_does_not_mark_root_incomplete() {
        // Every non-broker core binary present, broker absent (it lives in the
        // winget root / as the service) → NOT incomplete, so no pointless
        // update.
        let root = unmanaged_root(&["uffs", "uffsd", "uffsmcp", "uffs-update", "uffs-mft"]);
        assert!(
            !root_missing_core(&root, &CORE_WITH_BROKER),
            "a missing broker must not count as an incomplete install"
        );
    }

    #[test]
    fn absent_non_broker_core_marks_root_incomplete() {
        // A genuine gap (no uffsd) is still incomplete, broker present or not.
        let root = unmanaged_root(&["uffs", "uffsmcp", "uffs-update", "uffs-mft", "uffs-broker"]);
        assert!(
            root_missing_core(&root, &CORE_WITH_BROKER),
            "a missing non-broker core binary must count as incomplete"
        );
    }

    #[test]
    fn strip_verbatim_str_handles_drive_unc_and_plain() {
        assert_eq!(
            strip_verbatim_str(r"\\?\C:\Users\rnio\bin").as_deref(),
            Some(r"C:\Users\rnio\bin")
        );
        assert_eq!(
            strip_verbatim_str(r"\\?\UNC\server\share\bin").as_deref(),
            Some(r"\\server\share\bin")
        );
        // A plain path has no verbatim prefix -> None (left untouched
        // upstream).
        assert_eq!(strip_verbatim_str(r"C:\Users\rnio\bin"), None);
        assert_eq!(strip_verbatim_str("/usr/local/bin"), None);
    }

    #[test]
    fn normalize_tag_strips_leading_v_only() {
        // `v0.6.5` (release tag) must compare equal to `0.6.5` (installed).
        assert_eq!(normalize_tag("v0.6.5"), "0.6.5");
        assert_eq!(normalize_tag("0.6.5"), "0.6.5");
        // Only a *leading* v is stripped — nothing else is touched.
        assert_eq!(normalize_tag("v1.2.3-rc.1"), "1.2.3-rc.1");
        assert_eq!(normalize_tag(""), "");
    }

    fn root_dirs(roots: &[InstallRoot]) -> Vec<String> {
        roots
            .iter()
            .map(|root| root.dir.display().to_string())
            .collect()
    }

    fn anchors_of(root: &InstallRoot) -> Vec<Anchor> {
        root.anchored_by.clone()
    }

    #[test]
    fn upsert_dedupes_same_dir_and_merges_anchors() {
        let mut roots: Vec<InstallRoot> = Vec::new();
        // A non-existent path won't canonicalise, so the raw path is the key.
        let dir = std::path::PathBuf::from("/nonexistent/uffs/root");
        upsert_root(&mut roots, dir.clone(), Anchor::Cli);
        upsert_root(&mut roots, dir, Anchor::Daemon);
        assert_eq!(roots.len(), 1, "same dir must not create a second root");
        let first = roots.first().expect("one root");
        assert_eq!(anchors_of(first), vec![Anchor::Cli, Anchor::Daemon]);
    }

    #[test]
    fn upsert_keeps_distinct_dirs_separate() {
        let mut roots: Vec<InstallRoot> = Vec::new();
        upsert_root(&mut roots, "/nonexistent/a".into(), Anchor::Cli);
        upsert_root(&mut roots, "/nonexistent/b".into(), Anchor::Daemon);
        assert_eq!(roots.len(), 2);
        assert_eq!(root_dirs(&roots), vec!["/nonexistent/a", "/nonexistent/b"]);
    }

    #[test]
    fn upsert_same_anchor_twice_is_idempotent() {
        let mut roots: Vec<InstallRoot> = Vec::new();
        upsert_root(&mut roots, "/nonexistent/a".into(), Anchor::Cli);
        upsert_root(&mut roots, "/nonexistent/a".into(), Anchor::Cli);
        let first = roots.first().expect("one root");
        assert_eq!(anchors_of(first), vec![Anchor::Cli]);
    }
}
