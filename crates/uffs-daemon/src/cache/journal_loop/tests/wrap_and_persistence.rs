// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 7-D cursor-persistence + journal-wrap-detection tests.
//!
//! Pins the [`CursorStore`] + wrap-detection contracts:
//!
//! * **Cursor seeded at spawn** — `cursor_store.load(letter)` is the first
//!   poll's cursor argument when the store has a pre-loaded value.
//! * **Cursor handed to the sink on save** — the loop passes its read position
//!   into `PatchSink::trigger_save(letter, reason, cursor)`.  The loop itself
//!   no longer persists the cursor on save; the production sink
//!   (`RegistryPatchSink`) persists it **in lockstep with a successful
//!   compact-cache body save** (a parked shard's save is a no-op, so its cursor
//!   must not advance on disk).  The sink-side lockstep is pinned in
//!   `cache::journal_sink` tests.
//! * **Wrap detected via `journal_id` change** — a different `journal_id`
//!   between two successive non-zero polls fires `sink.journal_wrapped(letter)`
//!   and the wrap-tick's changes are NOT applied via `accept`.
//! * **Cursor resets after wrap** — the next poll after a wrap uses cursor=0
//!   (start of the new journal).
//!
//! [`CursorStore`]: super::super::CursorStore

use alloc::sync::Arc;
use core::time::Duration;

use super::super::{CursorStore, JournalLoopConfig, JournalSource, PatchSink, spawn_journal_loop};
use super::{
    CONVERGENCE_DEADLINE, FakeCursorStore, FakeJournalSource, RecordingSink, fast_config,
    one_change, wait_for,
};

#[tokio::test]
async fn cursor_loaded_from_store_at_spawn() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());
    let cursor_store = Arc::new(FakeCursorStore::new());
    cursor_store.set_cursor(uffs_mft::platform::DriveLetter::C, 42);

    // No batches enqueued; the loop will tick on empty results.
    // We're asserting the FIRST poll's cursor argument.
    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
        fast_config(),
    );

    // Wait until at least one poll has been issued.
    let source_for_pred = Arc::clone(&source);
    let converged = wait_for(move || !source_for_pred.cursors_seen().is_empty()).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(
        converged,
        "loop did not produce a poll within {CONVERGENCE_DEADLINE:?}"
    );
    let cursors = source.cursors_seen();
    assert_eq!(
        cursors.first().copied(),
        Some(42),
        "first poll must use the cursor loaded from the store, not 0"
    );
}

#[tokio::test]
async fn cursor_handed_to_sink_on_save_trigger() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());
    let cursor_store = Arc::new(FakeCursorStore::new());

    // 5 events with next_cursor=100 → events_threshold=5 fires save.
    source.enqueue_changes(
        vec![
            one_change(10),
            one_change(11),
            one_change(12),
            one_change(13),
            one_change(14),
        ],
        100,
    );

    let config = JournalLoopConfig {
        poll_interval: Duration::from_millis(5),
        initial_cursor: 0,
        save_threshold_events: 5,
        save_threshold_age: Duration::from_hours(24),
        save_min_interval: Duration::ZERO,
        apply_interval: Duration::from_hours(24),
        apply_debounce: Duration::from_hours(24),
    };
    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
        config,
    );

    let sink_for_pred = Arc::clone(&sink);
    let converged = wait_for(move || !sink_for_pred.save_calls().is_empty()).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(
        converged,
        "save did not fire within {CONVERGENCE_DEADLINE:?}"
    );
    // The loop hands its read position (100) to the sink's
    // `trigger_save` so the sink can persist it in lockstep with the
    // body save.
    let save_cursors = sink.save_cursors();
    assert!(
        save_cursors.contains(&100),
        "cursor 100 must be handed to the sink's trigger_save; got {save_cursors:?}"
    );
    // The loop itself must NOT persist the cursor on save — that
    // responsibility moved to the sink so a parked shard's no-op
    // save can't advance the on-disk cursor past the on-disk body.
    let log = cursor_store.store_log();
    assert!(
        !log.iter()
            .any(|&(letter, cursor)| letter == uffs_mft::platform::DriveLetter::C && cursor == 100),
        "loop must not persist the save cursor directly; log = {log:?}"
    );
}

#[tokio::test]
async fn journal_wrap_fires_journal_wrapped_and_skips_patch() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());
    let cursor_store = Arc::new(FakeCursorStore::new());

    // Tick 1: journal_id=1, one change.
    source.enqueue_changes_with_journal_id(vec![one_change(10)], 100, 1);
    // Tick 2: journal_id=2 (different) → wrap detected.
    // Even though changes are non-empty, accept must NOT be called.
    source.enqueue_changes_with_journal_id(vec![one_change(11)], 200, 2);

    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
        fast_config(),
    );

    let sink_for_pred = Arc::clone(&sink);
    let converged = wait_for(move || !sink_for_pred.wrap_calls().is_empty()).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(
        converged,
        "wrap detection did not fire within {CONVERGENCE_DEADLINE:?}"
    );
    let wraps = sink.wrap_calls();
    assert_eq!(
        wraps.len(),
        1,
        "exactly one journal_wrapped call expected; got {wraps:?}"
    );
    assert_eq!(
        wraps.first().copied(),
        Some(uffs_mft::platform::DriveLetter::C)
    );

    // The first batch (journal_id=1) MUST have been accepted; the
    // second (journal_id=2 wrap-tick) must NOT have produced an
    // accept call.
    let calls = sink.calls();
    assert_eq!(
        calls.len(),
        1,
        "only the pre-wrap batch must be accepted; got {calls:?}"
    );
    assert_eq!(
        calls.first().copied(),
        Some((uffs_mft::platform::DriveLetter::C, 1_usize))
    );
}

#[tokio::test]
async fn journal_wrap_resets_cursor_to_zero() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());
    let cursor_store = Arc::new(FakeCursorStore::new());

    // Tick 1: journal_id=1, advances cursor to 100.
    source.enqueue_changes_with_journal_id(vec![one_change(10)], 100, 1);
    // Tick 2: journal_id=2 → wrap detected, cursor reset to 0.
    source.enqueue_changes_with_journal_id(vec![one_change(11)], 200, 2);
    // Tick 3: journal_id=2 (same as tick 2), advances cursor.
    // The point of this batch is just to drive a third poll so the
    // assertion can observe the post-wrap cursor seed.
    source.enqueue_changes_with_journal_id(vec![one_change(12)], 300, 2);

    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
        fast_config(),
    );

    let source_for_pred = Arc::clone(&source);
    let converged = wait_for(move || source_for_pred.cursors_seen().len() >= 3).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(
        converged,
        "loop did not produce 3 polls within {CONVERGENCE_DEADLINE:?}"
    );
    let cursors = source.cursors_seen();
    // Tick 1 sees cursor=0 (initial), tick 2 sees cursor=100
    // (advanced from tick 1's next_cursor), tick 3 sees cursor=0
    // (reset by the wrap detected in tick 2).
    assert_eq!(
        cursors.first().copied(),
        Some(0),
        "first poll must use initial cursor 0"
    );
    assert_eq!(
        cursors.get(1).copied(),
        Some(100),
        "second poll must carry next_cursor=100 from tick 1"
    );
    assert_eq!(
        cursors.get(2).copied(),
        Some(0),
        "third poll must use cursor=0 (reset by the wrap on tick 2)"
    );
}
