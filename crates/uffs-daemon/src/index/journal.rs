// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Per-shard USN journal refresh path (Phase 7 activation, Phase 8
//! surgical-patch).
//!
//! Two entry points feed the per-shard journal loop's
//! [`crate::cache::journal_sink::RegistryPatchSink`] applier task:
//!
//! * [`IndexManager::handle_journal_save`] (Phase 8) — clones the warm
//!   `DriveCompactIndex` body, applies the buffered
//!   [`uffs_mft::usn::FileChange`] batch via
//!   [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`], swaps the
//!   new Arc into the registry, and returns the body for the background persist
//!   in [`super::journal_save`], which writes it and only then advances the
//!   on-disk cursor.
//!
//! * [`IndexManager::handle_journal_refresh`] (Phase 7) — full per-shard body
//!   refresh via [`uffs_core::compact_loader::load_drive_with_usn_refresh`]
//!   (MFT read + USN replay + compact rebuild).  Heavy (~2-7 s per drive) but
//!   correct against journal-wrap events where the buffered batch is stale
//!   relative to the new cursor.
//!
//! The split lets save triggers (events-exceeded / age-elapsed) take
//! the cheap surgical path while wrap triggers fall back to the full
//! reload — exactly the cadence the Phase-7 activation introduced,
//! now with a fast save path.
//!
//! ## Mac/Linux behaviour
//!
//! The underlying [`uffs_core::compact_loader::load_drive_with_usn_refresh`]
//! errors out by design on non-Windows targets (USN journals are
//! NTFS-only).  This method preserves that contract: the err arm
//! warn-logs the per-drive failure and returns `false` so the caller
//! can record the no-op without aborting the loop.  The surgical
//! [`IndexManager::handle_journal_save`] path is platform-agnostic
//! (it operates over the in-memory body + a `Vec<FileChange>` DTO)
//! and is exercised on Mac via synthesised `FileChange` inputs in
//! the per-method unit tests.  In production the Mac/Linux journal
//! loop wires [`MacStubJournalSource`] (always empty) so the
//! threshold-driven save trigger never fires anyway — the err arm
//! is defensive, not a hot path.
//!
//! [`MacStubJournalSource`]: crate::cache::journal_loop::sources::MacStubJournalSource

use alloc::sync::Arc;

use uffs_mft::usn::FileChange;

use super::IndexManager;
use crate::cache::journal_loop::JournalLoopHandle;

/// Helper for [`IndexManager::attach_journal_handle`]: lock the
/// journal-handle map, recovering from poison.  Mirrors the existing
/// `lock_in_flight_promotes` / `verify_shutdown_nonce` poison
/// handling pattern in this crate (per the `in_flight_promotes`
/// field doc rationale).
fn lock_journal_handles(
    map: &std::sync::Mutex<
        std::collections::HashMap<uffs_mft::platform::DriveLetter, JournalLoopHandle>,
    >,
) -> std::sync::MutexGuard<
    '_,
    std::collections::HashMap<uffs_mft::platform::DriveLetter, JournalLoopHandle>,
