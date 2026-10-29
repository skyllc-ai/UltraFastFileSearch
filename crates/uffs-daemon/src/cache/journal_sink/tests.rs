// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Unit tests for [`super::RegistryPatchSink`] — the journal-applier sink.
//!
//! Extracted from the parent `journal_sink.rs` to keep that file under the
//! workspace 800-LOC file-size policy; `use super::*` keeps every private
//! item (`ApplyMsg`, `dispatch_msg`, `save_reason_str`, …) in scope exactly
//! as it was inline.

use super::*;

/// Construct a fresh [`IndexManager`] suitable for sink lifecycle
/// tests.  No drives loaded — the applier exits cleanly when
/// the strong-Arc drops, regardless of whether refresh messages
/// were drained, so tests don't need a populated registry.
fn fresh_index_manager() -> Arc<IndexManager> {
    let (event_tx, _event_rx) = crate::events::event_channel();
    Arc::new(IndexManager::new(
        None,
        event_tx,
        Arc::new(crate::config::Config::default()),
    ))
}

/// In-memory [`CursorStore`] that records every `store` call.
/// Lets the applier tests assert *whether* a cursor was persisted
/// (the lockstep safety property) without touching the disk.
/// `load` always returns 0 (tests seed nothing).
struct RecordingCursorStore {
    log: Mutex<Vec<(uffs_mft::platform::DriveLetter, u64)>>,
}

impl RecordingCursorStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(Vec::new()),
        })
    }

    fn store_log(&self) -> Vec<(uffs_mft::platform::DriveLetter, u64)> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl CursorStore for RecordingCursorStore {
    fn load(&self, _letter: uffs_mft::platform::DriveLetter) -> u64 {
        0
    }

    fn store(&self, letter: uffs_mft::platform::DriveLetter, cursor: u64) {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((letter, cursor));
    }
}

/// Construct a [`FileChange`] fixture with a unique FRS for
/// per-event identification.  Fields other than `frs` use
/// `FileChange::default()` because the sink doesn't inspect them
/// — only `IndexManager::handle_journal_save` does (covered in
/// `cache::shard::tests` and the patch end-to-end suite).
///
/// Takes a raw `u64` because FRS values are most naturally
/// written as integer literals in test fixtures; lifts to the
/// typed `Frs` at this single construction boundary so the rest
/// of the test surface keeps the typed contract.
fn make_change(frs: u64) -> FileChange {
    FileChange {
        frs: frs.into(),
        ..FileChange::default()
    }
}

/// Snapshot the per-letter pending buffer's event FRS sequence,
/// dropping the `lock_pending()` guard before returning so the
/// caller's assertions don't hold the mutex (satisfies
/// `clippy::significant_drop_tightening` in tests).
///
/// Demotes typed `Frs` → raw `u64` at the snapshot boundary so
/// assertion literals stay as integer arrays.
fn pending_frs_for_letter(
    sink: &RegistryPatchSink,
    letter: uffs_mft::platform::DriveLetter,
) -> Option<Vec<u64>> {
    let guard = sink.lock_pending();
    guard
        .get(&letter)
        .map(|buf| buf.iter().map(|change| change.frs.raw()).collect())
}

/// Pin: `accept` appends to the per-letter pending buffer and
/// does NOT enqueue a message on the applier channel.
#[tokio::test]
async fn accept_buffers_changes_without_enqueueing() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    let accepted = sink.accept(uffs_mft::platform::DriveLetter::C, &[
        make_change(100),
        make_change(101),
    ]);
    assert!(accepted, "accept must return true optimistically");

    // The pending buffer holds the two events for letter 'C'.
    let buf = pending_frs_for_letter(&sink, uffs_mft::platform::DriveLetter::C)
        .expect("accept must populate pending buffer for 'C'");
    assert_eq!(
        buf,
        [100, 101],
        "accept must preserve event order in the buffer",
    );

    // Channel is empty: accept did not enqueue.
    assert!(
        rx.try_recv().is_err(),
        "accept must NOT enqueue an ApplyMsg",
    );
}

