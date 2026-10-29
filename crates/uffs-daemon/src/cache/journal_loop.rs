// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Per-shard USN journal polling loop (Phase 7 tasks 7.2 + 7.3).
//!
//! ## Architecture
//!
//! Each loaded shard owns one `tokio::task` polling its drive's USN
//! journal at [`JournalLoopConfig::poll_interval`] cadence (default
//! 500 ms, overridable via `UFFS_USN_POLL_INTERVAL_MS` for tests
//! and benchmarks).  Per tick:
//!
//! 1. **Poll the journal** via the trait-object [`JournalSource`]. Returns
//!    `(changes, next_cursor, journal_id)`.  Empty changes is the no-op fast
//!    path — no patch, no swap, just advance the cursor (still useful as a
//!    journal-liveness signal so wrap detection in Phase 7 task 7.7 has fresh
//!    data).
//! 2. **Apply to the shard** via the caller-supplied [`PatchSink`].  Production
//!    wires this to a closure that calls
//!    [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`] +
//!    [`crate::cache::ShardRegistry::replace_warm_body`]; tests wire it to a
//!    recording fake.
//! 3. **Update cursor** so the next poll picks up only what's new.
//! 4. **Sleep** until the next deadline, racing the cancellation receiver so
//!    shutdown takes effect within one poll-interval.
//!
//! ## Mac vs Windows split
//!
//! The trait is platform-agnostic; the **journal-source impl** is
//! Windows-only (`WindowsJournalSource` wraps `read_usn_journal`).
//! On macOS / Linux the production wire-up uses
//! [`crate::cache::journal_loop::sources::MacStubJournalSource`] which always
//! returns empty changes — the loop ticks at the configured cadence but
//! produces no patches. State-machine semantics (cancellation, cursor advance,
//! no-op ticks) are exercised end-to-end on Mac via `tests::FakeJournalSource`.
//!
//! ## Phase 7 commit boundary
//!
//! This module lands the per-shard infrastructure (tasks 7.2 / 7.3 /
//! 7.4 / 7.6 / 7.7).  Production spawning from `lib.rs::spawn_load_task`
//! waits for the activation commit; until then the existing Phase-5
//! 5-min global tick (`refresh_usn_for_warm_shards`) is the live path.

use alloc::sync::Arc;
use core::time::Duration;

use tokio::sync::watch;
use uffs_mft::usn::FileChange;

mod poll;
mod triggers;

pub(crate) use poll::{MAX_POLL_BACKOFF, PollBackoff};
pub(crate) use triggers::{
    ApplyTrigger, DEFAULT_APPLY_DEBOUNCE_MS, DEFAULT_APPLY_INTERVAL_MS, DEFAULT_SAVE_AGE_CEILING,
    DEFAULT_SAVE_AGE_MIN_EVENTS, DEFAULT_SAVE_MIN_INTERVAL, DEFAULT_SAVE_THRESHOLD_AGE,
    DEFAULT_SAVE_THRESHOLD_EVENTS, SavePolicy, SaveReason, SaveTrigger,
};

/// Default poll interval for the per-shard journal loop (500 ms).
///
/// Overridable at runtime via the `UFFS_USN_POLL_INTERVAL_MS`
/// environment variable; the env-var path lets benchmarks and
/// long-running soak tests slow the tick down to reduce log noise
/// without recompiling.
pub(crate) const DEFAULT_POLL_INTERVAL_MS: u64 = 500;

/// Result of one [`JournalSource::poll`] call.
///
/// Carries the change batch, the new cursor for the next call, and
/// the journal identifier (Windows-side: the USN-journal `JournalID`
/// from `FSCTL_QUERY_USN_JOURNAL`).  The `journal_id` is consumed by
/// Phase 7 task 7.7 (wrap detection): if it changes between two
/// successive polls the journal was recreated and the in-memory body
/// must be force-rebuilt instead of incrementally patched.
#[derive(Debug, Clone, Default)]
pub(crate) struct JournalPollResult {
    /// Aggregated per-file changes since the previous cursor.
    /// Empty on no-op ticks (no journal activity in the interval).
    pub(crate) changes: Vec<FileChange>,
    /// Cursor to pass into the next [`JournalSource::poll`] call.
    /// Advances monotonically except across a journal-wrap — see
    /// `journal_id` for the wrap-detection signal.
    pub(crate) next_cursor: u64,
    /// Journal identifier.  Compared against the previous tick's
    /// value to detect journal-wrap (Phase 7 task 7.7); changed
    /// → force a full rebuild on the next promote.
    pub(crate) journal_id: u64,
}

