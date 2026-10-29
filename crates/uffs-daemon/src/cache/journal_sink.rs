// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Production [`PatchSink`] for the per-shard journal loop (Phase 7
//! activation A3, Phase 8 surgical-patch B3).
//!
//! ## Architecture — buffered applier-task pattern
//!
//! [`RegistryPatchSink::accept`] / [`RegistryPatchSink::trigger_save`] /
//! [`RegistryPatchSink::journal_wrapped`] are **synchronous** callbacks
//! invoked from the journal loop's `process_tick` (which itself runs
//! in async context via [`crate::cache::journal_loop::JournalLoop::run`]).
//! The downstream work (registry write-lock acquisition, body patching,
//! background save) is `async` and uses [`tokio::sync::RwLock`].
//! Calling `Handle::block_on` from the sync callback would re-enter the
//! runtime and deadlock.
//!
//! Phase 7 fix: each callback **enqueues** an [`ApplyMsg`] onto an
//! [`tokio::sync::mpsc::UnboundedSender`] (sync, non-blocking) and
//! returns immediately.  A single async **applier task** owns the
//! receiver and processes messages serially.
//!
//! Phase 8 B3 extension: per-letter
//! [`std::sync::Mutex<HashMap<uffs_mft::platform::DriveLetter,
//! Vec<FileChange>>>`] **pending buffer** owned by the sink.  `accept` appends
//! to the buffer synchronously (no mpsc traffic).  `trigger_save` drains the
//! buffer for that letter and ships the drained `Vec<FileChange>` into
//! [`ApplyMsg::Save`] so the applier can run a *surgical*
//! [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`] instead of a
//! full [`uffs_core::compact_loader::load_drive_with_usn_refresh`].
//! `journal_wrapped` discards the buffer (a wrap means the journal
//! head reset, so any pending events are stale relative to the new
//! cursor) and falls back to the Phase-7 full-reload path.
//!
//! ## Apply / save cadence split (search-freshness)
//!
//! Draining the buffer only on the save tick (50k events / 5 min) left
//! freshly created / renamed / deleted files invisible to search for
//! up to 5 minutes.  `trigger_apply` decouples the two cadences: the
//! loop fires it on the apply interval (default ~30 s) to drain the buffer
//! into [`ApplyMsg::Apply`], which patches + swaps the in-memory body
//! (search goes near-live) but **skips** the compact-cache disk write
//! and the cursor persist.  `trigger_save` keeps doing the full
//! patch-plus-persist on its rare cadence, so a save tick subsumes an
//! apply.  The loop never fires both on the same poll; whichever fires
//! drains the buffer.  Because only a real body save advances the
//! on-disk cursor, a cold start re-replays the apply-only deltas from
//! the last saved cursor — idempotent against the freshly loaded body.
//!
//! Properties of the buffered design:
//!
//! 1. Preserves FIFO ordering (per-letter and across letters).
//! 2. Keeps the loop's hot path zero-cost — accept is `Vec::extend_from_slice`
//!    on a per-letter `Vec<FileChange>` under a short-held mutex.
//! 3. Save-tick latency is independent of accept-tick volume — the journal loop
//!    never blocks on the patch / swap / persist sequence.
//!
//! ## Lifecycle
//!
//! * Construction is via [`RegistryPatchSink::spawn_with_applier`] which
//!   returns `(Arc<Self>, JoinHandle<()>)`.  The caller (
//!   `lib.rs::spawn_journal_loops_for_warm_shards`, commit A4) keeps the
//!   `JoinHandle` and aborts it during graceful shutdown.
//!
//! * The applier holds [`alloc::sync::Weak<IndexManager>`] so the sink +
//!   applier pair never extends the daemon's lifetime past shutdown.  When all
//!   `Arc<IndexManager>` instances drop, the `Weak::upgrade` arm returns `None`
//!   and the applier exits cleanly.
//!
//! * Dropping all sinks (sink Arc count → 0) closes the `mpsc` sender.  The
//!   applier's `recv().await` then returns `None` and the task exits — same
//!   shutdown shape as the `Weak` path.

use alloc::sync::{Arc, Weak};
use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uffs_mft::usn::FileChange;

use super::journal_loop::{CursorStore, PatchSink, SaveReason};
use crate::index::IndexManager;
use crate::index::journal_save::SaveSlot;

