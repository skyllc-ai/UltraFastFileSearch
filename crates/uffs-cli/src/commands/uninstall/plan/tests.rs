// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Unit tests for the removal-plan construction ([`super`]) — extracted into a
//! sibling module (the `compact_cache/tests.rs` / `backend_tests.rs` pattern)
//! so `plan.rs` stays within the file-size policy.

use std::path::PathBuf;

#[cfg(windows)]
use super::build_stray_plan;
use super::{PlanTarget, RemovalPlan, build_plan};
use crate::commands::elevation::ElevatablePlan as _;
use crate::commands::uninstall::args::{UninstallArgs, UninstallScope};
use crate::commands::uninstall::inventory::{
    ArtifactDir, ArtifactKind, BrokerServiceState, Inventory,
};
use crate::commands::update::model::{
    BinaryInfo, Channel, Component, DetectionReport, InstallRoot, RunningProcess, Scope,
};

fn root(channel: Channel, scope: Scope, dir: &str) -> InstallRoot {
    InstallRoot {
        dir: PathBuf::from(dir),
        channel,
        scope,
        anchored_by: Vec::new(),
        binaries: vec![BinaryInfo {
            name: "uffs".to_owned(),
            version: Some("0.6.16".to_owned()),
        }],
    }
}

fn inventory(broker: BrokerServiceState, config_size: u64) -> Inventory {
    Inventory {
        dirs: vec![
            ArtifactDir {
                kind: ArtifactKind::Cache,
                path: PathBuf::from("/x/cache"),
                exists: true,
                size_bytes: 2048,
            },
            ArtifactDir {
                kind: ArtifactKind::Config,
                path: PathBuf::from("/x/config"),
                exists: true,
                size_bytes: config_size,
            },
        ],
        broker_service: broker,
    }
}

fn has_target(plan: &RemovalPlan, predicate: impl Fn(&PlanTarget) -> bool) -> bool {
    plan.items().any(|item| predicate(&item.target))
}

/// Build a plan with no PATH entries (PATH has its own dedicated test).
fn built(report: &DetectionReport, inventory: &Inventory, args: &UninstallArgs) -> RemovalPlan {
    build_plan(report, inventory, args, &[])
}