> {
    map.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl IndexManager {
    /// Drive a full per-shard USN refresh + body swap for `letter`.
    ///
    /// Called by the [`crate::cache::journal_sink::RegistryPatchSink`]
    /// applier task on every `Save` / `Wrap` message from the
    /// per-shard journal loop.  `reason` is a diagnostic string
    /// (`"events-exceeded"`, `"age-elapsed"`, `"journal-wrapped"`)
    /// surfaced in the success/failure log so operators can grep
    /// per-drive refresh churn by trigger type.
    ///
    /// ## Returns
    ///
    /// * `true` when the body was successfully Arc-swapped into the registry —
    ///   the call site MAY log a per-drive success event but the success log is
    ///   already emitted here at info-level.
    ///
    /// * `false` on every failure mode:
    ///
    ///   - Underlying
    ///     [`uffs_core::compact_loader::load_drive_with_usn_refresh`] err
    ///     (Windows: live MFT read failed, USN journal apply failed, volume
    ///     revoked; Mac/Linux: always — USN journals are NTFS-only by design).
    ///   - The blocking task aborted before producing a result.
    ///   - Registry race — the shard demoted to `Parked` / `Cold` between the
    ///     per-shard event-threshold trigger and this swap.  The next promote
    ///     on this letter will pick up the fresher snapshot via
    ///     [`crate::cache::body_loader::DiskBodyLoader`].
    ///
    /// `false` is **not** an error to propagate — the caller logs
    /// at warn/debug level depending on whether the failure was a
    /// real I/O error or just a benign demote race.
    pub(crate) async fn handle_journal_refresh(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: &str,
    ) -> bool {
        // Heavy work: live MFT read + USN replay + compact rebuild.
        // Runs on the blocking pool so the runtime's worker threads
        // stay free to service search RPCs concurrent with the refresh.
        // The closure enters [`BackgroundIoScope`] so the per-letter
        // syscalls run at Windows `THREAD_MODE_BACKGROUND_BEGIN`
        // priority — yielding to any foreground RPC handler under
        // disk contention.  RAII via `_bg_scope` ensures the matching
        // `_END` fires even if the underlying loader panics.  No-op
        // on Mac/Linux.  Mirrors the deleted Phase-5
        // `refresh_usn_for_warm_shards` per-letter wrapping.
        let background_io = Arc::clone(&self.background_io);
        let body_result = tokio::task::spawn_blocking(move || {
            let _bg_scope = crate::cache::background_io::BackgroundIoScope::enter(background_io);
            uffs_core::compact_loader::load_drive_with_usn_refresh(letter)
        })
        .await;

        let Some(body) = unwrap_refreshed_body(body_result, letter, reason) else {
            return false;
        };
        self.apply_journal_body(letter, reason, body).await
    }

    /// Phase 8 surgical-patch path for a save tick: patch the drained
    /// per-letter [`FileChange`] batch into the Warm body (as an apply tick
    /// does, so search goes live at once) and return the body that must be
    /// persisted.
    ///
    /// Called by the [`crate::cache::journal_sink::RegistryPatchSink`]
    /// applier task on every `Save` message (events-exceeded /
    /// age-elapsed) — the *fast* counterpart to
    /// [`IndexManager::handle_journal_refresh`] (which the applier still
    /// uses for `Wrap` messages where the cursor reset invalidates
    /// the buffered batch).  The heavy part — folding the delta and
    /// writing the file — is the caller's background persist
    /// ([`super::journal_save`]), off the applier task.
    ///
    /// An **empty** batch still returns the live body.  Apply ticks drain
    /// the same buffer, so by the time a save tick fires every change may
    /// already be applied in memory but not yet on disk; skipping the write
    /// there while the caller advanced the cursor would leave the cursor
    /// ahead of the file, and a cold start would never replay those changes.
    ///
    /// ## Returns
    ///
    /// * `Some(body)` — the body to persist, already serving queries.
    /// * `None` on every failure mode:
    ///   - the shard for `letter` is not registered, or not warm;
    ///   - the patch task aborted (panic, runtime shutdown);
    ///   - the shard demoted to `Parked` / `Cold` between the trigger and the
    ///     patch (`apply_usn_patch_to_body` returns `None`); the next promote
    ///     re-reads the cached body which a previous save already wrote, so the
    ///     per-shard drift is bounded by the per-shard save cadence;
    ///   - the body swap lost a separate demote race (`apply_journal_body`
    ///     returns `false`).
    pub(crate) async fn handle_journal_save(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: &str,
        changes: Vec<FileChange>,
    ) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
        if changes.is_empty() {
            return self.snapshot_shard_for_letter(letter).await?.body();
        }
        match self.apply_to_body(letter, reason, changes).await {
            BodyApplyOutcome::Applied(new_body) => Some(new_body),
            BodyApplyOutcome::NoOp | BodyApplyOutcome::Failed => None,
        }
    }

    /// Apply-tick sibling of [`IndexManager::handle_journal_save`]:
    /// patch the in-memory body and Arc-swap it into the registry,
    /// **without** the compact-cache disk write.
    ///
    /// The per-shard journal loop fires this on the short apply cadence
    /// (default ~2 s, [`crate::cache::journal_loop`]) so newly created /
    /// renamed / deleted files become searchable within a couple of
    /// seconds, while the heavy disk persistence stays on the rare
    /// `handle_journal_save` cadence (50k events / 5 min).  The apply
    /// path deliberately does **not** persist the journal cursor: only a
    /// real body save advances the on-disk cursor, so a cold start
    /// re-replays from the last saved cursor and the idempotent patcher
    /// re-applies the in-between deltas — identical to the pre-split
    /// cold-boot window.
    ///
    /// Returns `true` when the body was patched + swapped (or the batch
    /// was empty — a no-op), `false` only on a hard failure (shard not
    /// registered / not warm, patch task aborted, or the swap lost a
    /// demote race).  See [`IndexManager::handle_journal_save`] for the
    /// per-failure breakdown.
    pub(crate) async fn handle_journal_apply(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: &str,
        changes: Vec<FileChange>,
    ) -> bool {
        // Apply-only: keep the cheap delta overlay — nothing is
        // serialised on this tick, and folding every ~2 s apply would
        // be an O(total-records) rebuild per tick.
        !matches!(
            self.apply_to_body(letter, reason, changes).await,
            BodyApplyOutcome::Failed
        )
    }

    /// Shared core of [`IndexManager::handle_journal_save`] and
    /// [`IndexManager::handle_journal_apply`]: clone the warm body,
    /// apply the buffered batch, and Arc-swap the result into the
    /// registry.  Returns the patched body on success so the save-tick
    /// caller can hand it to the background disk-save task; the
    /// apply-tick caller discards it.  Stops short of any disk I/O —
    /// disk persistence is the save tick's responsibility alone.
    async fn apply_to_body(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: &str,
        changes: Vec<FileChange>,
    ) -> BodyApplyOutcome {
        if changes.is_empty() {
            log_save_empty_batch(letter, reason);
            return BodyApplyOutcome::NoOp;
        }

        let change_count = changes.len();
        let Some(shard) = self.snapshot_shard_for_letter(letter).await else {
            log_save_no_shard(letter, reason, change_count);
            return BodyApplyOutcome::Failed;
        };

        let (new_body, stats) = match self
            .run_surgical_patch_task(&shard, letter, reason, changes)
            .await
        {
            PatchTaskOutcome::Applied(body, stats) => (body, stats),
            PatchTaskOutcome::ShardNotWarm => {
                log_save_shard_demoted(letter, reason, change_count);
                return BodyApplyOutcome::Failed;
            }
            PatchTaskOutcome::TaskAborted => return BodyApplyOutcome::Failed,
        };

        log_save_patch_applied(letter, reason, change_count, &stats);

        if !self
            .apply_journal_body(letter, reason, Arc::clone(&new_body))
            .await
        {
            return BodyApplyOutcome::Failed;
        }

        BodyApplyOutcome::Applied(new_body)
    }

    /// Snapshot the shard for `letter` from the registry read-lock,
    /// cloning the `Arc<ShardEntry>` so subsequent registry
    /// mutations (demote, promote, `replace_warm_body`) leave this
    /// snapshot observing the pre-tick state.
    async fn snapshot_shard_for_letter(
        &self,
        letter: uffs_mft::platform::DriveLetter,
    ) -> Option<Arc<crate::cache::shard::ShardEntry>> {
        let guard = self.index.read().await;
        guard.iter().find(|entry| entry.drive == letter).cloned()
    }

    /// Run the surgical-patch heavy work on the blocking pool.
    ///
    /// Deep-clones the body (~100 ms `ColumnStorage` promote) +
    /// applies the patch + rebuilds children CSR + trigram +
    /// `ext_index` (~600 ms total on a 7M-record drive).  Wraps the
    /// closure in [`crate::cache::background_io::BackgroundIoScope`]
    /// so any syscall-bound work (mmap promote on Windows) yields
    /// to foreground search RPCs.  Drains the resulting
    /// [`tokio::task::JoinHandle`] through [`classify_patch_result`]
    /// so callers see the three explicit outcomes
    /// (`Applied` / `ShardNotWarm` / `TaskAborted`) without
    /// `Option<Option<_>>` smell.
    async fn run_surgical_patch_task(
        &self,
        shard: &Arc<crate::cache::shard::ShardEntry>,
        letter: uffs_mft::platform::DriveLetter,
        reason: &str,
        changes: Vec<FileChange>,
    ) -> PatchTaskOutcome {
        let background_io = Arc::clone(&self.background_io);
        let shard_for_patch = Arc::clone(shard);
        let change_count = changes.len();
        let patch_result = tokio::task::spawn_blocking(move || {
            let _bg_scope = crate::cache::background_io::BackgroundIoScope::enter(background_io);
            shard_for_patch.apply_usn_patch_to_body(&changes)
        })
        .await;
        classify_patch_result(patch_result, letter, reason, change_count)
    }

    /// Phase 7 activation: store a [`JournalLoopHandle`] for `letter`
    /// in the per-letter handle map.
    ///
    /// Called once per loaded drive from
    /// `lib.rs::spawn_journal_loops_for_warm_shards` after each
    /// [`crate::cache::journal_loop::spawn_journal_loop`] call.  If
    /// a handle is already present for `letter` (defensive — never
    /// happens in the production flow because each drive is loaded
    /// exactly once before this method fires), the previous handle
    /// is **cancelled** so the orphaned loop tears down cleanly.
    pub(crate) fn attach_journal_handle(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        handle: JournalLoopHandle,
    ) {
        let mut guard = lock_journal_handles(&self.journal_handles);
        if let Some(previous) = guard.insert(letter, handle) {
            tracing::warn!(
                target: "shard.journal",
                drive = %letter,
                "Replacing existing journal-loop handle (defensive cancel of previous loop)",
            );
            // Best-effort cancel: send the watch signal and forget
            // the JoinHandle.  The previous loop converges on its
            // next tick (~500 ms) and is reaped by the runtime.
            let _join = previous.cancel();
        }
    }

    /// Per-letter write-lock swap of a freshly-loaded body Arc.
    ///
    /// Extracted from [`IndexManager::handle_journal_refresh`] so the parent
    /// stays under clippy's strict-gate cognitive-complexity ceiling.
    /// `replace_warm_body` returns `None` when the shard demoted to
    /// `Parked` / `Cold` between the threshold trigger and this swap
    /// — a benign race that we log at debug-level and absorb.
    async fn apply_journal_body(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: &str,
        body: Arc<uffs_core::compact::DriveCompactIndex>,
    ) -> bool {
        let mut guard = self.index.write().await;
        let Some(new_registry) = guard.replace_warm_body(letter, body) else {
            tracing::debug!(
                target: "shard.journal",
                drive = %letter,
                reason,
                "Shard demoted between trigger and swap; no-op",
            );
            return false;
        };
        *guard = Arc::new(new_registry);
        drop(guard);
        self.bump_index_version();
        // Per-tick housekeeping, not a tier change: every 2-3 s per
        // warm drive on a busy box (159 lines in one 2026-10-03
        // session).  Operators follow transitions at INFO; the tick
        // itself is a DEBUG event.
        tracing::debug!(
            target: "shard.journal",
            drive = %letter,
            reason,
            "USN refresh applied",
        );
        true
    }
}

