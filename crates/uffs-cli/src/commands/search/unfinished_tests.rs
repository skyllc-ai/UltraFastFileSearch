// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for the client-timeout message: each daemon state the probe can
//! report must be named in the error, and an unanswered probe must say the
//! daemon looks stuck.

use core::time::Duration;

use uffs_client::protocol::response::{DaemonStatus, DriveInfo, ShardTier};
use uffs_mft::platform::DriveLetter;

use super::{Probe, Stage, describe_limit, letters, timeout_message};

fn drive(letter: char, tier: ShardTier, loading: bool) -> DriveInfo {
    DriveInfo {
        letter: DriveLetter::parse(letter).unwrap(),
        records: 0,
        source: "live".to_owned(),
        tier: Some(tier),
        records_when_warm: None,
        loading: Some(loading),
    }
}

const TEN_MIN: Option<Duration> = Some(Duration::from_secs(600));

#[test]
fn parked_drive_warming_is_named() {
    let probe = Probe {
        status: Some(DaemonStatus::Ready),
        drives: Some(vec![
            drive('C', ShardTier::Warm, false),
            drive('D', ShardTier::Parked, true),
        ]),
    };
    let msg = timeout_message(Stage::Search, TEN_MIN, &probe);
    assert!(
        msg.starts_with("the search did not finish within 10 minutes"),
        "{msg}"
    );
    assert!(
        msg.contains("D: was still being brought back from parked"),
        "{msg}"
    );
    assert!(!msg.contains("C:"), "{msg}");
    assert!(msg.contains("UFFS_CLIENT_TIMEOUT_SECS"), "{msg}");
}

#[test]
fn several_warming_drives_are_listed() {
    let probe = Probe {
        status: Some(DaemonStatus::Ready),
        drives: Some(vec![
            drive('D', ShardTier::Parked, true),
            drive('M', ShardTier::Parked, true),
            drive('S', ShardTier::Cold, true),
        ]),
    };
    let msg = timeout_message(Stage::Search, TEN_MIN, &probe);
    assert!(
        msg.contains("D:, M: and S: were still being brought back"),
        "{msg}"
    );
}

#[test]
fn daemon_index_load_is_named() {
    let probe = Probe {
        status: Some(DaemonStatus::Loading {
            drives_loaded: 4,
            drives_total: 6,
        }),
        drives: Some(Vec::new()),
    };
    let msg = timeout_message(Stage::Ready, TEN_MIN, &probe);
    assert!(
        msg.starts_with("the UFFS daemon did not become ready within 10 minutes"),
        "{msg}"
    );
    assert!(
        msg.contains("still loading its indexes (4 of 6 drives loaded)"),
        "{msg}"
    );
}

#[test]
fn unanswered_probe_says_stuck() {
    let probe = Probe {
        status: None,
        drives: None,
    };
    let msg = timeout_message(Stage::Search, TEN_MIN, &probe);
    assert!(msg.contains("did not answer a status check"), "{msg}");
    assert!(msg.contains("stuck"), "{msg}");
}

#[test]
fn idle_daemon_with_parked_drive_says_warm_up_not_started() {
    let probe = Probe {
        status: Some(DaemonStatus::Ready),
        drives: Some(vec![drive('D', ShardTier::Parked, false)]),
    };
    let msg = timeout_message(Stage::Search, TEN_MIN, &probe);
    assert!(msg.contains("D: is still parked"), "{msg}");
}

#[test]
fn idle_daemon_with_nothing_in_progress_says_search_itself() {
    let probe = Probe {
        status: Some(DaemonStatus::Ready),
        drives: Some(vec![drive('C', ShardTier::Warm, false)]),
    };
    let msg = timeout_message(Stage::Search, TEN_MIN, &probe);
    assert!(
        msg.contains("the search itself was still running or stuck"),
        "{msg}"
    );
}

#[test]
fn limit_is_said_in_minutes_or_seconds() {
    assert_eq!(describe_limit(Duration::from_secs(600)), "10 minutes");
    assert_eq!(describe_limit(Duration::from_secs(60)), "1 minute");
    assert_eq!(describe_limit(Duration::from_secs(120)), "2 minutes");
    assert_eq!(describe_limit(Duration::from_secs(90)), "90 s");
    assert_eq!(describe_limit(Duration::from_secs(30)), "30 s");
}

#[test]
fn letters_join_naturally() {
    let parse = |ch| DriveLetter::parse(ch).unwrap();
    assert_eq!(letters([].iter()), "");
    assert_eq!(letters([parse('D')].iter()), "D:");
    assert_eq!(letters([parse('D'), parse('E')].iter()), "D: and E:");
}