/// Cross-task message from a sync sink callback to the async applier.
///
/// Phase 8 dropped the Phase-7 `Accept` variant: per-letter event
/// buffering moved into the sink itself ([`RegistryPatchSink::pending`])
/// so `accept` no longer puts traffic on the channel.
#[derive(Debug)]
enum ApplyMsg {
    /// `trigger_save` callback — the applier runs a surgical
    /// [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`] over
    /// the drained per-letter buffer, then `replace_warm_body` +
    /// `save_compact_cache_background`.  The applier converts
    /// [`SaveReason`] to a stable diagnostic string
    /// (`"events-exceeded"` / `"age-elapsed"`) for the success /
    /// failure log.
    Save {
        /// Drive letter to refresh.
        letter: uffs_mft::platform::DriveLetter,
        /// Why the save threshold fired.
        reason: SaveReason,
        /// Drained per-letter event buffer.  Empty on age-elapsed
        /// triggers when the drive saw no churn since the last save
        /// (the surgical-patch path short-circuits to a no-op).
        changes: Vec<FileChange>,
        /// The journal loop's read position at this save tick.  The
        /// applier persists it via the cursor store **only when the
        /// compact-cache body save actually succeeds**, keeping the
        /// on-disk cursor in lockstep with the on-disk body (a parked
        /// shard's save is a no-op, so its cursor must not advance).
        cursor: u64,
    },
    /// `trigger_apply` callback — the short apply-cadence sibling of
    /// `Save`.  The applier runs the same surgical
    /// [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`] +
    /// `replace_warm_body` over the drained per-letter buffer so the
    /// in-memory body (and therefore search) goes near-live, but
    /// **skips** the compact-cache disk write and the cursor persist.
    /// Disk persistence stays on the rarer `Save` tick; the cursor only
    /// advances on a real body save, so a cold start re-replays the
    /// in-between deltas idempotently.
    Apply {
        /// Drive letter to patch.
        letter: uffs_mft::platform::DriveLetter,
        /// Drained per-letter event buffer.  Empty when no churn
        /// accumulated since the last apply / save (the surgical-patch
        /// path short-circuits to a no-op).
        changes: Vec<FileChange>,
    },
    /// `journal_wrapped` callback — the journal head reset so any
    /// pending events are stale; the applier discards them in the
    /// sink and runs a full
    /// [`IndexManager::handle_journal_refresh`] to resync the body
    /// against the new journal head.
    Wrap {
        /// Drive letter whose USN journal was recreated.
        letter: uffs_mft::platform::DriveLetter,
    },
}

/// Production [`PatchSink`] wired to the registry via an applier task.
///
/// Holds two pieces of state:
///
/// * `apply_tx` — the sender side of the applier task's mpsc channel.  `accept`
///   does NOT use it; `trigger_save` and `journal_wrapped` do.
/// * `pending` — per-letter buffer of [`FileChange`] entries that `accept` has
///   appended since the last save / wrap.
///
/// Both are owned by the sink Arc; cloning the Arc is cheap and the
/// inner state is shared across every per-shard journal loop that
/// holds a clone.
pub(crate) struct RegistryPatchSink {
    /// Channel into the applier task.  `UnboundedSender::send` is
    /// sync-non-blocking, which is exactly what the loop's sync
    /// callback contract requires.  Failures (receiver dropped /
    /// applier exited) are silently absorbed — the loop's cursor
    /// still advances and the per-shard `SaveTrigger` counters still
    /// reset, so the visible behaviour on a dead applier is "this
    /// shard's in-memory body stops refreshing" which is the
    /// correct degraded state.
    apply_tx: mpsc::UnboundedSender<ApplyMsg>,
    /// Per-letter pending [`FileChange`] buffer accumulated by
    /// `accept` between save / wrap triggers.  Drained on
    /// `trigger_save` (forwarded to the applier in
    /// [`ApplyMsg::Save`]) and discarded on `journal_wrapped`
    /// (the wrap full-reload supersedes any pending patches).
    ///
    /// Wrapped in [`std::sync::Mutex`] (not [`tokio::sync::Mutex`])
    /// because every access is from the *sync* sink callbacks; the
    /// critical section is `Vec::extend_from_slice` /
    /// `HashMap::remove` — microseconds at most, so the brief lock
    /// contention is invisible relative to the journal loop's 500 ms
    /// tick cadence.  Poisoning is recovered via
    /// [`std::sync::PoisonError::into_inner`] (matching the
    /// `lock_journal_handles` helper in `index/journal.rs`).
    pending: PendingBuffers,
}

