// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Update execution plan — the per-root "what will actually happen", derived
//! from Phase-A detection. This mirrors the uninstall flow's `RemovalPlan`:
//! every discovered root becomes an item with an action and a `needs_elevation`
//! verdict, so the shared elevation gate ([`crate::commands::elevation`]) can
//! surface admin-only work **up front** and the final summary never lists work
//! that will not happen.
//!
//! ## Elevation model (per root)
//!
//! | Root              | Action           | `needs_elevation`                    |
//! |-------------------|------------------|--------------------------------------|
//! | already current   | skip             | — (skipped, no daemon stop)          |
//! | dev-build/unknown | skip (never)     | — (skipped)                          |
//! | unmanaged         | replace in place | dir not writable **or** a component here can't be stopped without elevation |
//! | winget (user)     | `winget upgrade` | no — but must run **non-elevated**    |
//! | winget (machine)  | `winget upgrade` | yes                                  |
//!
//! Two rules keep an install the caller can't fully touch from failing mid-swap
//! (the elevated-daemon 20 s-rollback bug):
//!
//! 1. **Already current** — a root whose every binary is at the target version
//!    is a pure skip, so an already-current `~\bin` hosting an *elevated*
//!    daemon is never dragged in (its daemon is never stopped).
//! 2. **Stop cost — but only where the stop is gated.** Replacing an
//!    **unmanaged** root means the uffs-update helper stops the daemon via the
//!    *gated* CLI `daemon stop` (no UAC), so an elevated daemon (or a broker)
//!    hosted there makes the root elevation-required (fix #1's
//!    `mutating_management_needs_elevation`). A **winget** root is different:
//!    `winget::quiesce` stops a daemon inside the package *cooperatively* (an
//!    IPC shutdown the daemon honors regardless of elevation — not the gated
//!    CLI path) and cycles the package broker via its own one-UAC path, so a
//!    winget root is never gated for a running component.
//!
//! The winget-user-scope-while-elevated case is **not** an elevation item —
//! winget *refuses* to upgrade a user package from an elevated shell — so it is
//! surfaced as a "run from a normal terminal" delegation note, the inverse of
//! the elevation gate.

use std::path::Path;

use super::model::{Channel, Component, DetectionReport, InstallRoot, Scope};
use crate::commands::elevation::ElevatablePlan;

/// What the update will do to a given root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    /// Replace the binaries in place from the freshly-downloaded release.
    ReplaceBinaries,
    /// Delegate to `winget upgrade` (a winget-managed root).
    WingetUpgrade,
    /// A dev-build (`target/{debug,release}`) tree — never auto-updated.
    SkipDevBuild,
    /// The path could not be classified — left untouched.
    SkipUnknown,
    /// Every binary here is already at the target version — nothing to do (and,
    /// crucially, no running daemon here needs stopping).
    AlreadyCurrent,
}

impl UpdateAction {
    /// Whether this action actually mutates the root (vs. a skip).
    pub(crate) const fn is_mutating(self) -> bool {
        matches!(self, Self::ReplaceBinaries | Self::WingetUpgrade)
    }

    /// Short human label for report output.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ReplaceBinaries => "replace binaries",
            Self::WingetUpgrade => "winget upgrade",
            Self::SkipDevBuild => "skip (dev-build)",
            Self::SkipUnknown => "skip (unclassified)",
            Self::AlreadyCurrent => "skip (already current)",
        }
    }
}

/// One planned update action against a discovered root.
#[derive(Debug, Clone)]
pub(crate) struct UpdateItem {
    /// The root directory this item targets (for display + apply).
    pub(crate) dir: std::path::PathBuf,
    /// The install channel that placed the binaries.
    pub(crate) channel: Channel,
    /// The install scope (meaningful for winget).
    pub(crate) scope: Scope,
    /// What will happen to this root.
    pub(crate) action: UpdateAction,
    /// True when the action cannot proceed without Administrator (a
    /// non-writable target, or a machine-scope winget upgrade).
    pub(crate) needs_elevation: bool,
    /// True when this is a winget-user root that must be upgraded from a
    /// **non-elevated** terminal — the inverse of `needs_elevation`. Surfaced
    /// as a delegation note, never routed through the elevation gate.
    pub(crate) needs_non_elevated: bool,
}

