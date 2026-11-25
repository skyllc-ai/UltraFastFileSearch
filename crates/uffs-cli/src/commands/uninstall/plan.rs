// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Removal-plan construction for `uffs --uninstall` (task U-20 of
//! `docs/dev/architecture/UFFS-Uninstall-Implementation-Plan.md`).
//!
//! Pure: turns the analysis ([`DetectionReport`] + [`Inventory`]) into an
//! ordered, itemized [`RemovalPlan`], honoring `--keep-config` / `--scope`.
//! No IO, fully unit-tested. `WinGet` roots become a `winget uninstall`
//! delegation, never a hand-delete (design §7).
//!
//! Each [`PlanItem`] carries a structured [`PlanTarget`] — the single source of
//! truth that both the renderer (description / `--json`) and the executor
//! (M4 `remove`) consume, so what is shown is exactly what is removed.

use std::path::{Path, PathBuf};

use super::args::{UninstallArgs, UninstallScope};
use super::inventory::{ArtifactKind, BrokerServiceState, Inventory};
#[cfg(windows)]
use super::sweep::StrayHit;
use crate::commands::elevation::ElevatablePlan;
use crate::commands::update::model::{Channel, Component, DetectionReport, InstallRoot, Scope};

/// The `WinGet` package id UFFS publishes under.
pub(crate) const WINGET_PACKAGE_ID: &str = "SkyLLC.UFFS";

/// Heading of the shutdown group (daemon stop + broker service removal). Shared
/// so `RemovalPlan::ensure_daemon_shutdown` (Windows-only) can find / recreate
/// it verbatim.
const SHUTDOWN_GROUP_TITLE: &str = "Shutdown (stopped last)";

/// Heading of the data / cache / config group (the shutdown group must precede
/// it — a running daemon holds handles inside these dirs).
const DATA_GROUP_TITLE: &str = "Data / cache / config";

/// Heading of the runtime-binaries group (deletable only after shutdown).
const RUNTIME_GROUP_TITLE: &str = "Runtime binaries (after shutdown)";

/// The concrete target of a plan item: everything the executor needs, and
/// everything the renderer describes. Group ordering (in [`build_plan`]) plus
/// this discriminant define the safe removal order.
#[derive(Debug, Clone)]
pub(crate) enum PlanTarget {
    /// Stop a running UFFS process (daemon / MCP gateway).
    StopProcess {
        /// Component label (e.g. `daemon`).
        component: String,
        /// OS process id.
        pid: u32,
    },
    /// Stop + delete the broker Windows service.
    RemoveService {
        /// Service name (`UffsAccessBroker`).
        service: String,
    },
    /// Delete the UFFS binaries in an unmanaged / dev-build root.
    DeleteBinaries {
        /// The root directory.
        dir: PathBuf,
        /// The binary stems present in the root (no `.exe` suffix).
        stems: Vec<String>,
    },
    /// Delegate a `WinGet`-managed root to `winget uninstall`.
    DelegateWinget {
        /// The package id to uninstall.
        package_id: String,
        /// The root's install scope (user / machine).
        scope: Scope,
        /// The root directory (for the description).
        dir: PathBuf,
    },
    /// Recursively delete a data / cache / config directory.
    DeleteDir {
        /// The directory to remove.
        path: PathBuf,
        /// The artifact-kind label (e.g. `cache`), for the description.
        label: &'static str,
    },
    /// Remove a (provably UFFS) directory from PATH.
    RemovePathEntry {
        /// The PATH entry to remove.
        dir: PathBuf,
    },
    /// Delete a single stray UFFS file the deep sweep found outside the known
    /// roots. Confirmed separately from the main plan. Windows-only (the deep
    /// sweep does not run off Windows).
    #[cfg(windows)]
    DeleteFile {
        /// Absolute path of the stray file.
        path: PathBuf,
        /// Parsed `--version` if it is a probeable binary (display only).
        version: Option<String>,
    },
}