/// The per-letter pending buffers, shared between the sink (which fills
/// them) and the applier task (which puts a not-yet-warm drive's batch
/// back, see [`requeue_front`]).
type PendingBuffers = Arc<Mutex<HashMap<uffs_mft::platform::DriveLetter, Vec<FileChange>>>>;

/// Newest changes a drive that is not warm may hold for its next apply.
///
/// While a drive is parked its changes cannot be applied; the next promote
/// replays the journal from the persisted cursor itself, so only the changes
/// that arrive after that replay's last read are needed — seconds' worth.
/// Keeping the newest 100 000 covers that with room to spare and bounds the
/// memory of a drive that stays parked for days.
const MAX_HELD_CHANGES: usize = 100_000;

impl RegistryPatchSink {
    /// Construct a sink + spawn its applier task.
    ///
    /// Returns `(Arc<Self>, JoinHandle<()>)`:
    ///
    /// * The sink Arc is cloned into every per-shard journal-loop's `Arc<dyn
    ///   PatchSink>` (one shared sink across N letters).
    /// * The `JoinHandle` is held by the caller (typically
    ///   `lib.rs::spawn_journal_loops_for_warm_shards` in commit A4) and
    ///   aborted during graceful shutdown.
    ///
    /// The applier holds [`Weak<IndexManager>`] so this constructor
    /// does NOT extend the daemon's lifetime — when all
    /// `Arc<IndexManager>` instances drop the applier exits cleanly
    /// via the `Weak::upgrade` `None` arm.
    ///
    /// # Backpressure
    ///
    /// The `apply_tx` mpsc channel is **unbounded by design**.  Three
    /// constraints pin this choice (Phase 10d audit):
    ///
    /// 1. **Producer is sync-non-blocking by contract.**  `accept` /
    ///    `trigger_save` / `journal_wrapped` are `fn`, not `async fn` — invoked
    ///    synchronously from the journal-loop `process_tick`. They cannot
    ///    `.await` on a bounded `send`, so a bounded variant would have to use
    ///    `try_send` + drop-on-full, which is operationally identical to the
    ///    existing "dead applier silently absorbed" degraded path (documented
    ///    on `apply_tx`).
    ///
    /// 2. **Producer cadence is throttled upstream by
    ///    [`crate::cache::journal_loop::SaveTrigger`].**  Save messages fire on
    ///    either the 50K-event threshold OR the 5-minute age threshold.
    ///    Worst-case steady-state ≈ 1 `ApplyMsg::Save` per drive per 5 min × 26
    ///    drives ≈ 5 messages/min.  Wrap messages are rare (NTFS USN journal
    ///    head reset only).
    ///
    /// 3. **Payload is bounded.**  Each `ApplyMsg::Save` carries the drained
    ///    per-letter `Vec<FileChange>` (capped at the 50K-event threshold; ~10
    ///    MB peak per save tick) and is consumed within ~1 s by the applier's
    ///    serial loop.  If the applier wedges, memory grows by ~10 MB per drive
    ///    per 5 min — a worst-case that implies the daemon itself is wedged
    ///    (the applier's blocking step is registry write-lock + body patch,
    ///    which is a daemon-wide hot path), so process restart resolves both.
    ///
    /// See `docs/dev/baseline/2026-05-19/phase_10_backpressure_audit.md`
    /// (local) for the full per-site verdict.
    pub(crate) fn spawn_with_applier(
        idx: &Arc<IndexManager>,
        cursor_store: Arc<dyn CursorStore>,
    ) -> (Arc<Self>, JoinHandle<()>) {
        let (apply_tx, apply_rx) = mpsc::unbounded_channel();
        let weak = Arc::downgrade(idx);
        let pending: PendingBuffers = Arc::default();
        // The cursor store lives on the applier task (it persists the
        // cursor in lockstep with a successful body save); the sink
        // itself only forwards the cursor inside `ApplyMsg::Save`.
        let handle = tokio::spawn(applier_task(
            apply_rx,
            weak,
            cursor_store,
            Arc::clone(&pending),
        ));
        (Arc::new(Self { apply_tx, pending }), handle)
    }