impl UpdateItem {
    /// Human-readable one-line description for the elevation gate / summary.
    pub(crate) fn describe(&self) -> String {
        format!(
            "{} ({}{}) — {}",
            self.dir.display(),
            self.channel.label(),
            match self.scope {
                Scope::Unknown => String::new(),
                scope @ (Scope::User | Scope::Machine) => format!("/{}", scope.label()),
            },
            self.action.label(),
        )
    }
}

/// The full update execution plan: a per-root item list. The target tag is
/// carried by the caller ([`super::run_automatic_update`]), not duplicated
/// here.
#[derive(Debug, Clone)]
pub(crate) struct UpdatePlan {
    /// Per-root planned actions.
    pub(crate) items: Vec<UpdateItem>,
}

impl UpdatePlan {
    /// Build the plan by classifying every detected root against `latest`.
    pub(crate) fn build(report: &DetectionReport, latest: &str) -> Self {
        let latest_norm = super::normalize_tag(latest);
        let elevated = uffs_mft::platform::is_elevated();
        // fix #1's exact gate: can this terminal stop/restart the resident
        // daemon at all? An elevated daemon with no serving broker → no.
        let daemon_stop_needs_elev =
            crate::commands::daemon_mgmt::mutating_management_needs_elevation();
        let items = report
            .roots
            .iter()
            .map(|root| classify_root(root, latest_norm, report, elevated, daemon_stop_needs_elev))
            .collect::<Vec<_>>();
        Self { items }
    }

    /// The items that will actually mutate a root (drop the skips).
    pub(crate) fn mutating_items(&self) -> impl Iterator<Item = &UpdateItem> {
        self.items.iter().filter(|item| item.action.is_mutating())
    }

    /// When running **elevated**, split out the winget-user roots — `winget`
    /// refuses to upgrade a user package from an elevated shell — returning
    /// them for the "run from a normal terminal" delegation note and
    /// removing them from the plan. Non-elevated: nothing to split (they
    /// upgrade fine here).
    pub(crate) fn split_off_non_elevated_when_elevated(
        &mut self,
        elevated: bool,
    ) -> Vec<UpdateItem> {
        if !elevated {
            return Vec::new();
        }
        let (split, keep): (Vec<_>, Vec<_>) = core::mem::take(&mut self.items)
            .into_iter()
            .partition(|item| item.needs_non_elevated);
        self.items = keep;
        split
    }

    /// Whether the plan has any real work left after skips.
    pub(crate) fn has_work(&self) -> bool {
        self.mutating_items().next().is_some()
    }
}

/// Classify one root into a planned action + elevation verdict per the module's
/// elevation model.
fn classify_root(
    root: &InstallRoot,
    latest_norm: &str,
    report: &DetectionReport,
    elevated: bool,
    daemon_stop_needs_elev: bool,
) -> UpdateItem {
    // Already at the target version → never touched, and (crucially) its
    // running daemon is never stopped. This is what keeps an
    // already-current `~\bin` hosting an elevated daemon out of the update
    // entirely.
    if root_already_current(root, latest_norm) {
        return skip_item(root, UpdateAction::AlreadyCurrent);
    }

    let (action, needs_elevation, needs_non_elevated) = match root.channel {
        Channel::DevBuild => (UpdateAction::SkipDevBuild, false, false),
        Channel::Unknown => (UpdateAction::SkipUnknown, false, false),
        // Unmanaged: the uffs-update helper stops the daemon via the *gated*
        // CLI `daemon stop` (no UAC), so an elevated daemon (or a broker)
        // hosted here genuinely needs elevation — as does a non-writable dir.
        Channel::Unmanaged => (
            UpdateAction::ReplaceBinaries,
            !dir_writable(&root.dir)
                || root_stop_needs_elevation(root, report, elevated, daemon_stop_needs_elev),
            false,
        ),
        // winget's own `quiesce` stops a daemon inside the package
        // **cooperatively** (an IPC shutdown the daemon honors regardless of
        // its elevation — not the gated CLI `daemon stop`) and cycles the
        // package broker via its own one-UAC path. So a winget root is never
        // gated here; it only needs the privilege level winget itself demands:
        // user scope must run non-elevated (winget refuses a user package
        // elevated), machine scope requires Administrator.
        Channel::WinGet => match root.scope {
            Scope::Machine => (UpdateAction::WingetUpgrade, true, false),
            Scope::User | Scope::Unknown => (UpdateAction::WingetUpgrade, false, true),
        },
    };
    UpdateItem {
        dir: root.dir.clone(),
        channel: root.channel,
        scope: root.scope,
        action,
        needs_elevation,
        needs_non_elevated,
    }
}