/// Pluggable USN-journal data source.
///
/// Production wires
/// [`crate::cache::journal_loop::sources::MacStubJournalSource`] (always-empty)
/// on macOS / Linux and `WindowsJournalSource` (cfg(windows), reads via
/// `FSCTL_READ_USN_JOURNAL`) on Windows.  Tests wire
/// `tests::FakeJournalSource` (programmable event queue) to drive
/// the [`JournalLoop`] state machine deterministically without a
/// live MFT.
///
/// The trait is **synchronous** to match the established
/// `BackgroundIoPriority` / `BodyLoader` / `Prefetch` / `PressureSignal`
/// pattern across the cache module; the loop wraps every call in
/// [`tokio::task::spawn_blocking`] so the main runtime thread never
/// blocks on a kernel-mode journal read.
pub(crate) trait JournalSource: Send + Sync + 'static {
    /// Poll the journal for changes since `cursor`.
    ///
    /// **Returns** `Ok(JournalPollResult)` on success, including
    /// the empty-changes case (which is **not** an error — it just
    /// means nothing happened in the interval).
    ///
    /// **Returns** `Err(io::Error)` only when the underlying journal
    /// surface itself fails (e.g. the volume handle was revoked,
    /// the journal was deleted, the broker dropped the request).
    /// The loop logs the error at warn-level and retries on the
    /// next tick — a single failure does not abort the loop.
    ///
    /// # Errors
    ///
    /// Surfaces any platform-level failure reading the journal.
    fn poll(&self, cursor: u64) -> std::io::Result<JournalPollResult>;
}

/// Sink that consumes change batches produced by a [`JournalLoop`].
///
/// Production wires this to a closure that:
///
/// 1. Looks up the shard for `letter` in the registry.
/// 2. Calls [`crate::cache::shard::ShardEntry::apply_usn_patch_to_body`] on the
///    current body.
/// 3. Swaps the new body via
///    [`crate::cache::ShardRegistry::replace_warm_body`].
///
/// Tests wire it to a `Mutex<Vec<(uffs_mft::platform::DriveLetter,
/// Vec<FileChange>)>>` recorder so the test can assert which letters saw which
/// changes without touching a real registry.
///
/// The sink is **synchronous** (no `async fn`) for consistency with
/// the rest of the cache traits and so the loop body stays
/// transparent to platform allocators.  Mutation behind the trait
/// (registry write-lock acquire) is the implementor's concern.
pub(crate) trait PatchSink: Send + Sync + 'static {
    /// Apply `changes` for the shard identified by `letter`.
    ///
    /// **Returns** `true` if the sink accepted the batch (Warm /
    /// Hot shard, body present, swap succeeded), `false` if the
    /// caller should treat the tick as a no-op (Parked / Cold
    /// shard, registry race, etc.).  The boolean is purely
    /// informational — the loop continues in either case.
    fn accept(&self, letter: uffs_mft::platform::DriveLetter, changes: &[FileChange]) -> bool;

    /// Trigger a background compact-cache save for `letter`.
    ///
    /// Phase 7 task 7.4 — the loop calls this when the
    /// per-shard [`SaveTrigger`] decides the in-memory body has
    /// drifted far enough from the on-disk snapshot that a
    /// persist is warranted.  Production wires this to
    /// [`uffs_core::compact_cache::save_compact_cache_background`];
    /// tests record the call for assertion.
    ///
    /// The trigger is **fire-and-forget**.  The save runs on a
    /// background thread; the loop does not wait for it.  If
    /// the save fails, the implementor logs but does not
    /// propagate — the next threshold crossing will retry.
    ///
    /// `cursor` is the loop's current read position.  The sink
    /// persists it **in lockstep** with the on-disk compact-cache
    /// body — and only when that body save actually succeeds — so
    /// the persisted cursor never outruns the persisted body (a
    /// parked shard's save is a no-op, so its cursor must not
    /// advance on disk; see `journal_sink`).
    fn trigger_save(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        reason: SaveReason,
        cursor: u64,
    );

    /// Patch the in-memory body for `letter` on the apply cadence — the
    /// near-live sibling of [`PatchSink::trigger_save`].
    ///
    /// The loop calls this (via the per-shard [`ApplyTrigger`]) when
    /// buffered changes exist and at least
    /// [`JournalLoopConfig::apply_interval`] has elapsed since the last
    /// apply / save.  Production drains the same pending buffer
    /// `trigger_save` uses and runs the surgical patch + body swap so
    /// search sees the change — promptly after a quiet period (the
    /// interval is already satisfied), or within one apply interval under
    /// sustained churn — but **skips** the compact-cache disk write and
    /// the cursor persist, which stay the rarer `trigger_save` tick's job.
    ///
    /// Unlike `trigger_save` this takes **no cursor**: the on-disk
    /// cursor must only advance in lockstep with a real on-disk body
    /// save, so the apply tick deliberately leaves it pinned.  A cold
    /// start re-replays the apply-only deltas from the last saved
    /// cursor; the body patcher is idempotent on duplicate records, so
    /// the re-replay is a no-op against the freshly loaded body.
    ///
    /// Fire-and-forget, same as `trigger_save`.
    fn trigger_apply(&self, letter: uffs_mft::platform::DriveLetter);

    /// Whether changes for `letter` are buffered and not yet applied.
    ///
    /// The production sink puts a batch back when its drive is not warm
    /// yet; reporting it here keeps the loop's apply cadence armed, so the
    /// held changes are applied as soon as the drive is, even when no new
    /// change arrives.  Sinks that never hold anything keep the default.
    fn has_pending(&self, _letter: uffs_mft::platform::DriveLetter) -> bool {
        false
    }

    /// Notify the sink that the USN journal for `letter` was
    /// detected to have wrapped (Phase 7 task 7.7).
    ///
    /// A wrap is detected when [`JournalPollResult::journal_id`]
    /// changes between two successive non-zero-id polls — the
    /// journal was deleted + recreated (Windows admins can do this,
    /// or the volume can run out of journal space and rotate the
    /// `$UsnJrnl`).  Incremental patches don't apply across a wrap
    /// because the FRS → `compact_idx` mapping is now stale, so the
    /// production sink must force-rebuild the body on the next
    /// promote (typically by evicting the shard back to Cold and
    /// letting the standard cold-load path re-read the MFT).
    ///
    /// The loop resets its cursor to 0 after this call, so the
    /// next poll starts from the new journal's head.  No patches
    /// are applied for the wrap-tick — the sink's `accept` is
    /// **not** called.
    fn journal_wrapped(&self, letter: uffs_mft::platform::DriveLetter);
}