impl PlanTarget {
    /// Short verb label (used in `--json`).
    pub(crate) const fn action_label(&self) -> &'static str {
        match *self {
            Self::StopProcess { .. } => "stop-process",
            Self::RemoveService { .. } => "remove-service",
            Self::DeleteBinaries { .. } => "delete-binaries",
            Self::DelegateWinget { .. } => "delegate-winget",
            Self::DeleteDir { .. } => "delete-dir",
            Self::RemovePathEntry { .. } => "remove-path-entry",
            #[cfg(windows)]
            Self::DeleteFile { .. } => "delete-file",
        }
    }

    /// Human, one-line description of the target.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::StopProcess { component, pid } => format!("{component} (pid {pid})"),
            Self::RemoveService { service } => format!("Stop + delete service {service}"),
            Self::DeleteBinaries { dir, stems } => {
                format!("{} binaries in {}", stems.len(), dir.display())
            }
            Self::DelegateWinget {
                package_id,
                scope,
                dir,
            } => format!(
                "winget uninstall {package_id}  ({} root: {})",
                scope.label(),
                dir.display()
            ),
            Self::DeleteDir { path, label } => format!("{label} ({})", path.display()),
            Self::RemovePathEntry { dir } => format!("PATH entry {}", dir.display()),
            #[cfg(windows)]
            Self::DeleteFile { path, version } => version.as_ref().map_or_else(
                || path.display().to_string(),
                |ver| format!("{} (v{ver})", path.display()),
            ),
        }
    }
}

/// Coarse scope of a plan item, for `--scope` filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemScope {
    /// A per-user artifact (`%LOCALAPPDATA%`, a user-scope root).
    User,
    /// A machine-wide artifact (the service, a `%PROGRAMFILES%` root).
    Machine,
    /// Scope-agnostic (a running process).
    Any,
}

/// One unit of removal work.
#[derive(Debug, Clone)]
pub(crate) struct PlanItem {
    /// What to remove (structured; drives both render and execute).
    pub(crate) target: PlanTarget,
    /// Whether performing it requires Administrator.
    pub(crate) needs_elevation: bool,
    /// Coarse scope, for `--scope` filtering.
    pub(crate) scope: ItemScope,
    /// Bytes this item reclaims (0 for non-filesystem actions).
    pub(crate) bytes: u64,
}

/// A named, ordered group of plan items (Services, Processes, ...).
#[derive(Debug, Clone)]
pub(crate) struct PlanGroup {
    /// Group heading.
    pub(crate) title: &'static str,
    /// Items in the group.
    pub(crate) items: Vec<PlanItem>,
}

/// The full ordered removal plan.
#[derive(Debug, Clone, Default)]
pub(crate) struct RemovalPlan {
    /// Groups in safe removal order.
    pub(crate) groups: Vec<PlanGroup>,
}

impl ElevatablePlan for RemovalPlan {
    /// Whether any item requires Administrator.
    fn requires_elevation(&self) -> bool {
        self.items().any(|item| item.needs_elevation)
    }

    /// Drop every item that needs Administrator (the broker service + its
    /// process), removing any group left empty. Lets a non-elevated run remove
    /// everything it *can* and leave the broker for an elevated re-run. Returns
    /// the dropped items' descriptions so the final summary can list exactly
    /// what this run skips.
    fn drop_elevation_required(&mut self) -> Vec<String> {
        let mut dropped: Vec<String> = Vec::new();
        for group in &mut self.groups {
            group.items.retain(|item| {
                if item.needs_elevation {
                    dropped.push(item.target.describe());
                    return false;
                }
                true
            });
        }
        self.groups.retain(|group| !group.items.is_empty());
        dropped
    }

    /// The non-elevated preamble listing the admin-only items (the broker).
    fn render_elevation_needed(&self) {
        super::render::print_elevation_gate(self);
    }
}

impl RemovalPlan {
    /// Iterate every item across all groups, in order.
    pub(crate) fn items(&self) -> impl Iterator<Item = &PlanItem> {
        self.groups.iter().flat_map(|group| &group.items)
    }

