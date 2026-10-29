// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Concrete [`JournalSource`] + [`CursorStore`] implementations split
//! out from the parent `journal_loop` module so the parent stays
//! focused on the trait definitions, the [`super::JournalLoop`]
//! state machine, and the spawn helpers.
//!
//! The three types here are:
//!
//! * [`MacStubJournalSource`] — production source on macOS / Linux (NTFS USN
//!   journals don't exist there).  Always-empty polls keep the loop's state
//!   machine ticking — cursor advances, no patches, no save triggers — so
//!   cross-platform tests can exercise the full loop flow without driving real
//!   journal data.
//!
//! * `WindowsJournalSource` — production source on Windows, wrapping
//!   `FSCTL_QUERY_USN_JOURNAL` + `FSCTL_READ_USN_JOURNAL` via
//!   [`uffs_mft::usn`].  Compile-gated to `cfg(windows)` so a misconfigured Mac
//!   wiring is rejected at compile time rather than silently degrading to an
//!   `ErrorKind::Unsupported` retry loop against the underlying Mac stub
//!   helpers.
//!
//! * [`NullCursorStore`] — production cursor store on macOS / Linux + the
//!   test-suite default for tests that don't care about persistence.  `load`
//!   returns 0 (the "start from journal head" sentinel), `store` is a no-op.
//!
//! [`JournalSource`]: super::JournalSource
//! [`CursorStore`]: super::CursorStore

use super::{CursorStore, JournalPollResult, JournalSource};

// ─── Cross-platform always-empty journal source ─────────────────────────────

/// Cross-platform always-empty journal source.
///
/// Used as the production journal source on macOS / Linux where
/// USN journals don't exist, and as a default for tests that don't
/// need to drive change events.  Every poll returns
/// `JournalPollResult::default()` (no changes, cursor unchanged,
/// `journal_id == 0`) without any I/O.
#[cfg_attr(
    all(windows, not(test)),
    expect(
        dead_code,
        reason = "Production on Windows uses `WindowsJournalSource` \
                  (real FSCTL-backed) instead of this stub; \
                  `MacStubJournalSource` IS constructed on every \
                  platform under `cfg(test)` (the journal_loop test \
                  suite uses it as the default empty source) and \
                  on Mac/Linux production via the matching \
                  `cfg(not(windows))` arm of `lib.rs::\
                  make_journal_source`.  This narrow `expect` \
                  silences the Windows-prod-only dead-code warning \
                  without disabling cross-platform reachability."
    )
)]
#[derive(Debug, Default)]
pub(crate) struct MacStubJournalSource;

impl JournalSource for MacStubJournalSource {
    fn poll(&self, cursor: u64) -> std::io::Result<JournalPollResult> {
        Ok(JournalPollResult {
            changes: Vec::new(),
            next_cursor: cursor,
            journal_id: 0,
        })
    }
}

// ─── Windows production journal source ───────────────────────────────────────

/// Windows production journal source.
///
/// Wraps the real `FSCTL_QUERY_USN_JOURNAL` + `FSCTL_READ_USN_JOURNAL`
/// path via [`uffs_mft::usn::query_usn_journal`] +
/// [`uffs_mft::usn::read_usn_journal`] + [`uffs_mft::usn::aggregate_changes`].
/// Carries the drive letter so the broker's volume-handle pool
/// can resolve to the right NTFS volume.
///
/// Mac/Linux production wires [`MacStubJournalSource`] instead —
/// the underlying FSCTL helpers exist as Mac stubs that return
/// `ErrorKind::Unsupported`, but constructing `WindowsJournalSource`
/// on those platforms is rejected at compile time by the
/// `#[cfg(windows)]` gate so a misconfigured wiring can't reach
/// the no-op error path silently.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct WindowsJournalSource {
    /// Drive letter for which this source reads the USN journal.
    drive: uffs_mft::platform::DriveLetter,
}

#[cfg(windows)]
impl WindowsJournalSource {
    /// Create a source bound to `drive`.
    #[must_use]
    pub(crate) const fn new(drive: uffs_mft::platform::DriveLetter) -> Self {
        Self { drive }
    }
}