/// Pluggable cursor-persistence surface (Phase 7 task 7.6).
///
/// Production wires [`crate::cache::journal_loop::sources::NullCursorStore`]
/// (always-empty) as a fallback on platforms without a real persisted cursor
/// and the disk-backed implementation lands in the activation commit.
/// Tests wire `tests::FakeCursorStore` (in-memory `HashMap`)
/// to drive the load / store path deterministically.
///
/// Both methods are **infallible** at the trait level: any
/// underlying I/O failure must be logged and absorbed by the
/// implementor (cursor persistence is best-effort — a missed
/// store just means the next cold-boot re-replays a few extra
/// seconds of journal entries, which is correct since the body
/// patcher is idempotent on duplicate change records).
pub(crate) trait CursorStore: Send + Sync + 'static {
    /// Load the persisted cursor for `letter`.  Returns 0 (the
    /// "start from journal head" sentinel) when no cursor has
    /// been persisted yet or when the load failed.
    fn load(&self, letter: uffs_mft::platform::DriveLetter) -> u64;

    /// Persist `cursor` for `letter`.  Best-effort — the
    /// implementor logs failures but does not propagate.  The
    /// loop calls this every time it fires a save trigger so
    /// the on-disk cursor advances in lockstep with the on-disk
    /// snapshot.
    fn store(&self, letter: uffs_mft::platform::DriveLetter, cursor: u64);
}