    /// Acquire the pending-buffer mutex, recovering from poison.
    /// Mirrors the `lock_journal_handles` helper in
    /// `index/journal.rs` — a poisoned mutex on the sink side
    /// would otherwise propagate a panic from the applier task into
    /// every subsequent journal-loop tick, killing the whole
    /// journal-refresh subsystem.
    fn lock_pending(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<uffs_mft::platform::DriveLetter, Vec<FileChange>>> {
        lock_buffers(&self.pending)
    }

    /// Test-only constructor: build a sink with an explicit
    /// `mpsc::UnboundedReceiver<ApplyMsg>` returned to the caller.
    /// Skips the applier-task spawn so unit tests can drain the
    /// channel directly and assert on per-`ApplyMsg`-variant
    /// payloads.  Production code must use
    /// [`Self::spawn_with_applier`].
    #[cfg(test)]
    fn new_for_test() -> (Arc<Self>, mpsc::UnboundedReceiver<ApplyMsg>) {
        let (apply_tx, apply_rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                apply_tx,
                pending: Arc::default(),
            }),
            apply_rx,
        )
    }
}

impl PatchSink for RegistryPatchSink {
    fn accept(&self, letter: uffs_mft::platform::DriveLetter, changes: &[FileChange]) -> bool {
        // Phase 8: append to the per-letter pending buffer instead
        // of putting traffic on the mpsc channel.  `trigger_save`
        // drains this buffer and forwards the changes into the
        // applier task for surgical-patch processing.
        tracing::trace!(
            target: "shard.journal",
            drive = %letter,
            change_count = changes.len(),
            "Journal accept (buffered for next save tick)",
        );
        let mut guard = self.lock_pending();
        guard.entry(letter).or_default().extend_from_slice(changes);
        drop(guard);
        true
    }

    fn has_pending(&self, letter: uffs_mft::platform::DriveLetter) -> bool {
        self.lock_pending()
            .get(&letter)
            .is_some_and(|buffered| !buffered.is_empty())
    }

    fn trigger_save(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: SaveReason,
        cursor: u64,
    ) {
        // Drain the per-letter buffer in one swoop so the applier
        // sees a snapshot of all events accumulated since the
        // previous save.  An empty drained Vec is forwarded as-is
        // (the applier short-circuits the surgical-patch path).
        let drained = {
            let mut guard = self.lock_pending();
            guard.remove(&letter).unwrap_or_default()
        };
        let _ignore = self.apply_tx.send(ApplyMsg::Save {
            letter,
            reason,
            changes: drained,
            cursor,
        });
    }

    fn trigger_apply(&self, letter: uffs_mft::platform::DriveLetter) {
        // Drain the per-letter buffer just like `trigger_save`, but
        // route it to the apply-only path: the body is patched +
        // swapped (search goes live) without the compact-cache disk
        // write or the cursor persist.  Whichever tick (apply or save)
        // fires drains the buffer; a save tick subsumes the apply, so
        // the loop never fires both on the same poll.
        let drained = {
            let mut guard = self.lock_pending();
            guard.remove(&letter).unwrap_or_default()
        };
        if drained.is_empty() {
            // Nothing accumulated since the last drain — no work.  (The
            // loop only calls this when its event-count says there
            // *should* be churn, so an empty drain here just means a save
            // tick beat us to it.)
            return;
        }
        let _ignore = self.apply_tx.send(ApplyMsg::Apply {
            letter,
            changes: drained,
        });
    }

    fn journal_wrapped(&self, letter: uffs_mft::platform::DriveLetter) {
        // Wrap means the journal head reset; any buffered events
        // are stale relative to the new cursor.  Discard the
        // pending buffer and rely on the applier's full reload to
        // resync the body.
        {
            let mut guard = self.lock_pending();
            let _discarded = guard.remove(&letter);
        }
        let _ignore = self.apply_tx.send(ApplyMsg::Wrap { letter });
    }
}