/// A non-mutating item (a skip) — never needs elevation either way.
fn skip_item(root: &InstallRoot, action: UpdateAction) -> UpdateItem {
    UpdateItem {
        dir: root.dir.clone(),
        channel: root.channel,
        scope: root.scope,
        action,
        needs_elevation: false,
        needs_non_elevated: false,
    }
}

/// Whether every binary in `root` already reports the target version — an
/// update would be a pointless swap (and a pointless daemon stop).
fn root_already_current(root: &InstallRoot, latest_norm: &str) -> bool {
    !root.binaries.is_empty()
        && root
            .binaries
            .iter()
            .all(|bin| bin.version.as_deref() == Some(latest_norm))
}

/// Whether a component running from `root` cannot be stopped without elevation:
/// the broker (a `LocalSystem` service) always needs it; the daemon/mcp inherit
/// fix #1's gate (`daemon_stop_needs_elev` — an elevated daemon with no serving
/// broker). The root must be stopped before it can be replaced, so this cost is
/// the root's cost.
fn root_stop_needs_elevation(
    root: &InstallRoot,
    report: &DetectionReport,
    elevated: bool,
    daemon_stop_needs_elev: bool,
) -> bool {
    report.running.iter().any(|process| {
        process.image_path.as_deref().and_then(Path::parent) == Some(root.dir.as_path())
            && match process.component {
                Component::Broker => !elevated,
                Component::Daemon | Component::Mcp => daemon_stop_needs_elev,
            }
    })
}

/// Probe whether `dir` is writable by the current user without elevation, by
/// creating and immediately removing a uniquely-named temp file. A missing dir
/// or any I/O error is treated as "not writable" — the caller then routes the
/// item through the elevation gate rather than failing mid-apply.
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".uffs-write-probe-{}", std::process::id()));
    std::fs::File::create(&probe).is_ok_and(|file| {
        drop(file);
        drop(std::fs::remove_file(&probe));
        true
    })
}

impl ElevatablePlan for UpdatePlan {
    fn requires_elevation(&self) -> bool {
        self.items.iter().any(|item| item.needs_elevation)
    }

    fn drop_elevation_required(&mut self) -> Vec<String> {
        let dropped = self
            .items
            .iter()
            .filter(|item| item.needs_elevation)
            .map(UpdateItem::describe)
            .collect();
        self.items.retain(|item| !item.needs_elevation);
        dropped
    }

    fn render_elevation_needed(&self) {
        render_elevation_needed(self);
    }
}

/// Print the "these roots need Administrator" preamble for the elevation gate.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn render_elevation_needed(plan: &UpdatePlan) {
    println!(
        "\nThis terminal is not elevated (Administrator). The following can only\n\
         be updated from an elevated terminal:\n"
    );
    for item in plan.items.iter().filter(|item| item.needs_elevation) {
        println!("  - {}", item.describe());
    }
}

/// Summary for roots dropped at the elevation gate (the user chose to continue
/// without them). Nothing is printed when none were dropped.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn print_skipped_elevation(dropped: &[String]) {
    if dropped.is_empty() {
        return;
    }
    println!("\nLeft for an elevated run (re-run `uffs --update` as Administrator):");
    for item in dropped {
        println!("  - {item}");
    }
}