/// Configuration for a single [`JournalLoop`] task.
///
/// Carries the tuning knobs the production loop reads from env
/// vars and the test loop sets directly.  Keeping these in one
/// place lets future tasks extend the config without churning
/// the loop signature.
#[derive(Debug, Clone)]
pub(crate) struct JournalLoopConfig {
    /// Cadence between successive polls.  Default 500 ms.
    pub(crate) poll_interval: Duration,
    /// Fallback cursor used when the [`CursorStore`] returns 0
    /// (no persisted cursor for this letter yet).  Tests use 0
    /// as a clean-slate baseline; production keeps it 0 because
    /// real cursor seeding flows through the cursor store.
    pub(crate) initial_cursor: u64,
    /// Events-since-last-save ceiling (Phase 7 task 7.4).
    /// Crossing this triggers a [`SaveReason::EventsExceeded`]
    /// save.  Default [`DEFAULT_SAVE_THRESHOLD_EVENTS`] (50K).
    pub(crate) save_threshold_events: u64,
    /// Time-since-last-save ceiling (Phase 7 task 7.4).
    /// Crossing this triggers a [`SaveReason::AgeElapsed`] save
    /// when at least one event is pending.  Default
    /// [`DEFAULT_SAVE_THRESHOLD_AGE`] (5 min).
    pub(crate) save_threshold_age: Duration,
    /// Minimum gap between two events-driven saves.  Default
    /// [`DEFAULT_SAVE_MIN_INTERVAL`] (60 s).
    pub(crate) save_min_interval: Duration,
    /// Fewest pending events for the age save.  Default
    /// [`DEFAULT_SAVE_AGE_MIN_EVENTS`] (1 000).
    pub(crate) save_age_min_events: u64,
    /// Age after which any pending event makes a save due.  Default
    /// [`DEFAULT_SAVE_AGE_CEILING`] (1 h).
    pub(crate) save_age_ceiling: Duration,
    /// Search-freshness apply **max-wait** cap (Phase 5) — the ceiling of the
    /// debounce model. A burst that never settles is force-applied at least
    /// this often, bounding both freshness lag and apply CPU under sustained
    /// churn. Default [`DEFAULT_APPLY_INTERVAL_MS`] (2 s).
    pub(crate) apply_interval: Duration,
    /// Search-freshness apply **debounce / settle** window (Phase 5) — once
    /// buffered changes have been quiet this long, coalesce them into one
    /// apply, so an idle→active transition goes searchable promptly. Default
    /// [`DEFAULT_APPLY_DEBOUNCE_MS`] (250 ms).
    pub(crate) apply_debounce: Duration,
}

impl JournalLoopConfig {
    /// The save thresholds as one [`SavePolicy`].
    pub(crate) const fn save_policy(&self) -> SavePolicy {
        SavePolicy {
            threshold_events: self.save_threshold_events,
            min_interval: self.save_min_interval,
            threshold_age: self.save_threshold_age,
            age_min_events: self.save_age_min_events,
            age_ceiling: self.save_age_ceiling,
        }
    }
}

impl Default for JournalLoopConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
            initial_cursor: 0,
            save_threshold_events: DEFAULT_SAVE_THRESHOLD_EVENTS,
            save_threshold_age: DEFAULT_SAVE_THRESHOLD_AGE,
            save_min_interval: DEFAULT_SAVE_MIN_INTERVAL,
            save_age_min_events: DEFAULT_SAVE_AGE_MIN_EVENTS,
            save_age_ceiling: DEFAULT_SAVE_AGE_CEILING,
            apply_interval: Duration::from_millis(DEFAULT_APPLY_INTERVAL_MS),
            apply_debounce: Duration::from_millis(DEFAULT_APPLY_DEBOUNCE_MS),
        }
    }
}

/// Env var overriding [`JournalLoopConfig::poll_interval`] (milliseconds).
pub(crate) const POLL_INTERVAL_ENV: &str = "UFFS_USN_POLL_INTERVAL_MS";

/// Env var overriding [`JournalLoopConfig::apply_interval`] (the max-wait cap,
/// milliseconds).
pub(crate) const APPLY_INTERVAL_ENV: &str = "UFFS_USN_APPLY_INTERVAL_MS";

/// Env var overriding [`JournalLoopConfig::apply_debounce`] (milliseconds).
pub(crate) const APPLY_DEBOUNCE_ENV: &str = "UFFS_USN_APPLY_DEBOUNCE_MS";