    /// Total bytes the plan would reclaim.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.items()
            .map(|item| item.bytes)
            .fold(0, u64::saturating_add)
    }

    /// Fill in the reclaim bytes of every binary-delete item, so the summary's
    /// "Reclaims ~N" reflects the binaries too (not just the data dirs).
    /// Statting files is IO, which this pure module leaves to the caller:
    /// `size_of` maps a `(dir, stems)` binary-delete target to its on-disk
    /// total (best-effort — an absent file contributes 0). `WinGet`
    /// delegations and directory / process items are untouched (winget owns
    /// its bytes; dir sizes already came from the inventory).
    pub(crate) fn size_binaries(&mut self, size_of: impl Fn(&Path, &[String]) -> u64) {
        for item in self.groups.iter_mut().flat_map(|group| &mut group.items) {
            if let PlanTarget::DeleteBinaries { dir, stems } = &item.target {
                item.bytes = size_of(dir, stems);
            }
        }
    }

    /// Make sure the plan stops the daemon at `pid` before its binary is
    /// deleted. The deep sweep can *start* the daemon (the no-broker path's UAC
    /// start) **after** the plan was snapshotted, so `report.running` had none
    /// and the shutdown group carries no stop for it — without this the
    /// freshly-started, possibly elevated daemon keeps its image locked and the
    /// runtime-binary delete fails with Access-denied. No-op when a daemon stop
    /// already exists. The executor stops it with a graceful shutdown RPC (no
    /// caller elevation needed), so the elevation obtained to *start* it need
    /// not be re-acquired to stop it.
    #[cfg(windows)]
    pub(crate) fn ensure_daemon_shutdown(&mut self, pid: u32) {
        let already = self.items().any(|item| {
            matches!(&item.target, PlanTarget::StopProcess { component, .. } if component == "daemon")
        });
        if already {
            return;
        }
        let stop = PlanItem {
            target: PlanTarget::StopProcess {
                component: Component::Daemon.label().to_owned(),
                pid,
            },
            needs_elevation: false,
            scope: ItemScope::Any,
            bytes: 0,
        };
        // Prepend to the existing shutdown group, or create it just before the
        // data / runtime-binary groups it must precede (Windows locks the image
        // of a running process, so the stop has to run first).
        if let Some(group) = self
            .groups
            .iter_mut()
            .find(|group| group.title == SHUTDOWN_GROUP_TITLE)
        {
            group.items.insert(0, stop);
            return;
        }
        let at = self
            .groups
            .iter()
            .position(|group| group.title == DATA_GROUP_TITLE || group.title == RUNTIME_GROUP_TITLE)
            .unwrap_or(self.groups.len());
        self.groups.insert(at, PlanGroup {
            title: SHUTDOWN_GROUP_TITLE,
            items: vec![stop],
        });
    }

    /// Number of items across all groups.
    pub(crate) fn item_count(&self) -> usize {
        self.groups.iter().map(|group| group.items.len()).sum()
    }

    /// True when there is nothing to remove.
    pub(crate) fn is_empty(&self) -> bool {
        self.item_count() == 0
    }
}