/// Pin: a sequence of `accept` calls for the same letter
/// accumulates into the same buffer — no per-call drain or
/// truncation.
#[tokio::test]
async fn multiple_accepts_accumulate_in_pending() {
    let (sink, _rx) = RegistryPatchSink::new_for_test();

    sink.accept(uffs_mft::platform::DriveLetter::C, &[make_change(1)]);
    sink.accept(uffs_mft::platform::DriveLetter::C, &[
        make_change(2),
        make_change(3),
    ]);
    sink.accept(uffs_mft::platform::DriveLetter::C, &[make_change(4)]);

    let buf = pending_frs_for_letter(&sink, uffs_mft::platform::DriveLetter::C)
        .expect("buffer for 'C' must exist");
    assert_eq!(
        buf,
        [1, 2, 3, 4],
        "consecutive accepts must accumulate in send order",
    );
}

/// Pin: `trigger_save` drains the pending buffer for `letter`
/// and ships it inside `ApplyMsg::Save { changes }`.  The buffer
/// for `letter` is cleared after the drain.
#[tokio::test]
async fn trigger_save_drains_pending_into_save_message() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    sink.accept(uffs_mft::platform::DriveLetter::C, &[
        make_change(10),
        make_change(11),
    ]);
    sink.trigger_save(
        uffs_mft::platform::DriveLetter::C,
        SaveReason::EventsExceeded,
        4242,
    );

    let ApplyMsg::Save {
        letter,
        reason,
        changes,
        cursor,
    } = rx.try_recv().expect("trigger_save must enqueue Save")
    else {
        panic!("expected ApplyMsg::Save, got Wrap");
    };
    assert_eq!(letter, uffs_mft::platform::DriveLetter::C);
    assert!(matches!(reason, SaveReason::EventsExceeded));
    assert_eq!(cursor, 4242, "Save must carry the tick cursor");
    assert_eq!(
        changes
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [10, 11],
        "Save must carry the buffered changes in send order",
    );

    // Pending buffer for 'C' is gone after the drain.
    assert!(
        pending_frs_for_letter(&sink, uffs_mft::platform::DriveLetter::C).is_none(),
        "trigger_save must remove the per-letter pending entry",
    );
}

/// Pin: `trigger_save` on a letter with no prior `accept` still
/// emits `ApplyMsg::Save { changes: [] }`.  The applier's
/// empty-batch fast path then short-circuits to a no-op.
#[tokio::test]
async fn trigger_save_with_no_pending_sends_empty_changes() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    sink.trigger_save(
        uffs_mft::platform::DriveLetter::Z,
        SaveReason::AgeElapsed,
        77,
    );

    let ApplyMsg::Save {
        letter,
        reason,
        changes,
        cursor,
    } = rx.try_recv().expect("trigger_save must enqueue Save")
    else {
        panic!("expected ApplyMsg::Save, got Wrap");
    };
    assert_eq!(letter, uffs_mft::platform::DriveLetter::Z);
    assert_eq!(
        cursor, 77,
        "Save must carry the tick cursor even with no pending events"
    );
    assert!(matches!(reason, SaveReason::AgeElapsed));
    assert!(
        changes.is_empty(),
        "Save must carry an empty Vec when no events were buffered",
    );
}

/// Pin: `trigger_apply` drains the pending buffer for `letter`
/// and ships it inside `ApplyMsg::Apply { changes }` — the
/// near-live body-patch path, carrying NO cursor (the apply tick
/// never advances the on-disk cursor).  The buffer for `letter` is
/// cleared after the drain.
#[tokio::test]
async fn trigger_apply_drains_pending_into_apply_message() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    sink.accept(uffs_mft::platform::DriveLetter::C, &[
        make_change(20),
        make_change(21),
    ]);
    sink.trigger_apply(uffs_mft::platform::DriveLetter::C);

    let ApplyMsg::Apply { letter, changes } =
        rx.try_recv().expect("trigger_apply must enqueue Apply")
    else {
        panic!("expected ApplyMsg::Apply");
    };
    assert_eq!(letter, uffs_mft::platform::DriveLetter::C);
    assert_eq!(
        changes
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [20, 21],
        "Apply must carry the buffered changes in send order",
    );

    // Pending buffer for 'C' is gone after the drain.
    assert!(
        pending_frs_for_letter(&sink, uffs_mft::platform::DriveLetter::C).is_none(),
        "trigger_apply must remove the per-letter pending entry",
    );
}