impl JournalLoopConfig {
    /// Build the production config: [`Self::default`] with the two
    /// millisecond-valued env overrides applied when present + parseable.
    ///
    /// `UFFS_USN_POLL_INTERVAL_MS` dials the poll cadence (long
    /// documented; this is the wire-up) and `UFFS_USN_APPLY_INTERVAL_MS`
    /// dials the search-freshness apply cadence.  A missing, empty, or
    /// unparseable value leaves the corresponding default untouched and
    /// warn-logs the bad input so a typo is visible rather than silently
    /// ignored.  A `0` is accepted verbatim (apply / poll on every tick)
    /// — useful for tests and aggressive soak runs.
    #[must_use]
    pub(crate) fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(ms) = env_millis(POLL_INTERVAL_ENV) {
            config.poll_interval = Duration::from_millis(ms);
        }
        if let Some(ms) = env_millis(APPLY_INTERVAL_ENV) {
            config.apply_interval = Duration::from_millis(ms);
        }
        if let Some(ms) = env_millis(APPLY_DEBOUNCE_ENV) {
            config.apply_debounce = Duration::from_millis(ms);
        }
        config
    }
}

/// Read `name` from the environment as a `u64` millisecond count.
///
/// Returns `None` (caller keeps the default) when the var is unset,
/// empty, or non-numeric; a non-numeric value also warn-logs so the
/// operator sees the typo instead of silently getting the default.
fn env_millis(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<u64>() {
        Ok(ms) => Some(ms),
        Err(parse_err) => {
            tracing::warn!(
                env = name,
                value = trimmed,
                error = %parse_err,
                "Ignoring non-numeric journal-loop interval override; using default"
            );
            None
        }
    }
}

/// Per-shard journal-polling state machine.
///
/// Holds the trait-object `source` + `sink` that wire it to the
/// real journal + registry, the drive `letter` it serves, the
/// cancellation receiver that lets the daemon's shutdown tear it
/// down within one poll interval, and the [`JournalLoopConfig`] for
/// cadence + cursor seeding.
///
/// Construction is `pub(crate)` so the production spawn path
/// ([`spawn_journal_loop`]) and test paths can both build it; the
/// only state-machine entry point is [`Self::run`].
pub(crate) struct JournalLoop {
    /// Drive letter this loop polls.
    letter: uffs_mft::platform::DriveLetter,
    /// Plug-in journal-data source.
    source: Arc<dyn JournalSource>,
    /// Plug-in patch consumer.
    sink: Arc<dyn PatchSink>,
    /// Plug-in cursor-persistence surface (Phase 7 task 7.6).
    /// Loaded once at start of `run` to seed `cursor`; stored at
    /// the same time as each `trigger_save` call so on-disk cursor
    /// and on-disk body advance together.
    cursor_store: Arc<dyn CursorStore>,
    /// Cancellation channel — set to `true` by the daemon shutdown
    /// path; the loop checks on every iteration and exits cleanly.
    cancel_rx: watch::Receiver<bool>,
    /// Tuning knobs.
    config: JournalLoopConfig,
    /// Per-shard save-threshold state (Phase 7 task 7.4).
    /// Mutated on every non-empty tick by [`SaveTrigger::record`]
    /// + [`SaveTrigger::evaluate`].
    save_trigger: SaveTrigger,
    /// Per-shard apply-cadence state — the search-freshness sibling
    /// of `save_trigger`.  Mutated on every non-empty tick by
    /// [`ApplyTrigger::record`] + [`ApplyTrigger::evaluate`], and
    /// reset via [`ApplyTrigger::reset_after_save`] when a save tick
    /// subsumes the apply.
    apply_trigger: ApplyTrigger,
    /// Last `journal_id` observed from a non-zero-id poll
    /// (Phase 7 task 7.7 wrap detection).  `None` until the first
    /// non-zero `journal_id` is observed; transitions to
    /// `Some(id)` on the first such poll, then any subsequent
    /// poll with `journal_id != id` (and `journal_id != 0`) fires
    /// `sink.journal_wrapped` and resets the cursor.
    last_journal_id: Option<u64>,
}

impl JournalLoop {
    /// Construct a loop bound to `letter`, polling `source`,
    /// applying via `sink`, persisting cursor via `cursor_store`,
    /// watching `cancel_rx`, configured by `config`.
    #[must_use]
    pub(crate) fn new(
        letter: uffs_mft::platform::DriveLetter,
        source: Arc<dyn JournalSource>,
        sink: Arc<dyn PatchSink>,
        cursor_store: Arc<dyn CursorStore>,
        cancel_rx: watch::Receiver<bool>,
        config: JournalLoopConfig,
    ) -> Self {
        Self {
            letter,
            source,
            sink,
            cursor_store,
            cancel_rx,
            config,
            save_trigger: SaveTrigger::new(),
            apply_trigger: ApplyTrigger::new(),
            last_journal_id: None,
        }
    }

