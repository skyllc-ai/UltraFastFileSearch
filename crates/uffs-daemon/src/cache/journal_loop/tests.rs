// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Mac-deterministic test fixture for the per-shard USN journal
//! loop (Phase 7 tasks 7.2 / 7.3 / 7.4 / 7.6 / 7.7).
//!
//! This module hosts the shared helpers (programmable
//! [`FakeJournalSource`], recording [`RecordingSink`], in-memory
//! [`FakeCursorStore`], `wait_for` deadline-driven convergence
//! helper) plus the submodule split for individual phase contracts.
//! Submodule layout keeps each file well under the workspace
//! 800-LOC file-size policy:
//!
//! * [`basics`] \u2014 Phase 7-B (loop lifecycle, cancellation, source error
//!   recovery, [`MacStubJournalSource`] always-empty contract).
//! * [`thresholds`] \u2014 Phase 7-C (events / age threshold-triggered
//!   `trigger_save` calls + counter-reset semantics).
//! * [`wrap_and_persistence`] \u2014 Phase 7-D (cursor seeding from
//!   [`CursorStore::load`], cursor-on-save persistence, journal-wrap detection
//!   via `journal_id` comparison).
//!
//! All tests use real wall-clock time because the loop's
//! `spawn_blocking` poll runs on a real OS thread and tokio's
//! `pause`-mode virtual time cannot drive it forward.  Convergence
//! is **always** deadline-driven via [`wait_for`] — never via a
//! standalone `tokio::time::sleep(N ms)` followed by an assertion,
//! because a fixed N-ms sleep races on slow CI runners (see
//! issue #208 history).  The pattern mirrors
//! `lifecycle_hooks.rs::CASCADE_DEADLINE` and is the only way to
//! get a reliable "give the runtime up to `CONVERGENCE_DEADLINE` to
//! reach state X" assertion against a `spawn_blocking`-driven
//! loop.
//!
//! [`MacStubJournalSource`]: super::MacStubJournalSource

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::time::Duration;
use std::sync::Mutex;

use uffs_mft::usn::FileChange;

use super::sources::NullCursorStore;
use super::{
    CursorStore, JournalLoopConfig, JournalPollResult, JournalSource, PatchSink, SaveReason,
};

mod apply_cadence;
mod backoff;
mod basics;
mod integration;
mod save_log_message;
mod thresholds;
mod wrap_and_persistence;

/// Acquire `mutex`, defusing any poison error by recovering the
/// inner guard.  Test code never deliberately poisons — this
/// helper just satisfies clippy's `expect_used` denial without
/// silencing it across the file.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Programmable journal source.
///
/// Holds a queue of poll results that the test pre-loads.  Each
/// call to `poll` pops the front of the queue (or returns
/// `JournalPollResult::default()` when empty so the loop has a
/// well-defined post-drain steady state).  Polls are also recorded
/// for assertions on cursor monotonicity.
struct FakeJournalSource {
    /// Queued results, popped in FIFO order on `poll()`.
    /// `Result<JournalPollResult, io::Error>` so error-recovery
    /// tests can interleave failures with successful polls.
    queue: Mutex<VecDeque<std::io::Result<JournalPollResult>>>,
    /// Cursor values that were passed into `poll()`, in call order.
    /// Lets tests assert the loop carried each `next_cursor`
    /// forward into the subsequent call.
    cursors_seen: Mutex<Vec<u64>>,
    /// Last `journal_id` of an enqueued result.  Used as the
    /// `journal_id` for post-drain empty polls so the loop's
    /// wrap-detection state machine doesn't false-fire when the
    /// queue empties — a real journal's id is stable until the
    /// journal is genuinely recreated, not per-poll.
    last_enqueued_journal_id: Mutex<u64>,
}

impl FakeJournalSource {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            cursors_seen: Mutex::new(Vec::new()),
            last_enqueued_journal_id: Mutex::new(1),
        }
    }

    /// Enqueue a successful poll result with the default test
    /// `journal_id` of 1.
    fn enqueue_changes(&self, changes: Vec<FileChange>, next_cursor: u64) {
        self.enqueue_changes_with_journal_id(changes, next_cursor, 1);
    }

    /// Enqueue a successful poll result with an explicit
    /// `journal_id`.  Phase 7-D wrap-detection tests use this to
    /// alternate between `journal_id` values across successive polls.
    fn enqueue_changes_with_journal_id(
        &self,
        changes: Vec<FileChange>,
        next_cursor: u64,
        journal_id: u64,
    ) {
        *lock_or_recover(&self.last_enqueued_journal_id) = journal_id;
        lock_or_recover(&self.queue).push_back(Ok(JournalPollResult {
            changes,
            next_cursor,
            journal_id,
        }));
    }

    /// Enqueue an `io::Error` to exercise the source-error retry path.
    ///
    /// Takes a fully-constructed `std::io::Error` (rather than an
    /// `ErrorKind` taxonomy) because `core::io::ErrorKind` is still
    /// nightly-gated (rust-lang/rust#154046) and the workspace lint
    /// `restriction::std_instead_of_core` flags any `std::io::ErrorKind`
    /// reference that happens to be available via `core` on a newer
    /// toolchain.  Passing the constructed error keeps the test
    /// surface stable across both nightly + stable.
    fn enqueue_error(&self, error: std::io::Error) {
        lock_or_recover(&self.queue).push_back(Err(error));
    }

    /// Snapshot of cursor values seen since construction.
    fn cursors_seen(&self) -> Vec<u64> {
        lock_or_recover(&self.cursors_seen).clone()
    }
}

