// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The error for a search the client stopped waiting for.
//!
//! A plain search waits silently, for as long as the client limit allows
//! (`UFFS_CLIENT_TIMEOUT_SECS`, 10 minutes by default): a parked drive being
//! paged back in is a wait, not an error, and a script that just wants the
//! result must get it with nothing else on stdout or stderr.  Only when the
//! limit runs out does the command fail, and then the error names what had
//! not finished — the daemon's own index load, a drive still being brought
//! back from parked, or nothing the daemon could report at all — by asking
//! the daemon over a fresh, short-deadline connection.

use core::time::Duration;

use uffs_client::connect_sync::UffsClientSync;
use uffs_client::protocol::response::{DaemonStatus, DriveInfo, ShardTier};

/// Deadline for the diagnostic status probe.  Short on purpose: it must not
/// inherit the long search limit, or a stuck daemon would hang the error too.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Which wait ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    /// Waiting for the daemon to finish loading its indexes.
    Ready,
    /// Waiting for the search response.
    Search,
}

/// What the daemon reported when asked after the limit ran out.
#[derive(Debug)]
pub(super) struct Probe {
    /// The daemon's lifecycle status, or `None` if it did not answer.
    status: Option<DaemonStatus>,
    /// Per-drive tier and load state, or `None` if it did not answer.
    drives: Option<Vec<DriveInfo>>,
}

/// Ask the daemon what it is still busy with.  Never fails: an unanswered
/// probe is itself the finding.
pub(super) fn probe_daemon() -> Probe {
    let Ok(mut client) = UffsClientSync::connect_probe(PROBE_DEADLINE) else {
        return Probe {
            status: None,
            drives: None,
        };
    };
    let status = client.status().ok().map(|resp| resp.status);
    let drives = client.drives().ok().map(|resp| resp.drives);
    Probe { status, drives }
}

/// The full error text for a client timeout at `stage`.
pub(super) fn timeout_message(stage: Stage, limit: Option<Duration>, probe: &Probe) -> String {
    let limit_text = limit.map_or_else(|| "the client limit".to_owned(), describe_limit);
    let head = match stage {
        Stage::Ready => format!("the UFFS daemon did not become ready within {limit_text}"),
        Stage::Search => format!("the search did not finish within {limit_text}"),
    };
    let unfinished = describe_unfinished(probe);
    format!(
        "{head} (UFFS_CLIENT_TIMEOUT_SECS).\n\
         Not finished: {unfinished}.\n\
         Check `uffs --daemon status`, re-run the search once the daemon is \
         idle, or set UFFS_CLIENT_TIMEOUT_SECS to wait longer (0 = no limit)."
    )
}

/// `limit` as the operator would say it: whole minutes when it is a whole
/// number of minutes, seconds otherwise.
fn describe_limit(limit: Duration) -> String {
    let secs = limit.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) {
        let mins = secs / 60;
        let unit = if mins == 1 { "minute" } else { "minutes" };
        format!("{mins} {unit}")
    } else {
        format!("{secs} s")
    }
}

/// Everything the daemon reported as still in progress, joined into one
/// clause.
fn describe_unfinished(probe: &Probe) -> String {
    let Some(status) = &probe.status else {
        return "the daemon did not answer a status check within 5 s either, \
                so it looks stuck rather than busy; its log is the next place \
                to look"
            .to_owned();
    };

    let mut parts: Vec<String> = Vec::new();
    match status {
        DaemonStatus::Loading {
            drives_loaded,
            drives_total,
        } => parts.push(format!(
            "the daemon was still loading its indexes ({drives_loaded} of {drives_total} drives \
             loaded)"
        )),
        DaemonStatus::Refreshing { drives } => {
            parts.push(format!(
                "the daemon was refreshing {}",
                letters(drives.iter())
            ));
        }
        DaemonStatus::Ready => {}
    }

    if let Some(drives) = &probe.drives {
        let warming: Vec<&DriveInfo> = drives
            .iter()
            .filter(|drive| drive.loading == Some(true))
            .collect();
        if !warming.is_empty() {
            let verb = if warming.len() == 1 { "was" } else { "were" };
            parts.push(format!(
                "{} {verb} still being brought back from parked (the daemon re-reads \
                 the index from disk, which takes minutes on a large HDD volume)",
                letters(warming.iter().map(|drive| &drive.letter)),
            ));
        }
    }

    if parts.is_empty() {
        let parked = probe.drives.as_ref().map_or_else(Vec::new, |drives| {
            drives
                .iter()
                .filter(|drive| matches!(drive.tier, Some(ShardTier::Parked | ShardTier::Cold)))
                .map(|drive| drive.letter)
                .collect::<Vec<_>>()
        });
        match parked.len() {
            0 => "the daemon answered but reported no index load or drive warm-up \
                  in progress, so the search itself was still running or stuck"
                .to_owned(),
            count => format!(
                "the daemon reported no warm-up in progress, but {} {} still parked, \
                 so bringing {} back may not have started",
                letters(parked.iter()),
                if count == 1 { "is" } else { "are" },
                if count == 1 { "it" } else { "them" },
            ),
        }
    } else {
        parts.join("; ")
    }
}

/// `D:` / `D: and E:` / `C:, D: and E:`.
fn letters<'a>(drives: impl Iterator<Item = &'a uffs_mft::platform::DriveLetter>) -> String {
    let names: Vec<String> = drives.map(|letter| format!("{letter}:")).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
#[path = "unfinished_tests.rs"]
mod tests;