    /// Run the loop until `cancel_rx` flips to `true` (typically
    /// the daemon's shutdown path).  Polls `source` every
    /// `config.poll_interval`, applies non-empty change batches
    /// via `sink`, advances the cursor.
    ///
    /// On a poll error: warn-logs and retries on the next tick.
    /// One failure does **not** abort the loop — the journal can
    /// be transiently unavailable (volume revocation, broker
    /// reconnect) and the daemon should resume cleanly when the
    /// surface returns.
    pub(crate) async fn run(mut self) {
        let letter = self.letter;
        // Seed cursor from the persistence store; fall back to
        // the config's initial_cursor when the store returns 0
        // (no persisted cursor for this letter yet).
        let mut cursor = match self.cursor_store.load(letter) {
            0 => self.config.initial_cursor,
            persisted => persisted,
        };
        let mut backoff = PollBackoff::new(self.config.poll_interval, MAX_POLL_BACKOFF);
        loop {
            if !poll::wait_for_next_tick(&mut self.cancel_rx, backoff.current(), letter).await {
                return;
            }

            let result = match poll::poll_blocking(Arc::clone(&self.source), cursor).await {
                Ok(result) => {
                    if backoff.on_success() {
                        tracing::info!(
                            drive = %letter,
                            "Journal poll recovered; resuming normal cadence"
                        );
                    }
                    result
                }
                Err(failure) => {
                    let streak = backoff.on_failure();
                    poll::log_poll_failure(letter, &failure, streak, backoff.current());
                    continue;
                }
            };

            // Wrap-detection (Phase 7 task 7.7): if the journal_id
            // changed between two successive non-zero-id polls, the
            // journal was recreated.  Notify the sink, reset cursor
            // to the new journal's head, skip the patch.
            if let Some(prev_id) = self.last_journal_id
                && result.journal_id != 0
                && result.journal_id != prev_id
            {
                tracing::warn!(
                    drive = %letter,
                    prev_journal_id = prev_id,
                    new_journal_id = result.journal_id,
                    "Journal wrap detected; sink must force-rebuild body"
                );
                self.sink.journal_wrapped(letter);
                cursor = 0;
                self.last_journal_id = Some(result.journal_id);
                self.cursor_store.store(letter, cursor);
                continue;
            }
            if result.journal_id != 0 {
                self.last_journal_id = Some(result.journal_id);
            }

            cursor = result.next_cursor;
            // The cursor is persisted by the sink in lockstep with the
            // compact-cache body save (and only when that save actually
            // happens), so the loop no longer persists it here — doing
            // so would let a parked shard's on-disk cursor outrun its
            // frozen on-disk body.  See `PatchSink::trigger_save`.
            process_tick(
                self.sink.as_ref(),
                letter,
                cursor,
                &result.changes,
                &mut self.save_trigger,
                &mut self.apply_trigger,
                &self.config,
            );
        }
    }
}

/// Buffer the post-poll change batch into `sink`, record it into both
/// cadence triggers, and fire whichever is due via [`fire_due_cadence`]
/// — or trace-log the no-op tick when `changes` is empty.
///
/// On an idle drive neither cadence fires (both event counters stay at
/// zero), so a quiescent volume costs nothing beyond the poll itself.
fn process_tick(
    sink: &dyn PatchSink,
    letter: uffs_mft::platform::DriveLetter,
    cursor: u64,
    changes: &[FileChange],
    save_trigger: &mut SaveTrigger,
    apply_trigger: &mut ApplyTrigger,
    config: &JournalLoopConfig,
) {
    if changes.is_empty() {
        tracing::trace!(drive = %letter, "Journal poll: no changes");
        if sink.has_pending(letter) {
            apply_trigger.record();
        }
    } else {
        let accepted = sink.accept(letter, changes);
        save_trigger.record(changes.len() as u64);
        apply_trigger.record();
        tracing::debug!(
            drive = %letter,
            accepted,
            change_count = changes.len(),
            cursor,
            "Journal poll: buffered tick"
        );
    }

    // Phase 5: evaluate the cadences EVERY tick, not only on change-ticks. The
    // apply debounce/settle and the age-based save must be able to fire on a
    // QUIET tick (the first poll after a burst ends, or once a low-churn run
    // has aged out) — not wait for the next new change. Both evaluations are
    // cheap no-ops when nothing is pending, so an idle drive costs nothing
    // beyond the poll itself.
    fire_due_cadence(sink, letter, cursor, save_trigger, apply_trigger, config);
}