impl JournalSource for FakeJournalSource {
    fn poll(&self, cursor: u64) -> std::io::Result<JournalPollResult> {
        lock_or_recover(&self.cursors_seen).push(cursor);
        let popped = lock_or_recover(&self.queue).pop_front();
        match popped {
            Some(Ok(res)) => Ok(res),
            Some(Err(err)) => Err(err),
            None => Ok(JournalPollResult {
                changes: Vec::new(),
                next_cursor: cursor,
                // Mirror the last-enqueued journal_id so post-drain
                // empty polls don't false-trip the loop's wrap
                // detection.  A real journal's id is stable until
                // the journal is genuinely recreated.
                journal_id: *lock_or_recover(&self.last_enqueued_journal_id),
            }),
        }
    }
}

/// Recording sink that captures every `accept()`, `trigger_save`,
/// and `journal_wrapped` invocation.
struct RecordingSink {
    /// One entry per `accept()` call: `(letter, change_count)`.
    /// Storing only the count (not the full `Vec<FileChange>`)
    /// keeps the Mutex contention minimal and the assertions
    /// focused on the loop semantics rather than payload shape.
    calls: Mutex<Vec<(uffs_mft::platform::DriveLetter, usize)>>,
    /// One entry per `trigger_save()` call: `(letter, reason)`.
    /// Phase 7-C surface — lets tests assert the threshold state
    /// machine fires the right reason at the right time.
    save_calls: Mutex<Vec<(uffs_mft::platform::DriveLetter, SaveReason)>>,
    /// One entry per `trigger_save()` call: the `cursor` argument.
    /// The loop hands its read position to the sink here; the sink
    /// (production `RegistryPatchSink`) persists it in lockstep with
    /// a successful body save.  Lets the loop-level tests assert the
    /// cursor reaches the sink without coupling to persistence.
    save_cursors: Mutex<Vec<u64>>,
    /// One entry per `trigger_apply()` call: `letter`.  The
    /// apply-cadence surface — lets tests assert the near-live body
    /// apply fires on the short interval (and that a save tick
    /// suppresses a redundant apply on the same buffer).
    apply_calls: Mutex<Vec<uffs_mft::platform::DriveLetter>>,
    /// One entry per `journal_wrapped()` call: `letter`.
    /// Phase 7-D surface — lets tests assert the wrap-detection
    /// state machine fires when `journal_id` changes between
    /// successive non-zero polls.
    wrap_calls: Mutex<Vec<uffs_mft::platform::DriveLetter>>,
    /// Boolean to return from `accept()`.  Tests flip this to
    /// `false` to exercise the registry-race / Parked-shard path
    /// (loop must continue cleanly when accept returns false).
    accept_outcome: Mutex<bool>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            save_calls: Mutex::new(Vec::new()),
            save_cursors: Mutex::new(Vec::new()),
            apply_calls: Mutex::new(Vec::new()),
            wrap_calls: Mutex::new(Vec::new()),
            accept_outcome: Mutex::new(true),
        }
    }

    fn calls(&self) -> Vec<(uffs_mft::platform::DriveLetter, usize)> {
        lock_or_recover(&self.calls).clone()
    }

    fn save_calls(&self) -> Vec<(uffs_mft::platform::DriveLetter, SaveReason)> {
        lock_or_recover(&self.save_calls).clone()
    }

    fn save_cursors(&self) -> Vec<u64> {
        lock_or_recover(&self.save_cursors).clone()
    }

    fn apply_calls(&self) -> Vec<uffs_mft::platform::DriveLetter> {
        lock_or_recover(&self.apply_calls).clone()
    }

    fn wrap_calls(&self) -> Vec<uffs_mft::platform::DriveLetter> {
        lock_or_recover(&self.wrap_calls).clone()
    }
}

impl PatchSink for RecordingSink {
    fn accept(&self, letter: uffs_mft::platform::DriveLetter, changes: &[FileChange]) -> bool {
        lock_or_recover(&self.calls).push((letter, changes.len()));
        *lock_or_recover(&self.accept_outcome)
    }

    fn trigger_save(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: SaveReason,
        cursor: u64,
    ) {
        lock_or_recover(&self.save_calls).push((letter, reason));
        lock_or_recover(&self.save_cursors).push(cursor);
    }