/// Log the empty-batch fast path of [`IndexManager::handle_journal_save`].
///
/// Age-elapsed triggers on quiet drives produce empty change
/// batches; the surgical patch short-circuits to a debug-log no-op
/// so the journal loop's cursor still advances and the per-shard
/// save trigger resets.
fn log_save_empty_batch(letter: uffs_mft::platform::DriveLetter, reason: &str) {
    tracing::debug!(
        target: "shard.journal",
        drive = %letter,
        reason,
        "Surgical patch skipped (empty change batch)",
    );
}

/// Log the "no shard registered for letter" arm of
/// [`IndexManager::handle_journal_save`].
///
/// Defensive: in production every drive that emits journal events
/// has been registered via `add_drive` before its journal loop
/// starts.  Reaching this arm indicates a startup-ordering bug or
/// an out-of-order drive eject, both worth surfacing.
fn log_save_no_shard(letter: uffs_mft::platform::DriveLetter, reason: &str, change_count: usize) {
    tracing::debug!(
        target: "shard.journal",
        drive = %letter,
        reason,
        change_count,
        "No shard registered for letter; dropping changes",
    );
}

/// Log the "shard demoted between trigger and patch" arm of
/// [`IndexManager::handle_journal_save`].
///
/// Most likely cause: a Parked / Cold demote raced ahead of the
/// surgical-patch blocking task.  The next promote re-reads the
/// on-disk body which a previous patch tick already updated, so
/// the per-shard drift is bounded by the per-shard save cadence.
fn log_save_shard_demoted(
    letter: uffs_mft::platform::DriveLetter,
    reason: &str,
    change_count: usize,
) {
    tracing::debug!(
        target: "shard.journal",
        drive = %letter,
        reason,
        change_count,
        "Shard demoted between trigger and patch; dropping changes",
    );
}

