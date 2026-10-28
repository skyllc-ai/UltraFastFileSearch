// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 7-C threshold-triggered save tests.
//!
//! Pins the [`SaveTrigger`] state-machine contract: events- and
//! age-based thresholds fire `trigger_save` at the right time
//! with the right [`SaveReason`]; zero-event drives produce no
//! wasteful saves; counters reset after firing so a single
//! threshold crossing produces exactly one save.
//!
//! [`SaveTrigger`]: super::super::SaveTrigger

use alloc::sync::Arc;
use core::time::Duration;

use super::super::{
    JournalLoopConfig, JournalSource, PatchSink, SaveReason, SaveTrigger, spawn_journal_loop,
};
use super::{
    CONVERGENCE_DEADLINE, FakeJournalSource, RecordingSink, null_cursor_store, one_change, wait_for,
};

#[tokio::test]
async fn events_threshold_triggers_save() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());

    // Two batches of 3 + 4 = 7 events; threshold of 5 means the
    // second batch must trigger an EventsExceeded save.
    source.enqueue_changes(vec![one_change(10), one_change(11), one_change(12)], 100);
    source.enqueue_changes(
        vec![
            one_change(13),
            one_change(14),
            one_change(15),
            one_change(16),
        ],
        200,
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
        null_cursor_store(),
        config,
    );

    let sink_for_pred = Arc::clone(&sink);
    let converged = wait_for(move || !sink_for_pred.save_calls().is_empty()).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(
        converged,
        "events threshold did not fire trigger_save within {CONVERGENCE_DEADLINE:?}"
    );
    let saves = sink.save_calls();
    assert_eq!(
        saves.len(),
        1,
        "exactly one EventsExceeded save expected; got {saves:?}"
    );
    assert_eq!(
        saves.first().copied(),
        Some((
            uffs_mft::platform::DriveLetter::C,
            SaveReason::EventsExceeded
        )),
        "save reason must be EventsExceeded"
    );
}

#[tokio::test]
async fn age_threshold_triggers_save_with_pending_events() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());

    // First tick lands one event (under the events-threshold,
    // and well within the age-threshold from spawn time).
    source.enqueue_changes(vec![one_change(10)], 100);

    let config = JournalLoopConfig {
        poll_interval: Duration::from_millis(5),
        initial_cursor: 0,
        save_threshold_events: u64::MAX,
        save_threshold_age: Duration::from_millis(30),
        save_min_interval: Duration::ZERO,
        apply_interval: Duration::from_hours(24),
        apply_debounce: Duration::from_hours(24),
    };
    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        null_cursor_store(),
        config,
    );

    // Wait for the first batch to land via accept() — proves
    // events_since_save is now > 0.
    let sink_for_accept_pred = Arc::clone(&sink);
    let first_accept_landed = wait_for(move || !sink_for_accept_pred.calls().is_empty()).await;
    assert!(
        first_accept_landed,
        "first batch must land before age can be tested"
    );

    // Sleep past the 30 ms age threshold while events stay pending
    // (no save fires on empty ticks — those skip the threshold
    // evaluation per Phase 7 task 7.4 design).
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Now enqueue the second batch.  Its tick will observe
    // elapsed >= 30 ms with events_since_save > 0, firing AgeElapsed.
    source.enqueue_changes(vec![one_change(11)], 200);

    let sink_for_save_pred = Arc::clone(&sink);
    let converged = wait_for(move || !sink_for_save_pred.save_calls().is_empty()).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(
        converged,
        "age threshold did not fire trigger_save within {CONVERGENCE_DEADLINE:?}"
    );
    let saves = sink.save_calls();
    assert_eq!(
        saves.first().copied(),
        Some((uffs_mft::platform::DriveLetter::C, SaveReason::AgeElapsed)),
        "save reason must be AgeElapsed; got {saves:?}"
    );
}

#[tokio::test]
async fn zero_events_drive_does_not_trigger_save() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());

    // Empty queue + tight age threshold.  The age threshold
    // SHOULD NOT fire because no events have ever been recorded
    // — Phase 7 task 7.4 contract: zero-churn drives produce
    // no wasteful saves.
    let config = JournalLoopConfig {
        poll_interval: Duration::from_millis(5),
        initial_cursor: 0,
        save_threshold_events: 1,
        save_threshold_age: Duration::from_millis(20),
        save_min_interval: Duration::ZERO,
        apply_interval: Duration::from_hours(24),
        apply_debounce: Duration::from_hours(24),
    };
    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        null_cursor_store(),
        config,
    );

    // Let several ticks fire well past the age threshold.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    let saves = sink.save_calls();
    assert!(
        saves.is_empty(),
        "zero-event drive must not produce any save triggers; got {saves:?}"
    );
    // Sanity: the loop did poll the source.
    assert!(
        !source.cursors_seen().is_empty(),
        "loop must have polled the source"
    );
}

#[tokio::test]
async fn counter_resets_after_save() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());

    // First batch of 5 events crosses the events_threshold = 5
    // → first save fires.  Second batch of 1 event must NOT
    // trigger another save (counter reset; 1 < 5).
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
    source.enqueue_changes(vec![one_change(15)], 200);

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
        null_cursor_store(),
        config,
    );

    // Wait until both accept() calls land + first save fires.
    let sink_for_pred = Arc::clone(&sink);
    let converged = wait_for(move || {
        sink_for_pred.calls().len() >= 2 && !sink_for_pred.save_calls().is_empty()
    })
    .await;

    // Give the loop one extra tick window to potentially fire
    // a wrong second save.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let join = handle.cancel();
    drop(tokio::time::timeout(CONVERGENCE_DEADLINE, join).await);

    assert!(converged, "loop did not converge within deadline");
    let saves = sink.save_calls();
    assert_eq!(
        saves.len(),
        1,
        "exactly one save expected (counter reset after first); got {saves:?}"
    );
}

/// Issue #639: a sustained burst must not save on every events-threshold
/// crossing.  Inside the minimum interval the events threshold is held back;
/// the counter keeps accumulating, so the save fires as soon as the interval
/// has passed.
#[test]
fn events_threshold_waits_for_min_interval() {
    let mut trigger = SaveTrigger::new();
    trigger.record(10);
    assert_eq!(
        trigger.evaluate(5, Duration::from_hours(24), Duration::from_hours(1)),
        None,
        "events past the threshold must wait for the minimum interval",
    );
    trigger.record(10);
    assert_eq!(
        trigger.evaluate(5, Duration::from_hours(24), Duration::ZERO),
        Some(SaveReason::EventsExceeded),
        "once the interval has passed, the held-back events save fires",
    );
    assert_eq!(
        trigger.evaluate(5, Duration::from_hours(24), Duration::ZERO),
        None,
        "the save reset the counter",
    );
}

/// The age ceiling is the durability backstop and is never held back by the
/// minimum interval: a drive with a few pending events still saves once the
/// age threshold passes.
#[test]
fn age_threshold_ignores_min_interval() {
    let mut trigger = SaveTrigger::new();
    trigger.record(1);
    assert_eq!(
        trigger.evaluate(1_000, Duration::ZERO, Duration::from_hours(1)),
        Some(SaveReason::AgeElapsed),
    );
}