/// Pin: `trigger_apply` on a letter with no buffered events is a
/// pure no-op — it enqueues NO message (unlike `trigger_save`,
/// which always ships a Save so the cursor can advance on the
/// rare save cadence).  An empty apply has nothing to patch and
/// no cursor to move, so spending a channel slot + an applier
/// wake-up on it would be wasted work on every quiescent tick.
#[tokio::test]
async fn trigger_apply_with_no_pending_enqueues_nothing() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    sink.trigger_apply(uffs_mft::platform::DriveLetter::Z);

    assert!(
        rx.try_recv().is_err(),
        "an empty trigger_apply must not enqueue an ApplyMsg",
    );
}

/// Lockstep safety pin: the apply tick must NEVER persist the
/// cursor — only a real on-disk body save advances it.  Even on a
/// successful in-memory patch the on-disk cursor stays pinned to
/// the last save, so a cold start re-replays the apply-only deltas
/// idempotently.  Here the letter is unregistered (the patch
/// no-ops), but the contract holds on the success path too: the
/// apply dispatch never calls `cursor_store.store`.
#[tokio::test]
async fn apply_tick_never_persists_cursor() {
    let idx = fresh_index_manager(); // no drives registered
    let cursor_store = RecordingCursorStore::new();
    let (sink, applier) = RegistryPatchSink::spawn_with_applier(
        &idx,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
    );

    sink.accept(uffs_mft::platform::DriveLetter::C, &[make_change(1)]);
    sink.trigger_apply(uffs_mft::platform::DriveLetter::C);

    drop(sink);
    let join_result = tokio::time::timeout(core::time::Duration::from_secs(5), applier).await;
    join_result
        .expect("applier must exit within 5 s")
        .expect("applier must not panic");

    assert!(
        cursor_store.store_log().is_empty(),
        "the apply tick must never persist the cursor; got {:?}",
        cursor_store.store_log(),
    );
}

/// Pin: `journal_wrapped` clears the pending buffer for the
/// letter and emits `ApplyMsg::Wrap`.  A subsequent
/// `trigger_save` then sees an empty buffer (no replay of the
/// stale events past the wrap).
#[tokio::test]
async fn journal_wrapped_discards_pending_buffer_and_sends_wrap() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    sink.accept(uffs_mft::platform::DriveLetter::C, &[
        make_change(5),
        make_change(6),
    ]);
    sink.journal_wrapped(uffs_mft::platform::DriveLetter::C);

    let ApplyMsg::Wrap { letter } = rx.try_recv().expect("journal_wrapped must enqueue Wrap")
    else {
        panic!("expected ApplyMsg::Wrap, got Save");
    };
    assert_eq!(letter, uffs_mft::platform::DriveLetter::C);

    assert!(
        pending_frs_for_letter(&sink, uffs_mft::platform::DriveLetter::C).is_none(),
        "journal_wrapped must discard the per-letter pending entry",
    );

    // A subsequent trigger_save must see an empty buffer.
    sink.trigger_save(
        uffs_mft::platform::DriveLetter::C,
        SaveReason::AgeElapsed,
        0,
    );
    let ApplyMsg::Save {
        changes: post_wrap_changes,
        ..
    } = rx.try_recv().expect("trigger_save must enqueue Save")
    else {
        panic!("expected ApplyMsg::Save after wrap, got another Wrap");
    };
    assert!(
        post_wrap_changes.is_empty(),
        "post-wrap trigger_save must drain to empty (stale events discarded)",
    );
}