/// Build the ordered removal plan from the analysis + flags.
/// `removable_path_dirs` is the already-vetted set of PATH directories safe to
/// drop — dedicated UFFS roots only (see
/// [`super::analyze::removable_path_dirs`]). A shared bin dir is never in it,
/// so PATH cleanup never touches the user's general toolchain location.
pub(crate) fn build_plan(
    report: &DetectionReport,
    inventory: &Inventory,
    args: &UninstallArgs,
    removable_path_dirs: &[PathBuf],
) -> RemovalPlan {
    let mut groups: Vec<PlanGroup> = Vec::new();

    // The working tools stay alive until the very end: tool binaries first,
    // then PATH, then the shutdown of the running parts (daemon process +
    // broker service), then the data dirs they had open, and finally the
    // runtime binaries whose images were locked until that shutdown. The
    // running uffs.exe / uffs-update.exe are deferred past process exit
    // (self-delete) by the executor.

    // 1. Tool binaries — per root (unmanaged/dev roots only). The runtime
    // binaries (daemon, broker, MCP servers) AND the winget delegation are
    // deferred to the final group below: their images are locked while those
    // processes/services run, and winget cannot delete locked files either.
    let binaries: Vec<PlanItem> = report
        .roots
        .iter()
        .filter_map(|root| binary_item(root, StemSet::Tools))
        .collect();
    push_group(&mut groups, "Binaries", binaries, args.scope);

    // 2. PATH entries that point at a removed unmanaged/dev root that is
    // *dedicated* to UFFS (only uffs* files) — provably ours, so safe to drop.
    // A shared bin dir (~/bin, ~/.local/bin) is filtered out upstream and
    // never appears here. WinGet roots are managed by winget. Skipped under
    // --no-path.
    if !args.no_path {
        let path_items: Vec<PlanItem> = report
            .roots
            .iter()
            .filter(|root| !root.binaries.is_empty() && !matches!(root.channel, Channel::WinGet))
            .filter(|root| {
                removable_path_dirs
                    .iter()
                    .any(|dir| paths_equal_ignore_case(dir, &root.dir))
            })
            .map(|root| {
                let machine = matches!(root.scope, Scope::Machine);
                PlanItem {
                    target: PlanTarget::RemovePathEntry {
                        dir: root.dir.clone(),
                    },
                    needs_elevation: machine,
                    scope: if machine {
                        ItemScope::Machine
                    } else {
                        ItemScope::User
                    },
                    bytes: 0,
                }
            })
            .collect();
        push_group(&mut groups, "PATH", path_items, args.scope);
    }

    // 3. Shutdown of the running parts — LAST among the live pieces, so the
    // tooling stays usable during the run. The broker is a LocalSystem
    // **service** — `taskkill` can't stop it (returns exit 128, and the SCM
    // would just restart it), so it is never a StopProcess item; the
    // RemoveService item stops + deletes it via `sc`. The daemon / MCP are
    // ordinary user-owned processes, so a plain stop applies and needs no
    // admin. (At execution the daemon is re-discovered by its pid file — the
    // analyzed pid can go stale when the deep sweep reloads it.)
    let mut shutdown: Vec<PlanItem> = report
        .running
        .iter()
        .filter(|process| !matches!(process.component, Component::Broker))
        .map(|process| PlanItem {
            target: PlanTarget::StopProcess {
                component: process.component.label().to_owned(),
                pid: process.pid,
            },
            needs_elevation: false,
            scope: ItemScope::Any,
            bytes: 0,
        })
        .collect();
    if inventory.broker_service == BrokerServiceState::Installed {
        shutdown.push(PlanItem {
            target: PlanTarget::RemoveService {
                service: uffs_broker_protocol::SERVICE_NAME.to_owned(),
            },
            needs_elevation: true,
            scope: ItemScope::Machine,
            bytes: 0,
        });
    }
    push_group(&mut groups, SHUTDOWN_GROUP_TITLE, shutdown, args.scope);

    // 4. Data / cache / config dirs that exist (skip config under
    // --keep-config). After the daemon shutdown: a running daemon holds open
    // handles (pid file, socket, mmap'd caches) inside these dirs.
    let dirs: Vec<PlanItem> = inventory
        .dirs
        .iter()
        .filter(|dir| dir.exists)
        .filter(|dir| !(args.keep_config && dir.kind == ArtifactKind::Config))
        .map(|dir| PlanItem {
            target: PlanTarget::DeleteDir {
                path: dir.path.clone(),
                label: dir.kind.label(),
            },
            needs_elevation: false,
            scope: ItemScope::User,
            bytes: dir.size_bytes,
        })
        .collect();
    push_group(&mut groups, DATA_GROUP_TITLE, dirs, args.scope);

    // 5. Runtime binaries — deletable only now that their processes/services
    // are stopped (Windows locks a running image).
    let runtime: Vec<PlanItem> = report
        .roots
        .iter()
        .filter_map(|root| binary_item(root, StemSet::Runtime))
        .collect();
    push_group(&mut groups, RUNTIME_GROUP_TITLE, runtime, args.scope);

    RemovalPlan { groups }
}

/// Build a one-group plan for the stray files the deep sweep found outside the
/// known roots. Presented + confirmed **separately** from the main plan (a copy
/// the user placed themselves might be among them). Each item is a best-effort
/// single-file delete; none require elevation up front — a protected location
/// simply fails best-effort and is reported. Windows-only (the deep sweep does
/// not run off Windows).
#[cfg(windows)]
pub(crate) fn build_stray_plan(strays: &[StrayHit]) -> RemovalPlan {
    if strays.is_empty() {
        return RemovalPlan::default();
    }
    let items: Vec<PlanItem> = strays
        .iter()
        .map(|stray| PlanItem {
            target: PlanTarget::DeleteFile {
                path: stray.path.clone(),
                version: stray.version.clone(),
            },
            needs_elevation: false,
            scope: ItemScope::Any,
            bytes: 0,
        })
        .collect();
    RemovalPlan {
        groups: vec![PlanGroup {
            title: "Found elsewhere (deep sweep)",
            items,
        }],
    }
}

/// Case-insensitive path equality (Windows file systems + PATH entries vary in
/// case; a redundant exact match is what we require before touching PATH).
fn paths_equal_ignore_case(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

/// Binary stems whose images are locked while the resident parts run (the
/// daemon, the broker service, the MCP servers, the watchdog). Deleted in the
/// final plan group, after the shutdown items; every other stem is a plain
/// tool binary.
const RUNTIME_STEMS: &[&str] = &[
    "uffsd",
    "uffs-broker",
    "uffsmcp",
    "uffs-mcp-http",
    "uffs-watchdog",
];

/// Which slice of a root's binaries a [`binary_item`] call covers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StemSet {
    /// Plain tool binaries — deletable any time (group 1).
    Tools,
    /// [`RUNTIME_STEMS`] — deletable only after the shutdown group.
    Runtime,
}