#[test]
fn winget_delegation_runs_after_the_shutdown_group() {
    // A pure-winget install: uffsd runs FROM the winget package dir, and
    // winget cannot delete locked images — so the delegation must execute
    // after the daemon/broker shutdown, never before.
    let report = DetectionReport {
        roots: vec![root(Channel::WinGet, Scope::User, r"C:\winget\uffs")],
        running: vec![RunningProcess {
            component: Component::Daemon,
            pid: 4242,
            image_path: None,
            command_line: None,
            version: None,
        }],
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    let order: Vec<&'static str> = plan
        .items()
        .map(|item| item.target.action_label())
        .collect();
    let stop = order
        .iter()
        .position(|label| *label == "stop-process")
        .expect("daemon stop present");
    let winget = order
        .iter()
        .position(|label| *label == "delegate-winget")
        .expect("winget delegation present");
    assert!(
        stop < winget,
        "shutdown must precede the winget delegation: {order:?}"
    );
}

#[test]
fn winget_root_is_delegated_not_deleted() {
    let report = DetectionReport {
        roots: vec![root(Channel::WinGet, Scope::User, r"C:\winget\uffs")],
        running: Vec::new(),
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    assert!(has_target(&plan, |target| matches!(
        target,
        PlanTarget::DelegateWinget { .. }
    )));
    assert!(!has_target(&plan, |target| matches!(
        target,
        PlanTarget::DeleteBinaries { .. }
    )));
}

#[test]
fn machine_root_needs_elevation() {
    let report = DetectionReport {
        roots: vec![root(
            Channel::Unmanaged,
            Scope::Machine,
            r"C:\Program Files\uffs",
        )],
        running: Vec::new(),
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    assert!(plan.requires_elevation());
}

#[test]
fn service_present_requires_elevation_and_shuts_down_before_data() {
    let report = DetectionReport {
        roots: Vec::new(),
        running: Vec::new(),
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Installed, 1024),
        &UninstallArgs::default(),
    );
    assert!(plan.requires_elevation());
    assert!(has_target(&plan, |target| matches!(
        target,
        PlanTarget::RemoveService { .. }
    )));
    // Teardown-last ordering: the tools stay usable during the run, so the
    // shutdown group comes late — but still BEFORE the data dirs (a
    // running daemon holds open handles inside them).
    let titles: Vec<&str> = plan.groups.iter().map(|group| group.title).collect();
    let shutdown = titles
        .iter()
        .position(|title| *title == "Shutdown (stopped last)")
        .expect("a shutdown group");
    let data = titles
        .iter()
        .position(|title| *title == "Data / cache / config")
        .expect("a data group");
    assert!(shutdown < data, "shutdown must precede data: {titles:?}");
}

#[test]
fn runtime_binaries_split_into_the_post_shutdown_group() {
    // A root holding both tool and runtime binaries: the tools delete in
    // the first group; uffsd/uffs-broker (image locked while running) land
    // in "Runtime binaries (after shutdown)", after the shutdown group.
    let mut mixed = root(Channel::Unmanaged, Scope::User, "/opt/uffs");
    mixed.binaries = [
        "uffs",
        "uffs-analyze-diff",
        "uffsd",
        "uffs-broker",
        "uffs-watchdog",
    ]
    .into_iter()
    .map(|name| BinaryInfo {
        name: name.to_owned(),
        version: None,
    })
    .collect();
    let report = DetectionReport {
        roots: vec![mixed],
        running: Vec::new(),
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );

    let stems_of = |title: &str| -> Vec<String> {
        plan.groups
            .iter()
            .find(|group| group.title == title)
            .into_iter()
            .flat_map(|group| &group.items)
            .filter_map(|item| {
                if let PlanTarget::DeleteBinaries { stems, .. } = &item.target {
                    Some(stems.clone())
                } else {
                    None
                }
            })
            .flatten()
            .collect()
    };
    assert_eq!(stems_of("Binaries"), vec!["uffs", "uffs-analyze-diff"]);
    assert_eq!(stems_of("Runtime binaries (after shutdown)"), vec![
        "uffsd",
        "uffs-broker",
        "uffs-watchdog"
    ]);
    let titles: Vec<&str> = plan.groups.iter().map(|group| group.title).collect();
    let tools = titles
        .iter()
        .position(|title| *title == "Binaries")
        .expect("tools");
    let runtime = titles
        .iter()
        .position(|title| *title == "Runtime binaries (after shutdown)")
        .expect("runtime");
    assert!(tools < runtime, "runtime group must be last: {titles:?}");
}

#[test]
fn keep_config_drops_the_config_dir() {
    let report = DetectionReport {
        roots: Vec::new(),
        running: Vec::new(),
    };
    let inv = inventory(BrokerServiceState::Absent, 4096);
    let with_config = built(&report, &inv, &UninstallArgs::default());
    let keep = UninstallArgs {
        keep_config: true,
        ..UninstallArgs::default()
    };
    let without_config = built(&report, &inv, &keep);
    assert!(with_config.total_bytes() > without_config.total_bytes());
}

#[test]
fn scope_user_excludes_the_machine_service() {
    let report = DetectionReport {
        roots: Vec::new(),
        running: Vec::new(),
    };
    let user_only = UninstallArgs {
        scope: UninstallScope::User,
        ..UninstallArgs::default()
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Installed, 1024),
        &user_only,
    );
    assert!(!has_target(&plan, |target| matches!(
        target,
        PlanTarget::RemoveService { .. }
    )));
    assert!(!plan.requires_elevation());
}

#[test]
fn running_process_becomes_a_stop_item() {
    let report = DetectionReport {
        roots: Vec::new(),
        running: vec![RunningProcess {
            component: Component::Daemon,
            pid: 4242,
            image_path: None,
            command_line: None,
            version: None,
        }],
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    assert!(has_target(&plan, |target| matches!(
        target,
        PlanTarget::StopProcess { .. }
    )));
}

/// A running watchdog is stopped, and before the daemon it supervises (it
/// would restart a daemon stopped first).  Detection lists it first, and the
/// shutdown group keeps that order.
#[test]
fn watchdog_stops_before_the_daemon() {
    let process = |component: Component, pid: u32| RunningProcess {
        component,
        pid,
        image_path: None,
        command_line: None,
        version: None,
    };
    let report = DetectionReport {
        roots: Vec::new(),
        running: vec![
            process(Component::Watchdog, 7),
            process(Component::Daemon, 8),
        ],
    };
    let plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    let stops: Vec<&str> = plan
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .filter_map(|item| {
            if let PlanTarget::StopProcess { component, .. } = &item.target {
                Some(component.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(stops, ["watchdog", "daemon"]);
}

#[test]
fn size_binaries_fills_only_binary_delete_items_from_the_sizer() {
    // A plan with a deletable binaries root plus a data dir (already sized) and
    // a PATH item (never sized).
    let report = DetectionReport {
        roots: vec![root(Channel::Unmanaged, Scope::User, r"C:\Users\me\bin")],
        running: Vec::new(),
    };
    let mut plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    let dirs_before = plan
        .items()
        .filter(|item| matches!(item.target, PlanTarget::DeleteDir { .. }))
        .map(|item| item.bytes)
        .sum::<u64>();

    // Sizer reports 4096 bytes for any binary target.
    plan.size_binaries(|_dir, stems| 4096 * stems.len() as u64);

    let binary_bytes = plan
        .items()
        .filter(|item| matches!(item.target, PlanTarget::DeleteBinaries { .. }))
        .map(|item| item.bytes)
        .sum::<u64>();
    assert_eq!(binary_bytes, 4096, "the single `uffs` stem was sized");
    // The data-dir bytes are untouched by size_binaries.
    let dirs_after = plan
        .items()
        .filter(|item| matches!(item.target, PlanTarget::DeleteDir { .. }))
        .map(|item| item.bytes)
        .sum::<u64>();
    assert_eq!(
        dirs_after, dirs_before,
        "dir sizes are left as the inventory set them"
    );
}

#[cfg(windows)]
#[test]
fn ensure_daemon_shutdown_injects_a_stop_before_the_runtime_binaries() {
    // No daemon was running when the plan was built (the deep sweep starts one
    // afterwards), so the plan has no daemon stop — but the root holds a
    // RUNTIME binary (uffsd, the daemon image the sweep-started daemon would
    // lock), so a "Runtime binaries (after shutdown)" group forms and the
    // shutdown-before-runtime ordering is actually exercised. The shared
    // `root()` helper's default `uffs` is a *tool* stem that lands in the
    // "Binaries" group and forms no runtime group — the pre-existing bug this
    // `#[cfg(windows)]`, never-run-in-CI test hid until the workspace was first
    // tested natively on Windows.
    let mut install_root = root(Channel::Unmanaged, Scope::User, r"C:\Users\me\bin");
    install_root.binaries.push(BinaryInfo {
        name: "uffsd".to_owned(),
        version: Some("0.6.16".to_owned()),
    });
    let report = DetectionReport {
        roots: vec![install_root],
        running: Vec::new(),
    };
    let mut plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    assert!(
        !has_target(&plan, |target| matches!(
            target,
            PlanTarget::StopProcess { .. }
        )),
        "no daemon stop before the injection"
    );

    plan.ensure_daemon_shutdown(9191);

    let stop_group = plan
        .groups
        .iter()
        .position(|group| {
            group.items.iter().any(|item| {
                matches!(&item.target, PlanTarget::StopProcess { component, pid }
                    if component == "daemon" && *pid == 9191)
            })
        })
        .expect("the daemon stop was injected");
    let runtime_group = plan
        .groups
        .iter()
        .position(|group| group.title == "Runtime binaries (after shutdown)")
        .expect("runtime-binaries group present");
    assert!(
        stop_group < runtime_group,
        "the daemon stop must run before the runtime binaries are deleted"
    );
}

#[cfg(windows)]
#[test]
fn ensure_daemon_shutdown_is_a_noop_when_a_stop_already_exists() {
    let report = DetectionReport {
        roots: Vec::new(),
        running: vec![RunningProcess {
            component: Component::Daemon,
            pid: 4242,
            image_path: None,
            command_line: None,
            version: None,
        }],
    };
    let mut plan = built(
        &report,
        &inventory(BrokerServiceState::Absent, 1024),
        &UninstallArgs::default(),
    );
    let before = plan.item_count();
    plan.ensure_daemon_shutdown(9191);
    assert_eq!(
        plan.item_count(),
        before,
        "an existing daemon stop is not duplicated"
    );
    assert!(
        plan.items().any(|item| matches!(&item.target,
            PlanTarget::StopProcess { pid, .. } if *pid == 4242)),
        "the analyzed daemon stop is kept (not replaced)"
    );
}

#[test]
fn drop_elevation_required_removes_broker_keeps_the_rest() {
    let report = DetectionReport {
        roots: Vec::new(),
        running: vec![
            RunningProcess {
                component: Component::Broker,
                pid: 11,
                image_path: None,
                command_line: None,
                version: None,
            },
            RunningProcess {
                component: Component::Daemon,
                pid: 22,
                image_path: None,
                command_line: None,
                version: None,
            },
        ],
    };
    // Broker service installed -> an admin-only RemoveService item. The
    // broker *process* is filtered out (it's a service, stopped via sc, not
    // taskkill); only the user-owned daemon stop remains, needing no admin.
    let mut plan = built(
        &report,
        &inventory(BrokerServiceState::Installed, 1024),
        &UninstallArgs::default(),
    );
    assert!(
        plan.requires_elevation(),
        "broker service + process need admin"
    );

    let dropped = plan.drop_elevation_required();
    assert!(!plan.requires_elevation(), "admin-only items were dropped");
    assert!(
        !dropped.is_empty() && dropped.iter().all(|desc| !desc.is_empty()),
        "the dropped items are returned as human descriptions for the summary"
    );
    assert!(
        !has_target(&plan, |target| matches!(
            target,
            PlanTarget::RemoveService { .. }
        )),
        "the broker service item is gone"
    );
    let stop_pids: Vec<u32> = plan
        .items()
        .filter_map(|item| {
            if let PlanTarget::StopProcess { pid, .. } = &item.target {
                Some(*pid)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(stop_pids, vec![22], "only the daemon stop survives");
}

#[test]
#[cfg(windows)]
fn stray_plan_is_one_group_of_unprivileged_delete_file_items() {
    use crate::commands::uninstall::sweep::StrayHit;

    assert!(build_stray_plan(&[]).is_empty(), "no strays -> empty plan");
    let strays = vec![
        StrayHit {
            path: PathBuf::from("/home/me/Downloads/uffs"),
            version: Some("0.5.0".to_owned()),
        },
        StrayHit {
            path: PathBuf::from("/tmp/x_compact.uffs"),
            version: None,
        },
    ];
    let plan = build_stray_plan(&strays);
    assert_eq!(plan.item_count(), 2);
    assert!(
        plan.items()
            .all(|item| matches!(item.target, PlanTarget::DeleteFile { .. })),
        "every stray item is a DeleteFile"
    );
    assert!(
        !plan.requires_elevation(),
        "strays never require up-front elevation (best-effort on failure)"
    );
}

#[test]
fn path_entry_matching_a_removed_root_is_offered_and_respects_no_path() {
    let report = DetectionReport {
        roots: vec![root(Channel::Unmanaged, Scope::User, r"C:\Users\me\bin")],
        running: Vec::new(),
    };
    let inv = inventory(BrokerServiceState::Absent, 1024);
    // The 4th arg is the already-vetted removable-dir set; a case-insensitive
    // match to the removed root → offered. (Exclusivity vetting is tested in
    // analyze::removable_path_dirs; here we exercise build_plan's emission.)
    let on_path = [PathBuf::from(r"c:\users\me\bin")];
    let offered = build_plan(&report, &inv, &UninstallArgs::default(), &on_path);
    assert!(has_target(&offered, |target| matches!(
        target,
        PlanTarget::RemovePathEntry { .. }
    )));
    // --no-path suppresses the PATH group entirely.
    let no_path = UninstallArgs {
        no_path: true,
        ..UninstallArgs::default()
    };
    let suppressed = build_plan(&report, &inv, &no_path, &on_path);
    assert!(!has_target(&suppressed, |target| matches!(
        target,
        PlanTarget::RemovePathEntry { .. }
    )));
    // A PATH entry that does not match any root is never touched.
    let unrelated = [PathBuf::from(r"C:\unrelated")];
    let untouched = build_plan(&report, &inv, &UninstallArgs::default(), &unrelated);
    assert!(!has_target(&untouched, |target| matches!(
        target,
        PlanTarget::RemovePathEntry { .. }
    )));
}

#[cfg(unix)]
#[test]
fn unix_user_writable_root_skips_escalation_root_owned_flags_it() {
    use std::path::Path;

    use super::binaries_need_escalation;
    // The temp dir is user-writable → removable without sudo.
    assert!(!binaries_need_escalation(
        Scope::Unknown,
        &std::env::temp_dir()
    ));
    // A non-existent / unwritable path → flagged for escalation.
    assert!(binaries_need_escalation(
        Scope::Unknown,
        Path::new("/nonexistent/uffs-escalation-probe")
    ));
}

#[cfg(windows)]
#[test]
fn windows_escalation_follows_machine_scope() {
    use std::path::Path;

    use super::binaries_need_escalation;
    assert!(binaries_need_escalation(
        Scope::Machine,
        Path::new(r"C:\Program Files\uffs")
    ));
    assert!(!binaries_need_escalation(
        Scope::User,
        Path::new(r"C:\Users\me\bin")
    ));
}