/// Async drain loop that owns the applier-task receiver.
///
/// Exits cleanly when:
///
/// 1. All [`RegistryPatchSink`] instances drop, closing the sender —
///    `recv().await` returns `None`.
/// 2. The [`IndexManager`] drops past its last `Arc` reference —
///    `Weak::upgrade()` returns `None`, the loop returns immediately on the
///    next message even if more are pending (correctness: the daemon is
///    shutting down, no point applying more refreshes).
async fn applier_task(
    mut rx: mpsc::UnboundedReceiver<ApplyMsg>,
    idx_weak: Weak<IndexManager>,
    cursor_store: Arc<dyn CursorStore>,
    pending: PendingBuffers,
) {
    // A message pulled off the channel during apply-coalescing that did not
    // belong to the merged run (different letter, or a Save/Wrap); processed
    // on the next iteration before blocking on `recv`.
    let mut carried: Option<ApplyMsg> = None;
    // One background save at a time across every drive (issue #639).
    let save_slot = Arc::new(SaveSlot::default());
    loop {
        let msg = match carried.take() {
            Some(held) => held,
            None => match rx.recv().await {
                Some(received) => received,
                None => break,
            },
        };
        let Some(idx_strong) = idx_weak.upgrade() else {
            tracing::debug!(
                target: "shard.journal",
                "IndexManager dropped; exiting applier task",
            );
            return;
        };
        // Coalesce a run of consecutive same-letter `Apply` messages into a
        // single body patch.  When apply ticks fire faster than the O(n)
        // rebuild drains them (e.g. a tiny `UFFS_USN_APPLY_INTERVAL_MS` on a
        // huge, busy drive), the channel backs up with many small applies;
        // merging collapses N rebuilds into one and bounds the apply rate to
        // the rebuild rate.  No change is dropped — FIFO order is preserved by
        // appending in receive order.  Save / Wrap are never merged (they
        // carry their own cursor / reload semantics) and fall through.
        let to_dispatch = if let ApplyMsg::Apply { letter, changes } = msg {
            let (merged, leftover) = coalesce_apply_run(letter, changes, &mut rx);
            carried = leftover;
            ApplyMsg::Apply {
                letter,
                changes: merged,
            }
        } else {
            msg
        };
        let ctx = DispatchCtx {
            cursor_store: &cursor_store,
            save_slot: &save_slot,
            pending: &pending,
        };
        dispatch_msg(&idx_strong, &ctx, to_dispatch).await;
    }
    tracing::debug!(
        target: "shard.journal",
        "Sink channel closed; applier task exiting",
    );
}

/// Greedily merge the run of consecutive same-`letter` [`ApplyMsg::Apply`]
/// messages currently queued behind a first apply into one change vector,
/// collapsing a backlog of apply ticks into a single body rebuild.
///
/// Drains via non-blocking [`mpsc::UnboundedReceiver::try_recv`], appending
/// each same-letter batch in receive order (FIFO preserved, so create →
/// delete → create-into-reused-FRS sequences still apply correctly).  Stops
/// at the first message that is **not** a same-letter `Apply` — a different
/// letter, or a `Save` / `Wrap` — and returns it as the "carried" leftover
/// for the caller to process next, so no message is reordered past a Save or
/// dropped.
fn coalesce_apply_run(
    letter: uffs_mft::platform::DriveLetter,
    mut changes: Vec<FileChange>,
    rx: &mut mpsc::UnboundedReceiver<ApplyMsg>,
) -> (Vec<FileChange>, Option<ApplyMsg>) {
    loop {
        match rx.try_recv() {
            Ok(ApplyMsg::Apply {
                letter: next_letter,
                changes: more,
            }) if next_letter == letter => changes.extend(more),
            Ok(other) => return (changes, Some(other)),
            Err(_) => return (changes, None),
        }
    }
}