/// Fire whichever cadence is due this tick — **at most one**, since a
/// save drains + applies the same buffer an apply would (a save
/// subsumes an apply).
///
/// * **Save tick** (rare — 50k events / 5 min): fire
///   [`PatchSink::trigger_save`] (passing `cursor` so the sink persists it in
///   lockstep with the on-disk body), then [`ApplyTrigger::reset_after_save`]
///   so the loop doesn't redundantly re-apply the just-drained buffer.
/// * **Apply tick** (default 30 s): when no save fired, fire
///   [`PatchSink::trigger_apply`] so the in-memory body (and search) goes
///   near-live without the disk write.
///
/// Extracted from [`process_tick`] so each function stays under
/// clippy's strict-gate cognitive-complexity ceiling.
fn fire_due_cadence(
    sink: &dyn PatchSink,
    letter: uffs_mft::platform::DriveLetter,
    cursor: u64,
    save_trigger: &mut SaveTrigger,
    apply_trigger: &mut ApplyTrigger,
    config: &JournalLoopConfig,
) {
    if let Some(reason) = save_trigger.evaluate(&config.save_policy()) {
        // A save drains + applies the buffer AND writes the body to
        // disk + persists the cursor — so it subsumes the apply tick.
        sink.trigger_save(letter, reason, cursor);
        apply_trigger.reset_after_save();
        tracing::info!(
            drive = %letter,
            ?reason,
            cursor,
            "Journal poll: triggered background compact-cache save"
        );
    } else if apply_trigger.evaluate(config.apply_debounce, config.apply_interval) {
        // No save this tick: patch the in-memory body so search goes near-live.
        // Fires once a burst settles (debounce) or at the max-wait cap under
        // sustained churn — disk persistence stays a later save tick's job.
        sink.trigger_apply(letter);
        tracing::debug!(
            drive = %letter,
            "Journal poll: triggered near-live body apply"
        );
    }
}

/// Handle returned by [`spawn_journal_loop`] for cancellation +
/// join.  Holding it keeps the loop alive; dropping the
/// `cancel_tx` causes the loop to exit on its next iteration via
/// the `watch` receiver's `changed()` arm.
pub(crate) struct JournalLoopHandle {
    /// Sender side of the cancellation watch.  Setting it to
    /// `true` (or dropping it) causes the loop to exit.
    cancel_tx: watch::Sender<bool>,
    /// Joinable handle on the spawned loop task.  Awaiting this
    /// after a `cancel()` or `cancel_tx` drop blocks until the
    /// loop has finished its in-flight tick and returned.
    join: tokio::task::JoinHandle<()>,
}

impl JournalLoopHandle {
    /// Request cancellation and return the join handle.  After
    /// this call, awaiting the returned `JoinHandle` blocks until
    /// the loop's next iteration observes the signal and returns
    /// (typically within one [`JournalLoopConfig::poll_interval`]).
    pub(crate) fn cancel(self) -> tokio::task::JoinHandle<()> {
        let _ignore = self.cancel_tx.send(true);
        self.join
    }
}

/// Spawn a journal loop on the current runtime.  Returns a
/// [`JournalLoopHandle`] for cancellation + join.
///
/// Caller responsibility: ensure the runtime is alive for the
/// duration of the loop, and call [`JournalLoopHandle::cancel`]
/// before the runtime tears down.
#[must_use]
pub(crate) fn spawn_journal_loop(
    letter: uffs_mft::platform::DriveLetter,
    source: Arc<dyn JournalSource>,
    sink: Arc<dyn PatchSink>,
    cursor_store: Arc<dyn CursorStore>,
    config: JournalLoopConfig,
) -> JournalLoopHandle {
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let loop_state = JournalLoop::new(letter, source, sink, cursor_store, cancel_rx, config);
    let join = tokio::spawn(loop_state.run());
    JournalLoopHandle { cancel_tx, join }
}

pub(crate) mod sources;

#[cfg(test)]
mod tests;
