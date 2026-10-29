// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 7-E end-to-end integration test for the per-shard USN
//! journal loop.
//!
//! Drives 10 000 events through the full pipeline —
//! [`FakeJournalSource`] \u2192 [`JournalLoop`] \u2192
//! [`RecordingSink::accept`] \u2192 threshold check \u2192
//! [`RecordingSink::trigger_save`] \u2192 [`FakeCursorStore::store`] \u2014 and
//! pins the integrated contract:
//!
//! * **All events accepted** \u2014 the sink's `accept` calls collectively
//!   carry `TOTAL_EVENTS` change records (no events lost across tick
//!   boundaries, no events double-applied).
//! * **Multiple saves fire** \u2014 with `save_threshold_events = BATCH_SIZE +
//!   (BATCH_SIZE / 2)`, the threshold crosses inside roughly every other batch.
//!   A 10-batch run produces \u2265 4 saves.
//! * **Final cursor handed to sink** \u2014 the cursor handed to the sink
//!   advances to the post-final-batch value, proving the loop carries its read
//!   position forward to the sink (which persists it in lockstep with the body
//!   save; the sink-side lockstep is pinned in `cache::journal_sink`).
//! * **Cursor monotonicity** \u2014 across 10+ polls, every cursor passed into
//!   `JournalSource::poll` is \u2265 the previous one.
//! * **No false-positive wrap detection** \u2014 with a single stable
//!   `journal_id`, `sink.journal_wrapped` is **never** invoked.
//!
//! ## Sizing rationale
//!
//! 10 000 events split into 10 batches of 1 000 keeps the Mac tick
//! count low (one batch consumed per tick \u2192 ~10 ticks total) so the
//! 5 ms poll interval + `spawn_blocking` overhead lands the run
//! comfortably under 1 second on every supported runner.  Larger
//! batches (e.g. 100 000 / 100) would stress the [`FakeJournalSource`]'s
//! `Vec` allocations more than the loop semantics they aim to verify.
//!
//! [`FakeJournalSource`]: super::FakeJournalSource
//! [`JournalLoop`]: super::super::JournalLoop
//! [`RecordingSink::accept`]: super::RecordingSink
//! [`RecordingSink::trigger_save`]: super::RecordingSink
//! [`FakeCursorStore::store`]: super::FakeCursorStore

use alloc::sync::Arc;
use core::time::Duration;

use super::super::{CursorStore, JournalLoopConfig, JournalSource, PatchSink, spawn_journal_loop};
use super::{FakeCursorStore, FakeJournalSource, RecordingSink, one_change};

/// Total events driven through the pipeline in this end-to-end test.
const TOTAL_EVENTS: usize = 10_000;

/// Events per batch.  10 batches of 1 000 keeps tick count manageable.
const BATCH_SIZE: usize = 1_000;

/// Number of batches enqueued in the source (10).
const NUM_BATCHES: usize = TOTAL_EVENTS / BATCH_SIZE;

/// Save threshold tuned to fire roughly every other batch — at
/// 1 500 events per save and 1 000 events per batch, the second
/// batch's tick crosses 2 000 ≥ 1 500 → save; the fourth crosses
/// 4 000 → save (after the post-save reset to 0); etc.
const SAVE_THRESHOLD_EVENTS: u64 = 1_500;

/// Wall-clock deadline for the loop to drain all 10 000 events.
/// Generous: at 5 ms tick + spawn-blocking overhead, ~10 ticks
/// should land the full batch in well under 1 second; 5 s gives
/// a 50× safety margin against slower CI runners.
const E2E_DEADLINE: Duration = Duration::from_secs(5);

/// Polling interval inside the test for `wait_for_e2e` predicate
/// re-evaluation.  10 ms keeps the test responsive without
/// busy-waiting.
const E2E_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Fast polls, an events-only save threshold and no save gap, so the saves
/// [`ten_thousand_events_end_to_end`] counts come from the event volume alone.
fn end_to_end_config() -> JournalLoopConfig {
    JournalLoopConfig {
        poll_interval: Duration::from_millis(5),
        initial_cursor: 0,
        save_threshold_events: SAVE_THRESHOLD_EVENTS,
        save_threshold_age: Duration::from_hours(24),
        save_min_interval: Duration::ZERO,
        save_age_min_events: 0,
        save_age_ceiling: Duration::MAX,
        apply_interval: Duration::from_hours(24),
        apply_debounce: Duration::from_hours(24),
    }
}