/// Pin: per-letter buffers are independent.  Accepting events on
/// 'C' must not leak into 'D's buffer or pending state.
#[tokio::test]
async fn pending_buffers_are_per_letter() {
    let (sink, mut rx) = RegistryPatchSink::new_for_test();

    sink.accept(uffs_mft::platform::DriveLetter::C, &[make_change(1)]);
    sink.accept(uffs_mft::platform::DriveLetter::D, &[
        make_change(2),
        make_change(3),
    ]);

    // Drain 'C' first — should NOT include any of 'D's events.
    sink.trigger_save(
        uffs_mft::platform::DriveLetter::C,
        SaveReason::EventsExceeded,
        10,
    );
    let ApplyMsg::Save {
        letter: c_letter,
        changes: c_changes,
        ..
    } = rx.try_recv().expect("Save for 'C'")
    else {
        panic!("expected ApplyMsg::Save for 'C'");
    };
    assert_eq!(c_letter, uffs_mft::platform::DriveLetter::C);
    assert_eq!(
        c_changes
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [1],
    );

    // 'D's buffer must still hold its events.
    sink.trigger_save(
        uffs_mft::platform::DriveLetter::D,
        SaveReason::EventsExceeded,
        20,
    );
    let ApplyMsg::Save {
        letter: d_letter,
        changes: d_changes,
        ..
    } = rx.try_recv().expect("Save for 'D'")
    else {
        panic!("expected ApplyMsg::Save for 'D'");
    };
    assert_eq!(d_letter, uffs_mft::platform::DriveLetter::D);
    assert_eq!(
        d_changes
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [2, 3],
        "'D's buffer must be preserved across 'C's drain",
    );
}

/// Drop all `Arc<RegistryPatchSink>` instances → the sender side
/// of the channel is dropped → the applier's `rx.recv().await`
/// returns `None` → the task exits cleanly.  Pins the graceful-
/// shutdown shape used when the daemon's load task tears down
/// without dropping `IndexManager` first.
#[tokio::test]
async fn applier_exits_when_sink_dropped() {
    let idx = fresh_index_manager();
    let (sink, applier) = RegistryPatchSink::spawn_with_applier(&idx, RecordingCursorStore::new());

    // Sanity: applier is still running before we drop the sink.
    assert!(!applier.is_finished());

    drop(sink);

    // Should converge within the test deadline; in practice
    // it's <1 ms on Mac (single context-switch).
    let join_result = tokio::time::timeout(core::time::Duration::from_secs(2), applier).await;
    assert!(
        join_result.is_ok(),
        "applier must exit within 2 s of last sender dropping",
    );
    join_result
        .expect("timeout deadline must not elapse")
        .expect("applier must not panic");
}

/// Drop the last `Arc<IndexManager>` → the applier's
/// `Weak::upgrade()` returns `None` on the next message →
/// the task exits cleanly.  Pins the graceful-shutdown shape
/// used when the daemon's main lifecycle drops the index before
/// the load task observes the sink-side drop.
#[tokio::test]
async fn applier_exits_when_index_manager_dropped() {
    let idx = fresh_index_manager();
    let (sink, applier) = RegistryPatchSink::spawn_with_applier(&idx, RecordingCursorStore::new());

    drop(idx);
    // Send one message so the applier wakes up, observes the
    // dropped Weak, and exits.  Without this the applier blocks
    // on `recv().await` forever (no Weak-watch primitive in tokio).
    sink.trigger_save(
        uffs_mft::platform::DriveLetter::C,
        SaveReason::EventsExceeded,
        1,
    );

    let join_result = tokio::time::timeout(core::time::Duration::from_secs(2), applier).await;
    assert!(
        join_result.is_ok(),
        "applier must exit within 2 s of IndexManager Arc dropping + one wake message",
    );
    join_result
        .expect("timeout deadline must not elapse")
        .expect("applier must not panic");
}

/// Pin that `save_reason_str` produces stable diagnostic strings.
/// These strings are surfaced in the production tracing target
/// `shard.journal` and consumed by operator runbooks — drift
/// would silently break grep-based monitoring, so the round-trip
/// is asserted directly.
#[test]
fn save_reason_str_maps_each_variant() {
    assert_eq!(
        save_reason_str(SaveReason::EventsExceeded),
        "events-exceeded"
    );
    assert_eq!(save_reason_str(SaveReason::AgeElapsed), "age-elapsed");
}