/// Note the winget-user roots that must be upgraded from a NON-elevated
/// terminal (winget refuses a user package when the shell is elevated). Nothing
/// is printed when there are none (i.e. this terminal is not elevated).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn print_winget_delegation(delegated: &[UpdateItem]) {
    if delegated.is_empty() {
        return;
    }
    println!(
        "\nThis terminal is elevated; `winget` will not upgrade a user-scope\n\
         package from an elevated shell. Re-run `uffs --update` from a normal\n\
         (non-elevated) terminal to update:"
    );
    for item in delegated {
        println!("  - {}", item.describe());
    }
}

/// Printed when the gate + winget delegation left nothing this terminal can do.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn print_no_local_work() {
    println!("\nNothing left to update in this terminal — see the note(s) above.");
}

/// A detection report pruned to just the roots the gated `plan` will touch, so
/// the journaled execute (snapshot → quiesce → apply → winget) never operates
/// on a dropped (elevation-required or winget-delegated) root.
pub(crate) fn prune_report(report: &DetectionReport, plan: &UpdatePlan) -> DetectionReport {
    let keep: std::collections::HashSet<&Path> = plan
        .mutating_items()
        .map(|item| item.dir.as_path())
        .collect();
    let roots = report
        .roots
        .iter()
        .filter(|root| keep.contains(root.dir.as_path()))
        .cloned()
        .collect();
    let running = report
        .running
        .iter()
        .filter(|process| {
            process
                .image_path
                .as_deref()
                .and_then(Path::parent)
                .is_some_and(|dir| keep.contains(dir))
        })
        .cloned()
        .collect();
    DetectionReport { roots, running }
}

#[cfg(test)]
mod tests {
    use super::{UpdateAction, UpdateItem, UpdatePlan, classify_root};
    use crate::commands::elevation::ElevatablePlan as _;
    use crate::commands::update::model::{
        BinaryInfo, Channel, Component, DetectionReport, InstallRoot, RunningProcess, Scope,
    };

    /// A root with the given channel/scope and no binaries/anchors.
    fn root(dir: &str, channel: Channel, scope: Scope) -> InstallRoot {
        InstallRoot {
            dir: dir.into(),
            channel,
            scope,
            anchored_by: Vec::new(),
            binaries: Vec::new(),
        }
    }

    /// One binary at a version, for the already-current check.
    fn bin(name: &str, version: &str) -> BinaryInfo {
        BinaryInfo {
            name: name.to_owned(),
            version: Some(version.to_owned()),
        }
    }

    /// A report with a single daemon running from `dir`.
    fn report_with_daemon(dir: &std::path::Path) -> DetectionReport {
        DetectionReport {
            roots: Vec::new(),
            running: vec![RunningProcess {
                component: Component::Daemon,
                pid: 1,
                image_path: Some(dir.join("uffsd.exe")),
                command_line: None,
                version: None,
            }],
        }
    }

    /// A path string for a directory the current user can write without
    /// elevation (the OS temp dir), so `dir_writable` returns `true`.
    fn writable_dir() -> String {
        std::env::temp_dir().to_string_lossy().into_owned()
    }

    /// Classify with no running components, non-elevated, and a target no empty
    /// root can already match — the pure channel/scope path.
    fn classify(root: &InstallRoot) -> UpdateItem {
        classify_root(root, "9.9.9", &DetectionReport::default(), false, false)
    }

    #[test]
    fn dev_build_and_unknown_are_skipped() {
        let dev = classify(&root("/x", Channel::DevBuild, Scope::Unknown));
        assert_eq!(dev.action, UpdateAction::SkipDevBuild);
        assert!(!dev.needs_elevation);
        assert!(!dev.needs_non_elevated);

        let unknown = classify(&root("/x", Channel::Unknown, Scope::Unknown));
        assert_eq!(unknown.action, UpdateAction::SkipUnknown);
    }

    #[test]
    fn winget_user_needs_non_elevated_never_elevation() {
        let item = classify(&root("/x", Channel::WinGet, Scope::User));
        assert_eq!(item.action, UpdateAction::WingetUpgrade);
        assert!(item.needs_non_elevated, "user scope must run non-elevated");
        assert!(
            !item.needs_elevation,
            "winget refuses a user package elevated"
        );
    }

    #[test]
    fn winget_machine_needs_elevation() {
        let item = classify(&root("/x", Channel::WinGet, Scope::Machine));
        assert!(item.needs_elevation);
        assert!(!item.needs_non_elevated);
    }