/// Log the success path of
/// [`IndexManager::handle_journal_save`] with per-variant patch
/// stats so operators can grep for the surgical-patch tick rate
/// and the create / delete / rename / skip mix per drive.
fn log_save_patch_applied(
    letter: uffs_mft::platform::DriveLetter,
    reason: &str,
    change_count: usize,
    stats: &uffs_core::compact_loader::PatchStats,
) {
    tracing::debug!(
        target: "shard.journal",
        drive = %letter,
        reason,
        change_count,
        applied_create = stats.created,
        applied_delete = stats.deleted,
        applied_rename = stats.renamed,
        applied_skip = stats.skipped,
        "Surgical patch applied",
    );
}

/// Outcome of [`IndexManager::apply_to_body`] — the shared body-patch
/// core behind the save tick and the apply tick.  Carries the patched
/// body on success so the save-tick caller can persist it while the
/// apply-tick caller drops it; keeps the empty-batch no-op distinct
/// from a hard failure so each caller maps it to the right `bool`.
enum BodyApplyOutcome {
    /// Body cloned, patched, and Arc-swapped into the registry.  The
    /// save tick hands the inner Arc to the background disk-save task;
    /// the apply tick discards it (in-memory swap is all it owes).
    Applied(Arc<uffs_core::compact::DriveCompactIndex>),
    /// The batch was empty — nothing to apply.  Both ticks treat this
    /// as success (a save with no churn is a no-op, not a failure).
    NoOp,
    /// A hard failure: shard not registered / not warm, the patch task
    /// aborted, or the swap lost a demote race.  Already logged at the
    /// failure site; both ticks surface it as `false`.
    Failed,
}