#[tokio::test]
async fn ten_thousand_events_end_to_end() {
    let source = Arc::new(FakeJournalSource::new());
    let sink = Arc::new(RecordingSink::new());
    let cursor_store = Arc::new(FakeCursorStore::new());

    // Enqueue NUM_BATCHES batches of BATCH_SIZE events each.  FRS
    // values monotonically increase across batches so each event
    // is unique; next_cursor advances by BATCH_SIZE per batch.
    for batch_idx in 0..NUM_BATCHES {
        let mut changes = Vec::with_capacity(BATCH_SIZE);
        for event_idx in 0..BATCH_SIZE {
            let frs = ((batch_idx * BATCH_SIZE) + event_idx) as u64;
            changes.push(one_change(frs));
        }
        let next_cursor = ((batch_idx + 1) * BATCH_SIZE) as u64;
        source.enqueue_changes(changes, next_cursor);
    }

    let config = end_to_end_config();
    let handle = spawn_journal_loop(
        uffs_mft::platform::DriveLetter::C,
        Arc::clone(&source) as Arc<dyn JournalSource>,
        Arc::clone(&sink) as Arc<dyn PatchSink>,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
        config,
    );

    // Wait until all TOTAL_EVENTS have been accepted.  The
    // predicate sums the change-counts across every accept() call;
    // with a single drive letter and no wrap, this monotonically
    // increases until the source drains.
    let sink_for_pred = Arc::clone(&sink);
    let converged = wait_for_e2e(move || {
        let total: usize = sink_for_pred.calls().iter().map(|(_, count)| *count).sum();
        total >= TOTAL_EVENTS
    })
    .await;

    let join = handle.cancel();
    drop(tokio::time::timeout(Duration::from_secs(2), join).await);

    assert!(
        converged,
        "loop did not drain {TOTAL_EVENTS} events within {E2E_DEADLINE:?}"
    );

    // ── All events accepted ─────────────────────────────────────────
    let calls = sink.calls();
    let total_accepted: usize = calls.iter().map(|(_, count)| *count).sum();
    assert_eq!(
        total_accepted, TOTAL_EVENTS,
        "sink.accept must have received exactly {TOTAL_EVENTS} events; got {total_accepted}"
    );
    // Every accept call was for letter 'C'.
    assert!(
        calls
            .iter()
            .all(|(letter, _)| *letter == uffs_mft::platform::DriveLetter::C),
        "every accept() must be for drive 'C'; got {calls:?}"
    );

    // ── Multiple saves fired ────────────────────────────────────────
    let saves = sink.save_calls();
    let save_threshold_usize = usize::try_from(SAVE_THRESHOLD_EVENTS).unwrap_or(usize::MAX);
    let expected_min_saves = (TOTAL_EVENTS / save_threshold_usize).saturating_sub(1);
    assert!(
        saves.len() >= expected_min_saves,
        "expected at least {expected_min_saves} saves for {TOTAL_EVENTS} events @ \
         threshold {SAVE_THRESHOLD_EVENTS}; got {} saves: {saves:?}",
        saves.len()
    );
    assert!(
        saves
            .iter()
            .all(|(letter, _)| *letter == uffs_mft::platform::DriveLetter::C),
        "every save must be for drive 'C'; got {saves:?}"
    );

    // ── Final cursor handed to sink ─────────────────────────────────
    // The loop forwards its read position to the sink on every save;
    // persistence then happens sink-side in lockstep with the body
    // save (so the loop itself never writes the cursor store on a
    // save tick).
    let save_cursors = sink.save_cursors();
    assert!(
        !save_cursors.is_empty(),
        "sink must have received at least one save cursor; got empty list"
    );
    // The largest cursor handed to the sink should match TOTAL_EVENTS
    // (the final batch's next_cursor).
    let max_handed = save_cursors.iter().copied().max().unwrap_or(0);
    assert!(
        max_handed >= TOTAL_EVENTS as u64,
        "highest cursor handed to the sink must be ≥ {TOTAL_EVENTS}; got {max_handed}"
    );
    // The loop must not persist the cursor itself on a save tick —
    // that moved to the sink so a parked shard's no-op save can't
    // advance the on-disk cursor past the on-disk body.
    assert!(
        cursor_store.store_log().is_empty(),
        "loop must not write the cursor store on save ticks; got {:?}",
        cursor_store.store_log(),
    );

    // ── Cursor monotonicity ─────────────────────────────────────────
    let cursors_seen = source.cursors_seen();
    assert!(
        cursors_seen.len() >= NUM_BATCHES,
        "source must have been polled at least {NUM_BATCHES} times; got {}",
        cursors_seen.len()
    );
    for window in cursors_seen.windows(2) {
        let prev = window.first().copied().unwrap_or(0);
        let curr = window.get(1).copied().unwrap_or(0);
        assert!(
            curr >= prev,
            "cursor must advance monotonically; observed regression {prev} → {curr} in {cursors_seen:?}"
        );
    }

    // ── No false-positive wrap detection ────────────────────────────
    let wraps = sink.wrap_calls();
    assert!(
        wraps.is_empty(),
        "single-journal_id run must NOT trigger any wrap detection; got {wraps:?}"
    );
}

/// E2E predicate-poll helper with a longer deadline ([`E2E_DEADLINE`]
/// = 5 s) than the standard `super::wait_for` ([`super::CONVERGENCE_DEADLINE`]
/// = 250 ms).  10 000 events at 5 ms tick + spawn-blocking overhead
/// can take up to ~1 second; the 5 s budget keeps the test reliable
/// across slower CI runners.
async fn wait_for_e2e<F: Fn() -> bool>(predicate: F) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < E2E_DEADLINE {
        if predicate() {
            return true;
        }
        tokio::time::sleep(E2E_POLL_INTERVAL).await;
    }
    predicate()
}