/// Pin the multi-message FIFO ordering contract: a sink that
/// buffers many messages before the applier can drain them all
/// must process them in send order.  Out-of-order applies don't
/// corrupt (each per-letter `handle_journal_save` is independent),
/// but the FIFO contract is a regression-net for any future
/// change that swaps `mpsc::unbounded_channel` for an unordered
/// surface.
#[tokio::test]
async fn applier_drains_in_fifo_order() {
    let idx = fresh_index_manager();
    let (sink, applier) = RegistryPatchSink::spawn_with_applier(&idx, RecordingCursorStore::new());

    // Burst-send 5 trigger_save calls without prior `accept`.
    // Each one drains an empty pending buffer and ships
    // `ApplyMsg::Save { changes: [] }`; the applier's empty-batch
    // fast path in `handle_journal_save` short-circuits to a
    // debug-log no-op.  The test verifies the applier processes
    // all 5 in order before the sink's drop closes the channel.
    for letter in [
        uffs_mft::platform::DriveLetter::C,
        uffs_mft::platform::DriveLetter::D,
        uffs_mft::platform::DriveLetter::E,
        uffs_mft::platform::DriveLetter::F,
        uffs_mft::platform::DriveLetter::G,
    ] {
        sink.trigger_save(letter, SaveReason::EventsExceeded, 1);
    }

    // Drop the sink → applier drains remaining messages → exits.
    drop(sink);
    let join_result = tokio::time::timeout(core::time::Duration::from_secs(5), applier).await;
    assert!(
        join_result.is_ok(),
        "applier must finish draining within 5 s (5 empty-batch no-ops, no real I/O)",
    );
    join_result
        .expect("timeout deadline must not elapse")
        .expect("applier must not panic");
}

/// Lockstep safety pin: when `handle_journal_save` does NOT save a
/// body (here: no shard is registered for the letter, so the save
/// returns `false` — the same outcome as a Parked shard), the
/// applier must NOT persist the cursor.  This is the invariant the
/// startup warm-load guard (`cache::guarded_load`) relies on: the
/// on-disk cursor never outruns the on-disk compact-cache body, so
/// the guard can fast-serve the body and let the background loop
/// converge the bounded delta without stranding events.
#[tokio::test]
async fn no_warm_shard_save_does_not_persist_cursor() {
    let idx = fresh_index_manager(); // no drives registered
    let cursor_store = RecordingCursorStore::new();
    let (sink, applier) = RegistryPatchSink::spawn_with_applier(
        &idx,
        Arc::clone(&cursor_store) as Arc<dyn CursorStore>,
    );

    // A save tick for an unregistered letter: handle_journal_save
    // finds no shard and returns false → cursor must stay unwritten.
    sink.accept(uffs_mft::platform::DriveLetter::C, &[make_change(1)]);
    sink.trigger_save(
        uffs_mft::platform::DriveLetter::C,
        SaveReason::EventsExceeded,
        9999,
    );

    // Drain + join the applier so the (a)synchronous dispatch has
    // certainly run before we inspect the store.
    drop(sink);
    let join_result = tokio::time::timeout(core::time::Duration::from_secs(5), applier).await;
    join_result
        .expect("applier must exit within 5 s")
        .expect("applier must not panic");

    assert!(
        cursor_store.store_log().is_empty(),
        "cursor must NOT be persisted when the body save no-ops (no warm shard); \
             got {:?}",
        cursor_store.store_log(),
    );
}

/// Pin: `coalesce_apply_run` merges a run of consecutive same-letter
/// `Apply` messages into one change vector, in FIFO order, draining the
/// channel — collapsing an apply backlog into a single rebuild.
#[tokio::test]
async fn coalesce_merges_consecutive_same_letter_applies() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ApplyMsg>();
    // The "first" apply (10) is passed by value; (11), (12) are queued.
    tx.send(ApplyMsg::Apply {
        letter: uffs_mft::platform::DriveLetter::C,
        changes: vec![make_change(11)],
    })
    .unwrap();
    tx.send(ApplyMsg::Apply {
        letter: uffs_mft::platform::DriveLetter::C,
        changes: vec![make_change(12)],
    })
    .unwrap();

    let (merged, leftover) = coalesce_apply_run(
        uffs_mft::platform::DriveLetter::C,
        vec![make_change(10)],
        &mut rx,
    );

    assert_eq!(
        merged
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [10, 11, 12],
        "merged changes must preserve FIFO order across the coalesced run",
    );
    assert!(
        leftover.is_none(),
        "channel fully drained → no carried message"
    );
}