/// Three-way classification of the surgical-patch blocking task's
/// `JoinHandle` result — lets [`IndexManager::apply_to_body`]
/// match each outcome with its own diagnostic + control-flow
/// without resorting to the `Option<Option<T>>` shape `clippy`
/// (rightly) flags as a smell.
enum PatchTaskOutcome {
    /// The task ran to completion and the shard's
    /// [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`] returned
    /// a fresh body Arc + per-batch stats.  Caller swaps the body
    /// into the registry + spawns a background cache save.
    Applied(
        Arc<uffs_core::compact::DriveCompactIndex>,
        uffs_core::compact_loader::PatchStats,
    ),
    /// The task ran to completion but the shard wasn't `Warm` /
    /// `Hot` at the moment the patch fired — most likely a demote
    /// race between the journal trigger and the blocking task
    /// scheduling.  Caller drops the changes; the next promote
    /// re-reads the on-disk body which a previous patch tick has
    /// already saved.
    ShardNotWarm,
    /// The blocking task itself aborted before producing a result
    /// (panic, runtime shutdown).  The classifier already
    /// warn-logged the [`tokio::task::JoinError`] with full
    /// context; caller short-circuits the body swap.
    TaskAborted,
}

/// Classify the surgical-patch task's `JoinHandle` result into a
/// [`PatchTaskOutcome`].
///
/// On `Err(join_err)` the helper emits a `shard.journal` warn-log
/// (the same shape `handle_journal_refresh`'s
/// [`unwrap_refreshed_body`] uses for its task-abort arm) and returns
/// [`PatchTaskOutcome::TaskAborted`] so the caller doesn't re-log.
fn classify_patch_result(
    patch_result: Result<
        Option<(
            Arc<uffs_core::compact::DriveCompactIndex>,
            uffs_core::compact_loader::PatchStats,
        )>,
        tokio::task::JoinError,
    >,
    letter: uffs_mft::platform::DriveLetter,
    reason: &str,
    change_count: usize,
) -> PatchTaskOutcome {
    match patch_result {
        Ok(Some((body, stats))) => PatchTaskOutcome::Applied(body, stats),
        Ok(None) => PatchTaskOutcome::ShardNotWarm,
        Err(join_err) => {
            tracing::warn!(
                target: "shard.journal",
                drive = %letter,
                reason,
                change_count,
                error = %join_err,
                "Surgical patch blocking task aborted; shard kept previous body",
            );
            PatchTaskOutcome::TaskAborted
        }
    }
}