    #[test]
    fn unmanaged_writable_dir_needs_no_elevation() {
        let item = classify(&root(&writable_dir(), Channel::Unmanaged, Scope::Unknown));
        assert_eq!(item.action, UpdateAction::ReplaceBinaries);
        assert!(!item.needs_elevation);
    }

    #[test]
    fn root_already_at_target_is_a_pure_skip_even_with_an_elevated_daemon() {
        let mut install = root(&writable_dir(), Channel::Unmanaged, Scope::Unknown);
        install.binaries = vec![bin("uffs", "0.6.24"), bin("uffsd", "0.6.24")];
        // Elevated daemon running here + non-elevated caller: still a pure
        // skip, because there is nothing to update — so the daemon is
        // never stopped.
        let report = report_with_daemon(&install.dir);
        let item = classify_root(&install, "0.6.24", &report, false, true);
        assert_eq!(item.action, UpdateAction::AlreadyCurrent);
        assert!(!item.needs_elevation);
    }

    #[test]
    fn winget_user_root_hosting_a_daemon_is_not_gated() {
        // A daemon running from a winget-user root (even elevated) must NOT be
        // gated: winget::quiesce stops it cooperatively (IPC shutdown, no
        // elevation), so the upgrade runs non-elevated. Gating it would give
        // the dead-end "re-run elevated" hint winget can't honor.
        let dir = writable_dir();
        let mut install = root(&dir, Channel::WinGet, Scope::User);
        install.binaries = vec![bin("uffs", "0.6.18")]; // out of date
        let report = report_with_daemon(&install.dir);
        let item = classify_root(&install, "0.6.24", &report, false, true);
        assert_eq!(item.action, UpdateAction::WingetUpgrade);
        assert!(
            !item.needs_elevation,
            "winget::quiesce stops the daemon cooperatively — no elevation"
        );
        assert!(item.needs_non_elevated);
    }

    #[test]
    fn out_of_date_root_hosting_unstoppable_daemon_forces_elevation() {
        let mut install = root(&writable_dir(), Channel::Unmanaged, Scope::Unknown);
        install.binaries = vec![bin("uffs", "0.6.18")]; // out of date → needs updating
        let report = report_with_daemon(&install.dir);
        // daemon_stop_needs_elev = true (elevated daemon, no serving broker):
        // the writable dir no longer makes it doable — stopping the daemon to
        // swap the binaries needs Administrator.
        let item = classify_root(&install, "0.6.24", &report, false, true);
        assert_eq!(item.action, UpdateAction::ReplaceBinaries);
        assert!(
            item.needs_elevation,
            "an unstoppable daemon in the root forces elevation"
        );
    }

    #[test]
    fn split_off_non_elevated_only_when_elevated() {
        let build = || UpdatePlan {
            items: vec![
                classify(&root("/a", Channel::WinGet, Scope::User)),
                classify(&root(&writable_dir(), Channel::Unmanaged, Scope::Unknown)),
            ],
        };

        // Non-elevated: nothing is split off — winget upgrades fine here.
        let mut plan_non_elevated = build();
        assert!(
            plan_non_elevated
                .split_off_non_elevated_when_elevated(false)
                .is_empty()
        );
        assert_eq!(plan_non_elevated.items.len(), 2);

        // Elevated: the winget-user item moves to the delegation list.
        let mut plan_elevated = build();
        let split = plan_elevated.split_off_non_elevated_when_elevated(true);
        assert_eq!(split.len(), 1);
        assert_eq!(plan_elevated.items.len(), 1);
    }

    #[test]
    fn drop_elevation_required_prunes_but_keeps_doable_work() {
        let mut plan = UpdatePlan {
            items: vec![
                classify(&root("/m", Channel::WinGet, Scope::Machine)),
                classify(&root(&writable_dir(), Channel::Unmanaged, Scope::Unknown)),
            ],
        };
        assert!(plan.requires_elevation());
        assert_eq!(plan.drop_elevation_required().len(), 1);
        assert!(!plan.requires_elevation());
        assert!(
            plan.has_work(),
            "the writable unmanaged root is still doable"
        );
    }
}