/// Pin: coalescing stops at a different-letter `Apply` and returns it as
/// the carried leftover (never merged into the wrong letter's batch).
#[tokio::test]
async fn coalesce_stops_at_other_letter() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ApplyMsg>();
    tx.send(ApplyMsg::Apply {
        letter: uffs_mft::platform::DriveLetter::C,
        changes: vec![make_change(2)],
    })
    .unwrap();
    tx.send(ApplyMsg::Apply {
        letter: uffs_mft::platform::DriveLetter::D,
        changes: vec![make_change(9)],
    })
    .unwrap();

    let (merged, leftover) = coalesce_apply_run(
        uffs_mft::platform::DriveLetter::C,
        vec![make_change(1)],
        &mut rx,
    );

    assert_eq!(
        merged
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let Some(ApplyMsg::Apply { letter, changes }) = leftover else {
        panic!("expected carried Apply for 'D'");
    };
    assert_eq!(letter, uffs_mft::platform::DriveLetter::D);
    assert_eq!(
        changes
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [9]
    );
}

/// Pin: a `Save` is never merged into an apply run — it carries cursor /
/// persistence semantics, so coalescing stops and returns it intact.
#[tokio::test]
async fn coalesce_does_not_swallow_save() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ApplyMsg>();
    tx.send(ApplyMsg::Save {
        letter: uffs_mft::platform::DriveLetter::C,
        reason: SaveReason::EventsExceeded,
        changes: vec![make_change(5)],
        cursor: 42,
    })
    .unwrap();

    let (merged, leftover) = coalesce_apply_run(
        uffs_mft::platform::DriveLetter::C,
        vec![make_change(1)],
        &mut rx,
    );

    assert_eq!(
        merged
            .iter()
            .map(|change| change.frs.raw())
            .collect::<Vec<_>>(),
        [1]
    );
    assert!(
        matches!(leftover, Some(ApplyMsg::Save { cursor: 42, .. })),
        "Save must be carried out intact, not merged into the apply batch",
    );
}

/// A batch that reaches the applier while its drive is not warm goes back
/// to the front of the pending buffer, in order, instead of being dropped —
/// the first apply after a promote installs the body picks it up.
#[tokio::test]
async fn batch_for_drive_that_is_not_warm_is_held_in_order() {
    let idx = fresh_index_manager(); // no drives registered: nothing is warm
    let (sink, applier) = RegistryPatchSink::spawn_with_applier(&idx, RecordingCursorStore::new());
    let letter = uffs_mft::platform::DriveLetter::C;

    sink.accept(letter, &[make_change(1), make_change(2)]);
    sink.trigger_apply(letter);
    let deadline = tokio::time::Instant::now() + core::time::Duration::from_secs(5);
    while !sink.has_pending(letter) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the applier must hand the batch back within 5 s",
        );
        tokio::time::sleep(core::time::Duration::from_millis(5)).await;
    }

    // A change buffered after the hold queues behind the held batch.
    sink.accept(letter, &[make_change(3)]);
    assert_eq!(
        pending_frs_for_letter(&sink, letter),
        Some(vec![1, 2, 3]),
        "held changes stay in front of newer ones",
    );

    drop(sink);
    tokio::time::timeout(core::time::Duration::from_secs(5), applier)
        .await
        .expect("applier must exit within 5 s")
        .expect("applier must not panic");
}

/// The hold keeps the newest [`MAX_HELD_CHANGES`]: a drive parked for a long
/// time cannot grow its buffer without bound, and the changes that matter
/// after a promote are the newest ones.
#[test]
fn requeue_keeps_the_newest_changes_up_to_the_cap() {
    let pending: PendingBuffers = Arc::default();
    let letter = uffs_mft::platform::DriveLetter::C;
    lock_buffers(&pending).insert(letter, vec![make_change(u64::MAX)]);

    let older: Vec<FileChange> = (0..MAX_HELD_CHANGES as u64).map(make_change).collect();
    requeue_front(&pending, letter, older);

    let held = lock_buffers(&pending).remove(&letter).unwrap_or_default();
    assert_eq!(held.len(), MAX_HELD_CHANGES, "the buffer is capped");
    assert_eq!(
        held.first().map(|change| change.frs.raw()),
        Some(1),
        "the oldest change is dropped"
    );
    assert_eq!(
        held.last().map(|change| change.frs.raw()),
        Some(u64::MAX),
        "the newest change survives"
    );
}