    fn trigger_apply(&self, letter: uffs_mft::platform::DriveLetter) {
        lock_or_recover(&self.apply_calls).push(letter);
    }

    fn journal_wrapped(&self, letter: uffs_mft::platform::DriveLetter) {
        lock_or_recover(&self.wrap_calls).push(letter);
    }
}

/// In-memory cursor store backed by a `HashMap<uffs_mft::platform::DriveLetter,
/// u64>`. Pre-loaded by tests via [`Self::set_cursor`] to seed the
/// loop's initial cursor; observed via [`Self::store_log`] to
/// assert the loop's persistence behaviour.
struct FakeCursorStore {
    cursors: Mutex<std::collections::HashMap<uffs_mft::platform::DriveLetter, u64>>,
    /// Append-only log of every `(letter, cursor)` passed to
    /// `store()`, in call order.  Lets tests assert which
    /// cursors were persisted at which points (not just the
    /// final state).
    store_log: Mutex<Vec<(uffs_mft::platform::DriveLetter, u64)>>,
}

impl FakeCursorStore {
    fn new() -> Self {
        Self {
            cursors: Mutex::new(std::collections::HashMap::new()),
            store_log: Mutex::new(Vec::new()),
        }
    }

    fn set_cursor(&self, letter: uffs_mft::platform::DriveLetter, cursor: u64) {
        lock_or_recover(&self.cursors).insert(letter, cursor);
    }

    fn store_log(&self) -> Vec<(uffs_mft::platform::DriveLetter, u64)> {
        lock_or_recover(&self.store_log).clone()
    }
}

impl CursorStore for FakeCursorStore {
    fn load(&self, letter: uffs_mft::platform::DriveLetter) -> u64 {
        lock_or_recover(&self.cursors)
            .get(&letter)
            .copied()
            .unwrap_or(0)
    }

    fn store(&self, letter: uffs_mft::platform::DriveLetter, cursor: u64) {
        lock_or_recover(&self.cursors).insert(letter, cursor);
        lock_or_recover(&self.store_log).push((letter, cursor));
    }
}

/// Default cursor store for the existing tick / cancel / cursor /
/// threshold tests that don't care about persistence — the
/// `NullCursorStore` returns 0 on `load` and is a no-op on `store`.
fn null_cursor_store() -> Arc<dyn CursorStore> {
    Arc::new(NullCursorStore)
}

/// Helper: small fake change so the queue is exercising a
/// non-trivial batch shape (not just `vec![]`).
///
/// Lifts the raw `u64` test literal into the typed [`uffs_mft::Frs`]
/// at this single construction boundary.
fn one_change(frs: u64) -> FileChange {
    FileChange {
        frs: frs.into(),
        deleted: true,
        ..FileChange::default()
    }
}

/// Helper: build a [`JournalLoopConfig`] with a fast-tick
/// `poll_interval` so the loop fires multiple ticks in a
/// hundred-millisecond test budget without flaking on slower
/// CI runners.  5 ms is well below any realistic CI scheduler
/// jitter while still being long enough that `spawn_blocking`
/// work can complete between ticks.  Save thresholds are set
/// generously so the tick / cancel / cursor tests don't
/// accidentally trigger saves; threshold tests override.
fn fast_config() -> JournalLoopConfig {
    JournalLoopConfig {
        poll_interval: Duration::from_millis(5),
        initial_cursor: 0,
        save_threshold_events: u64::MAX,
        save_threshold_age: Duration::from_hours(24),
        save_min_interval: Duration::ZERO,
        save_age_min_events: 0,
        save_age_ceiling: Duration::MAX,
        // Disabled by default so the generic tick / cancel / cursor
        // tests don't accidentally fire an apply; the apply-cadence
        // tests override this with a short interval.
        apply_interval: Duration::from_hours(24),
        apply_debounce: Duration::from_hours(24),
    }
}

/// Deadline for polling assertions — the maximum wall-clock time
/// the test will wait for the loop to converge on the expected
/// state.  At a 5 ms tick interval this gives the loop ~50
/// chances to fire, well above what every test below actually
/// needs.  Mirrors the `lifecycle_hooks.rs::CASCADE_DEADLINE`
/// idiom of "give the runtime a generous wall-clock budget
/// before declaring the convergence missed".
const CONVERGENCE_DEADLINE: Duration = Duration::from_millis(250);

/// Yield to the runtime + sleep a small amount so the journal
/// loop has a chance to fire one tick.  Used by [`wait_for`].
async fn yield_one_tick() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

/// Drive the runtime forward until `predicate()` returns `true`
/// or [`CONVERGENCE_DEADLINE`] elapses.  Returns `true` on
/// convergence, `false` on timeout — the caller asserts.
async fn wait_for<F: Fn() -> bool>(predicate: F) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < CONVERGENCE_DEADLINE {
        if predicate() {
            return true;
        }
        yield_one_tick().await;
    }
    predicate()
}