/// Whether `stem` names a runtime binary (see [`RUNTIME_STEMS`]).
fn is_runtime_stem(stem: &str) -> bool {
    RUNTIME_STEMS
        .iter()
        .any(|runtime| runtime.eq_ignore_ascii_case(stem))
}

/// Build the per-root binary plan item for the requested stem set, or `None`
/// when the root has no matching binaries. A `WinGet` root delegates whole to
/// `winget uninstall` in the RUNTIME (post-shutdown) pass — winget cannot
/// delete the locked images of a daemon/broker still running from its package
/// dir — so its Tools pass is empty.
fn binary_item(root: &InstallRoot, set: StemSet) -> Option<PlanItem> {
    if root.binaries.is_empty() {
        return None;
    }
    let needs_elevation = binaries_need_escalation(root.scope, &root.dir);
    let item_scope = if needs_elevation {
        ItemScope::Machine
    } else {
        ItemScope::User
    };
    let target = match root.channel {
        Channel::WinGet => {
            // The delegation runs AFTER the shutdown group: a pure-winget
            // install has uffsd (and possibly the running uffs.exe) inside the
            // winget package dir, and `winget uninstall` cannot delete locked
            // images. Delegating before the daemon/broker stop made winget
            // fail on its own still-running files.
            if set == StemSet::Tools {
                return None;
            }
            PlanTarget::DelegateWinget {
                package_id: WINGET_PACKAGE_ID.to_owned(),
                scope: root.scope,
                dir: root.dir.clone(),
            }
        }
        Channel::Unmanaged | Channel::DevBuild | Channel::Unknown => {
            let stems: Vec<String> = root
                .binaries
                .iter()
                .filter(|bin| (set == StemSet::Runtime) == is_runtime_stem(&bin.name))
                .map(|bin| bin.name.clone())
                .collect();
            if stems.is_empty() {
                return None;
            }
            PlanTarget::DeleteBinaries {
                dir: root.dir.clone(),
                stems,
            }
        }
    };
    Some(PlanItem {
        target,
        needs_elevation,
        scope: item_scope,
        bytes: 0,
    })
}

/// Whether removing the UFFS binaries in `dir` (of install `scope`) needs
/// privilege escalation the current user may not have.
///
/// Windows: machine-scope roots (`%PROGRAMFILES%`) need Administrator; the
/// classified scope already captures this.
#[cfg(windows)]
const fn binaries_need_escalation(scope: Scope, _dir: &Path) -> bool {
    matches!(scope, Scope::Machine)
}

/// Unix variant (see the Windows declaration): probe `dir` with a POSIX
/// `access(W_OK)` check — a user-owned root (`~/bin`, `~/.cargo/bin`, a dev
/// build) is removable without `sudo`, while a root-owned one
/// (`/usr/local/bin`) is flagged before the executor tries.
#[cfg(unix)]
fn binaries_need_escalation(_scope: Scope, dir: &Path) -> bool {
    !uffs_mft::platform::dir_user_writable(dir)
}

/// Fallback for non-Windows, non-Unix targets: never require escalation.
#[cfg(not(any(windows, unix)))]
fn binaries_need_escalation(_scope: Scope, _dir: &Path) -> bool {
    false
}

/// Apply the `--scope` filter and append the group only if it has items left.
fn push_group(
    groups: &mut Vec<PlanGroup>,
    title: &'static str,
    items: Vec<PlanItem>,
    scope: UninstallScope,
) {
    let kept: Vec<PlanItem> = items
        .into_iter()
        .filter(|item| scope_admits(scope, item.scope))
        .collect();
    if !kept.is_empty() {
        groups.push(PlanGroup { title, items: kept });
    }
}

/// Whether a `--scope` request admits an item of the given scope.
const fn scope_admits(requested: UninstallScope, item: ItemScope) -> bool {
    match (requested, item) {
        (UninstallScope::All, _)
        | (_, ItemScope::Any)
        | (UninstallScope::User, ItemScope::User)
        | (UninstallScope::Machine, ItemScope::Machine) => true,
        (UninstallScope::User, ItemScope::Machine) | (UninstallScope::Machine, ItemScope::User) => {
            false
        }
    }
}

#[cfg(test)]
mod tests;