/// Drain a `tokio::task::JoinHandle` result into either the freshly-
/// loaded body Arc or `None` (with the per-failure warn-log already
/// emitted).
///
/// Extracted from [`IndexManager::handle_journal_refresh`] so the
/// parent stays under clippy's strict-gate cognitive-complexity
/// ceiling.  Two failure modes share the `None` return:
///
/// * `Ok(Err(err))` — the underlying
///   [`uffs_core::compact_loader::load_drive_with_usn_refresh`] surfaced an
///   error (Windows: live MFT read / USN apply failed; Mac/Linux: always — the
///   helper's `cfg(not(windows))` arm bails).
/// * `Err(join_err)` — the blocking task itself aborted before producing a
///   result (panic, runtime shutdown).
fn unwrap_refreshed_body(
    body_result: Result<
        anyhow::Result<(
            uffs_core::compact::DriveCompactIndex,
            uffs_core::compact_loader::RefreshTiming,
        )>,
        tokio::task::JoinError,
    >,
    letter: uffs_mft::platform::DriveLetter,
    reason: &str,
) -> Option<Arc<uffs_core::compact::DriveCompactIndex>> {
    match body_result {
        Ok(Ok((body, _timing))) => Some(Arc::new(body)),
        Ok(Err(err)) => {
            tracing::warn!(
                target: "shard.journal",
                drive = %letter,
                reason,
                error = %err,
                "USN refresh failed; shard kept previous body",
            );
            None
        }
        Err(join_err) => {
            tracing::warn!(
                target: "shard.journal",
                drive = %letter,
                reason,
                error = %join_err,
                "USN refresh blocking task aborted; shard kept previous body",
            );
            None
        }
    }
}
