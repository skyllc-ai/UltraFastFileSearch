// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Fast re-warm of a parked drive: the on-disk compact cache plus the
//! journal changes since it was written.
//!
//! The re-promote path used to rebuild from scratch every time
//! (`load_drive_with_usn_refresh`): load the drive's whole `MftIndex`,
//! apply the journal, build a new compact index while the `MftIndex` is
//! still alive, and start a cache save on top.  For the owner's 30.8 M-record
//! D: that took about three minutes and pushed the daemon to ~28 GB (field
//! 2026-10-14) for a 4.9 GB index.  The compact cache already is that index
//! as of its last save, so this path loads it and replays only the changes
//! since, synchronously, before the body is served.
//!
//! It applies only when the replay is known to be complete and exact; every
//! other case returns `None` and the caller falls back to the full rebuild:
//!
//! * the persisted cursor is outside the live journal's window, or the journal
//!   wrapped or was recreated during the replay ([`super::guarded_load`]'s
//!   `decide_strategy` rules);
//! * the compact cache is missing or unreadable;
//! * the replay holds more creates/renames than one targeted MFT read covers,
//!   so some files would land without size and timestamps.
//!
//! The persisted cursor never runs ahead of the on-disk body: the journal
//! loop stores it only after a save has written the file
//! (`crate::index::journal_save`), and a parked drive's save ticks write
//! nothing.  Replaying from it can therefore only overlap the body, never
//! leave a gap, and an overlap is harmless because applying a change is
//! idempotent.

/// Whether a replay of `changes` can be applied with full metadata: every
/// created or renamed file needs one targeted MFT read, and a single poll
/// reads at most `max_targeted_reads`.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used by the Windows re-warm path and its tests")
)]
pub(crate) fn replay_fits(
    changes: &[uffs_mft::usn::FileChange],
    max_targeted_reads: usize,
) -> bool {
    changes
        .iter()
        .filter(|change| change.created || change.renamed)
        .count()
        <= max_targeted_reads
}

/// Re-warm `letter` from its compact cache plus the journal since the
/// persisted cursor, or `None` when that cannot be done exactly (see the
/// module docs); the caller then rebuilds from scratch.
#[cfg(windows)]
pub(crate) fn load_parked_drive(
    letter: uffs_mft::platform::DriveLetter,
) -> Option<uffs_core::compact::DriveCompactIndex> {
    use std::time::Instant;

    use super::cursor_store::DiskCursorStore;
    use super::guarded_load::{WarmLoadStrategy, decide_strategy};
    use super::journal_loop::sources::WindowsJournalSource;
    use super::journal_loop::{CursorStore as _, JournalSource as _};

    let started = Instant::now();
    let info = uffs_mft::usn::query_usn_journal(letter)
        .map_err(|err| fallback(letter, "USN journal unavailable", &err))
        .ok()?;
    let cursor = DiskCursorStore::new(uffs_mft::cache::cache_dir()).load(letter);
    if decide_strategy(cursor, info.first_usn.raw(), info.next_usn.raw())
        != WarmLoadStrategy::FastFromCompactCache
    {
        fallback(letter, "persisted cursor outside the live journal", &cursor);
        return None;
    }

    let mut body = uffs_core::compact_cache::load_compact_cache(letter, u64::MAX, 0, true)
        .map_err(|err| fallback(letter, "compact cache unusable", &err))
        .ok()?;
    let cache_ms = started.elapsed().as_millis();

    let replay = WindowsJournalSource::new(letter)
        .poll(cursor)
        .map_err(|err| fallback(letter, "journal read failed", &err))
        .ok()?;
    if replay.next_cursor == 0 || replay.journal_id != info.journal_id {
        fallback(
            letter,
            "journal wrapped or was recreated during the re-warm",
            &cursor,
        );
        return None;
    }
    if !replay_fits(
        &replay.changes,
        WindowsJournalSource::MAX_TARGETED_READS_PER_POLL,
    ) {
        fallback(
            letter,
            "too many created/renamed files since the last save to read their metadata",
            &replay.changes.len(),
        );
        return None;
    }

    let stats = uffs_core::compact_loader::apply_usn_patch(&mut body, &replay.changes);
    tracing::info!(
        target: "shard.transition",
        drive = %letter,
        cursor,
        replayed = replay.changes.len(),
        created = stats.created,
        deleted = stats.deleted,
        renamed = stats.renamed,
        cache_ms,
        total_ms = started.elapsed().as_millis(),
        "parked drive re-warmed from its compact cache plus the journal since the last save",
    );
    Some(body)
}

/// Log why the fast re-warm is not used for `letter`.
#[cfg(windows)]
fn fallback(letter: uffs_mft::platform::DriveLetter, why: &str, detail: &dyn core::fmt::Display) {
    tracing::info!(
        target: "shard.transition",
        drive = %letter,
        detail = %detail,
        "re-warm falls back to a full rebuild: {why}",
    );
}

#[cfg(test)]
mod tests {
    use uffs_mft::usn::FileChange;

    use super::replay_fits;

    fn change(created: bool, renamed: bool, deleted: bool) -> FileChange {
        FileChange {
            created,
            renamed,
            deleted,
            ..FileChange::default()
        }
    }

    #[test]
    fn only_creates_and_renames_count_against_the_read_budget() {
        let changes = [
            change(true, false, false),
            change(false, true, false),
            change(false, false, true),
            change(false, false, true),
        ];
        assert!(replay_fits(&changes, 2), "deletes need no metadata read");
        assert!(
            !replay_fits(&changes, 1),
            "two creates/renames exceed a budget of one"
        );
    }

    #[test]
    fn empty_replay_always_fits() {
        assert!(replay_fits(&[], 0));
    }
}