#[cfg(windows)]
impl JournalSource for WindowsJournalSource {
    /// Poll the live NTFS USN journal for changes since `cursor`.
    ///
    /// Three-step flow:
    ///
    /// 1. **Query** — `FSCTL_QUERY_USN_JOURNAL` returns the volume's current
    ///    `journal_id` + `first_usn`.  The `journal_id` is forwarded to the
    ///    loop's wrap-detection state machine; a change between two successive
    ///    non-zero ids signals the journal was recreated and the loop fires
    ///    [`super::PatchSink::journal_wrapped`] + resets the cursor.
    ///
    /// 2. **Wrap short-circuit** — when the caller's `cursor` predates
    ///    `first_usn` (operator-recreated journal where the id stayed stable
    ///    but the journal head moved past the persisted cursor), return empty
    ///    changes with `next_cursor=0` so the loop seeds from the new head.
    ///    Belt-and-braces with the loop's id-comparison wrap detection.
    ///
    /// 3. **Read + aggregate** — `FSCTL_READ_USN_JOURNAL` walks the journal
    ///    until exhausted (the underlying helper handles 64 KiB chunking
    ///    internally), then [`uffs_mft::usn::aggregate_changes`] folds the raw
    ///    `UsnRecord` stream into per-FRS [`uffs_mft::usn::FileChange`] entries
    ///    so the registry-side patch path applies one update per file rather
    ///    than one per record.
    fn poll(&self, cursor: u64) -> std::io::Result<JournalPollResult> {
        let info = uffs_mft::usn::query_usn_journal(self.drive)?;

        // Wrap short-circuit: if the persisted cursor predates the
        // current journal's first valid USN, force the loop to
        // reseed from journal head.  `journal_id` is preserved so
        // the loop's id-comparison wrap detection can still fire
        // on a subsequent recreation.
        // Persistence wire-format is `u64` (the cursor store stores
        // unsigned cursors); the kernel-facing API is `Usn` (signed
        // 64-bit `LONGLONG`).  Narrow at this single boundary.
        let start_usn = uffs_mft::usn::Usn::new(i64::try_from(cursor).unwrap_or(i64::MAX));
        if start_usn < info.first_usn {
            return Ok(JournalPollResult {
                changes: Vec::new(),
                next_cursor: 0,
                journal_id: info.journal_id,
            });
        }

        let (records, next_usn) =
            uffs_mft::usn::read_usn_journal(self.drive, info.journal_id, start_usn)?;
        let aggregated = uffs_mft::usn::aggregate_changes(&records);
        let mut changes: Vec<uffs_mft::usn::FileChange> = aggregated.into_values().collect();
        let next_cursor = u64::try_from(next_usn.raw()).unwrap_or(u64::MAX);

        // Backfill real size/timestamps/flags via a targeted MFT read. USN
        // records carry only name+parent, so a create/rename would otherwise
        // land with size 0 and zero timestamps. This runs HERE (in `poll`,
        // on the spawn_blocking thread) — before the registry write-lock is
        // taken in `accept` — so it never lengthens the lock hold or touches
        // the query path. Best-effort: any failure leaves `meta = None` and
        // the records keep their (current) zeroed metrics.
        Self::backfill_metadata(self.drive, &mut changes);

        Ok(JournalPollResult {
            changes,
            next_cursor,
            journal_id: info.journal_id,
        })
    }
}

#[cfg(windows)]
impl WindowsJournalSource {
    /// Upper bound on targeted MFT reads per poll. A bulk operation (e.g.
    /// unzipping thousands of files) can produce a large change set in one
    /// 500 ms window; cap the read so a single poll can't stall the loop.
    /// Records past the cap keep `meta = None` for this poll and are
    /// backfilled on a subsequent one (or by the next full re-warm).
    pub(crate) const MAX_TARGETED_READS_PER_POLL: usize = 4096;

    /// Issue one batched targeted MFT read for the created/renamed FRSes in
    /// `changes` and attach the recovered [`uffs_mft::usn::RecordMeta`] to
    /// each. Deletes need no metadata and are skipped.
    fn backfill_metadata(
        drive: uffs_mft::platform::DriveLetter,
        changes: &mut [uffs_mft::usn::FileChange],
    ) {
        // Collect the FRSes that need real metadata (creates + renames).
        let frs_list: Vec<u64> = changes
            .iter()
            .filter(|change| change.created || change.renamed)
            .take(Self::MAX_TARGETED_READS_PER_POLL)
            .map(|change| change.frs.raw())
            .collect();
        if frs_list.is_empty() {
            return;
        }
        let Some(scratch) = Self::read_targeted_records(drive, &frs_list) else {
            return;
        };

        // Attach the recovered metadata. Representation matches CompactRecord
        // exactly (i64 µs timestamps, raw NTFS flags), so it copies straight.
        for change in changes.iter_mut() {
            if !(change.created || change.renamed) {
                continue;
            }
            if let Some(record) = scratch.find(change.frs) {
                change.meta = Some(uffs_mft::usn::RecordMeta {
                    size: record.first_stream.size.length,
                    allocated: record.first_stream.size.allocated,
                    created: record.stdinfo.created,
                    modified: record.stdinfo.modified,
                    accessed: record.stdinfo.accessed,
                    flags: record.stdinfo.flags,
                });
            }
        }
    }

    /// Open the volume (auto-adopting the broker handle when non-elevated —
    /// the same path the USN read already uses) and read `frs_list` into a
    /// scratch [`MftIndex`](uffs_mft::index::MftIndex). Best-effort: any
    /// failure returns `None` (debug-logged), leaving callers' `meta` empty.
    fn read_targeted_records(
        drive: uffs_mft::platform::DriveLetter,
        frs_list: &[u64],
    ) -> Option<uffs_mft::index::MftIndex> {
        let handle = match uffs_mft::platform::VolumeHandle::open(drive) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::debug!(drive = %drive, error = %err, "usn backfill: volume open failed");
                return None;
            }
        };
        let mut scratch = uffs_mft::index::MftIndex::new(drive);
        match uffs_mft::usn::read_targeted_frs_records(&handle, &mut scratch, frs_list) {
            Ok(read) => {
                tracing::debug!(drive = %drive, requested = frs_list.len(), read, "usn backfill: targeted MFT reads complete");
                Some(scratch)
            }
            Err(err) => {
                tracing::debug!(drive = %drive, error = %err, "usn backfill: targeted reads failed");
                None
            }
        }
    }
}

// ─── No-op cursor store ─────────────────────────────────────────────────────

/// Always-empty cursor store: `load` returns 0, `store` is a
/// no-op.  Used as the production fallback on macOS / Linux
/// where there is no live journal to persist a cursor for, and
/// as a default for tests that don't care about the persistence
/// path.
#[derive(Debug, Default)]
pub(crate) struct NullCursorStore;

impl CursorStore for NullCursorStore {
    fn load(&self, _letter: uffs_mft::platform::DriveLetter) -> u64 {
        0
    }
    fn store(&self, _letter: uffs_mft::platform::DriveLetter, _cursor: u64) {}
}