/// Dispatch a single drained [`ApplyMsg`] to the appropriate
/// [`IndexManager`] entry point.
///
/// Extracted from [`applier_task`] so the parent stays under
/// clippy's strict-gate cognitive-complexity ceiling.  The split
/// also keeps the `Weak::upgrade` lifecycle decision in the parent
/// (visible at the recv-loop level) and the per-variant dispatch
/// in this helper (focused, easy to extend).
async fn dispatch_msg(idx: &Arc<IndexManager>, ctx: &DispatchCtx<'_>, msg: ApplyMsg) {
    match msg {
        ApplyMsg::Save {
            letter,
            reason,
            changes,
            cursor,
        } => {
            let reason_str = save_reason_str(reason);
            let Some(batch) = hold_unless_warm(idx, ctx.pending, letter, changes).await else {
                return;
            };
            // One save at a time: while the previous one is still
            // writing, apply this batch in memory (search stays live)
            // and leave the write to a later save tick.  The cursor is
            // not stored, so a cold start replays from the last body
            // that actually reached disk.
            let Some(permit) = ctx.save_slot.try_acquire() else {
                tracing::debug!(
                    target: "shard.journal",
                    drive = %letter,
                    reason = reason_str,
                    change_count = batch.len(),
                    "Save deferred: the previous save is still writing; applied in memory only",
                );
                let _applied = idx
                    .handle_journal_apply(letter, "save-deferred", batch)
                    .await;
                return;
            };
            // Patch the batch in (search goes live at once), then hand
            // the body to the background persist, which writes it and
            // only then stores the cursor — so the on-disk cursor never
            // outruns the on-disk body.  A parked or missing shard
            // yields `None`: nothing is written and the cursor stays
            // pinned to the last real body save, which the startup
            // warm-load guard relies on (`cache::guarded_load`).
            if let Some(body) = idx.handle_journal_save(letter, reason_str, batch).await {
                idx.spawn_journal_persist(
                    letter,
                    body,
                    cursor,
                    Arc::clone(ctx.cursor_store),
                    permit,
                );
            }
        }
        ApplyMsg::Apply { letter, changes } => {
            let Some(batch) = hold_unless_warm(idx, ctx.pending, letter, changes).await else {
                return;
            };
            // Apply tick: patch the body + swap it into the registry so
            // search goes live, but do NOT persist the compact cache or
            // advance the on-disk cursor.  Disk persistence + cursor
            // advance stay on the rarer `Save` tick; a cold start
            // re-replays the in-between deltas idempotently from the
            // last saved cursor.
            let _applied = idx.handle_journal_apply(letter, "apply-tick", batch).await;
        }
        ApplyMsg::Wrap { letter } => {
            // Wrap stays on the Phase-7 full-reload path.  The
            // patched-body snapshot is invalidated by the journal
            // head reset, so cloning + patching is wasted work — the
            // cleanest option is `load_drive_with_usn_refresh` which
            // re-reads the MFT and replays the new journal from
            // its current head.
            let _applied = idx.handle_journal_refresh(letter, "journal-wrapped").await;
        }
    }
}

/// What [`dispatch_msg`] needs beyond the message: the cursor store, the
/// single background-save slot, and the sink's pending buffers.
struct DispatchCtx<'ctx> {
    /// Persists the cursor after a body save reaches disk.
    cursor_store: &'ctx Arc<dyn CursorStore>,
    /// One background save at a time across every drive.
    save_slot: &'ctx Arc<SaveSlot>,
    /// Where a not-yet-warm drive's batch goes back to.
    pending: &'ctx PendingBuffers,
}

/// Pass `changes` through when `letter` is warm; otherwise put them back at
/// the front of its pending buffer and return `None`.
///
/// A batch for a drive that is not warm cannot be applied.  Dropping it was
/// safe only for the part the next promote's journal replay covers; the
/// changes that arrive between that replay's last read and the moment the
/// promoted body is installed were lost, and a file created in that window
/// stayed missing from search.  Holding the batch (newest
/// [`MAX_HELD_CHANGES`]) lets the first apply after the install pick it up;
/// replaying changes the promote already covered is harmless because
/// applying a change is idempotent and the batches keep their order.
async fn hold_unless_warm(
    idx: &Arc<IndexManager>,
    pending: &PendingBuffers,
    letter: uffs_mft::platform::DriveLetter,
    changes: Vec<FileChange>,
) -> Option<Vec<FileChange>> {
    if changes.is_empty() || idx.is_drive_warm(letter).await {
        return Some(changes);
    }
    requeue_front(pending, letter, changes);
    None
}

/// Put `older` back in front of whatever `letter` has buffered since, keeping
/// the newest [`MAX_HELD_CHANGES`].
fn requeue_front(
    pending: &PendingBuffers,
    letter: uffs_mft::platform::DriveLetter,
    mut older: Vec<FileChange>,
) {
    let mut guard = lock_buffers(pending);
    let slot = guard.entry(letter).or_default();
    older.append(slot);
    let excess = older.len().saturating_sub(MAX_HELD_CHANGES);
    if excess > 0 {
        drop(older.drain(..excess));
    }
    *slot = older;
    drop(guard);
}

/// Lock the pending buffers, recovering from poison (see
/// [`RegistryPatchSink::lock_pending`]).
fn lock_buffers(
    pending: &PendingBuffers,
) -> std::sync::MutexGuard<'_, HashMap<uffs_mft::platform::DriveLetter, Vec<FileChange>>> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Map a [`SaveReason`] to its stable diagnostic string for tracing
/// and the [`IndexManager::handle_journal_refresh`] success/failure
/// log.  Centralised here so log strings stay consistent across the
/// activation surface.
const fn save_reason_str(reason: SaveReason) -> &'static str {
    match reason {
        SaveReason::EventsExceeded => "events-exceeded",
        SaveReason::AgeElapsed => "age-elapsed",
    }
}

#[cfg(test)]
mod tests;
