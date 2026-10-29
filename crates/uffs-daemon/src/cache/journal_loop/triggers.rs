// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Per-shard cadence state machines for the journal loop.
//!
//! Two independent counters govern when the loop drains its pending
//! change buffer, deliberately decoupled so search freshness and disk
//! persistence run on their own clocks:
//!
//! * [`SaveTrigger`] — the rare, expensive **disk save** (default 50k events /
//!   5 min).  Crossing either threshold patches the body AND persists the
//!   compact cache + cursor.
//! * [`ApplyTrigger`] — the frequent, disk-free **in-memory apply** (Phase 5
//!   debounce 250 ms / max-wait 2 s).  A settled burst — or the max-wait cap
//!   under sustained churn — patches the body so a freshly created / renamed /
//!   deleted file becomes searchable, without touching disk.
//!
//! A save subsumes an apply (it drains + applies the same buffer), so
//! the loop fires at most one of the two per tick; see
//! [`super::process_tick`].  Extracted from `journal_loop.rs` to keep
//! that file under the workspace file-size policy while keeping the two
//! cadence machines (and the threshold constants they default from)
//! together as one auditable unit.

use core::time::Duration;
use std::time::Instant;

/// Default events-since-save threshold for triggering a background
/// compact-cache save (Phase 7 task 7.4).
///
/// Sized to approximate the plan's "5% churn" criterion at the
/// typical 1.3 GB × ~7 M-record drive shape (`50_000` events ≈ 0.7%
/// churn, comfortably below 5%).  Saving more frequently would
/// thrash the disk; less frequently would let the on-disk snapshot
/// drift far enough that a cold-boot replay window grows beyond
/// the cost of an incremental save.
pub(crate) const DEFAULT_SAVE_THRESHOLD_EVENTS: u64 = 50_000;

/// Default time-since-save threshold for triggering a background
/// compact-cache save (Phase 7 task 7.4) — 5 minutes.
///
/// Provides a wall-clock ceiling for how stale the on-disk snapshot
/// can get under low-churn workloads (where the events-threshold
/// would never fire on its own).  Five minutes matches the cadence
/// of the existing Phase-5 `refresh_usn_for_warm_shards` global
/// tick so the persistence guarantee carries over to the per-shard
/// path without changing the operator-visible recovery window.
pub(crate) const DEFAULT_SAVE_THRESHOLD_AGE: Duration = Duration::from_mins(5);

/// Fewest pending events that make the [`DEFAULT_SAVE_THRESHOLD_AGE`] save
/// worth it — 1 000.
///
/// A save rewrites the drive's whole cache.  Measured on the winbox
/// (2026-10-28/29), the age save of the 30.8 M-record D: cost ~60 CPU-seconds
/// and several GB every 5 minutes for a handful of changes (its cursor moved a
/// few hundred bytes).  Fewer changes than this cost nothing to replay from the
/// journal on a cold start or a re-warm, so they wait for
/// [`DEFAULT_SAVE_AGE_CEILING`] instead.
pub(crate) const DEFAULT_SAVE_AGE_MIN_EVENTS: u64 = 1_000;

/// Longest a drive with any pending events goes without a save — 1 hour.
///
/// Bounds how far behind the on-disk cache of a quiet drive can fall, so the
/// replay a re-warm needs stays well inside what one targeted MFT read covers.
pub(crate) const DEFAULT_SAVE_AGE_CEILING: Duration = Duration::from_hours(1);

/// Default minimum gap between two **events-driven** saves — 60 seconds.
///
/// Measured on the winbox 2026-10-28 (issue #639): one compact-cache save of a
/// 4 M-record C: costs 15-35 CPU-seconds and 1-5 GB of transient memory, and a
/// 200 k-file burst crossed [`DEFAULT_SAVE_THRESHOLD_EVENTS`] every ~18 s, so
/// saves ran back to back for the whole burst.  The saves only bound how far a
/// cold start must replay the journal; search freshness comes from the apply
/// tick, which is unaffected.  One save a minute under sustained churn keeps
/// that replay window small while giving the CPU back to whatever is causing
/// the churn.  The [`DEFAULT_SAVE_THRESHOLD_AGE`] save is not gated by this.
pub(crate) const DEFAULT_SAVE_MIN_INTERVAL: Duration = Duration::from_mins(1);

/// Default apply **max-wait** cap for the per-shard journal loop — 2 seconds
/// (Phase 5).
///
/// This is the ceiling of the debounce model in [`ApplyTrigger`]: under
/// *sustained* back-to-back churn (a burst that never settles), the loop still
/// applies at least every this long so search freshness never lags more than
/// the cap. It is the CPU governor — each apply now costs ~200 ms on a multi-
/// million-record drive (Phases 1-4 made paths/trigram/ext/children
/// incremental), so a 2 s cap throttles a constantly-churning volume to
/// ~200 ms / 2 s ≈ 10 % of one core instead of a continuous drag.
///
/// The much shorter [`DEFAULT_APPLY_DEBOUNCE_MS`] is what makes the common
/// case feel snappy; this cap only bites when changes never stop.
///
/// Overridable at runtime via `UFFS_USN_APPLY_INTERVAL_MS`, so soak tests and
/// latency-sensitive setups can dial the cap up or down without recompiling.
pub(crate) const DEFAULT_APPLY_INTERVAL_MS: u64 = 2_000;

/// Default apply **debounce / settle** window — 250 ms (Phase 5).
///
/// The snappy half of the apply model: once a run of changes has been quiet for
/// this long (the burst settled), the loop coalesces it into one apply and the
/// new file is searchable. So an idle→active transition (saving one file,
/// finishing an unzip) becomes visible in well under a second, while a
/// continuous burst keeps re-arming the window and falls back to the
/// [`DEFAULT_APPLY_INTERVAL_MS`] cap. Sized below the 500 ms poll cadence so
/// the first quiet poll after a burst always satisfies it.
///
/// Overridable via `UFFS_USN_APPLY_DEBOUNCE_MS`.
pub(crate) const DEFAULT_APPLY_DEBOUNCE_MS: u64 = 250;

/// Why a [`super::PatchSink::trigger_save`] call fired.
///
/// Encoded so observability surfaces (logs, metrics) can
/// distinguish heavy-churn-driven saves from time-pressure-driven
/// saves; the production sink also passes this through to the
/// compact-cache writer for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveReason {
    /// `events_since_save >= save_threshold_events` — lots of
    /// churn has accumulated and the on-disk snapshot is
    /// progressively stale.
    EventsExceeded,
    /// `Instant::now() - last_save_at >= save_threshold_age` —
    /// time-pressure path for low-churn drives where the
    /// events threshold would otherwise never fire.
    AgeElapsed,
}

/// The thresholds [`SaveTrigger::evaluate`] checks (see the `DEFAULT_SAVE_*`
/// constants for the defaults and their reasons).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavePolicy {
    /// Pending events that make a save due.
    pub(crate) threshold_events: u64,
    /// Minimum gap between two events-driven saves.
    pub(crate) min_interval: Duration,
    /// Age after which a save is due, given `age_min_events`.
    pub(crate) threshold_age: Duration,
    /// Fewest pending events for the age save.
    pub(crate) age_min_events: u64,
    /// Age after which any pending event makes a save due.
    pub(crate) age_ceiling: Duration,
}

/// Per-shard save-threshold state machine (Phase 7 task 7.4).
///
/// Tracks the wall-clock time of the last save trigger and the
/// number of events accumulated since.  Crossing either the
/// events- or age-threshold (with at least one event pending)
/// produces a [`SaveReason`] and resets both counters.  Held
/// inside the [`super::JournalLoop`] so each per-shard task carries
/// its own independent counters.
#[derive(Debug)]
pub(crate) struct SaveTrigger {
    /// Wall-clock time of the last save trigger (or, before any
    /// triggers, the loop's spawn time).  Compared against
    /// `Instant::now()` to compute elapsed-since-last-save.
    last_save_at: Instant,
    /// Total events accumulated across [`Self::record`] calls
    /// since the last save trigger.  Compared against
    /// `save_threshold_events` to fire the events-based save.
    events_since_save: u64,
}

impl SaveTrigger {
    /// Construct a fresh trigger with `last_save_at` set to
    /// `Instant::now()` (so the first age-based save can't fire
    /// until at least `save_threshold_age` has elapsed since
    /// loop spawn).
    pub(super) fn new() -> Self {
        Self {
            last_save_at: Instant::now(),
            events_since_save: 0,
        }
    }

    /// Record `change_count` events accumulating toward the
    /// events-based threshold.  Saturating add so a runaway
    /// drive can't wrap and silently miss the threshold.
    pub(super) const fn record(&mut self, change_count: u64) {
        self.events_since_save = self.events_since_save.saturating_add(change_count);
    }

    /// Evaluate the thresholds.
    ///
    /// **Returns** `Some(reason)` if a save should fire — and
    /// resets both counters as a side effect (so the next
    /// `evaluate` after a save starts from a clean slate).
    /// Returns `None` when no threshold is crossed *or* when no
    /// events are pending (zero-churn drives never produce
    /// no-op saves).
    ///
    /// The events threshold only fires once `min_interval` has passed since
    /// the previous save, so a sustained burst saves at most once per interval
    /// instead of on every threshold crossing.  The age threshold only fires
    /// with at least `age_min_events` pending, or once `age_ceiling` has
    /// passed with any.
    pub(super) fn evaluate(&mut self, policy: &SavePolicy) -> Option<SaveReason> {
        if self.events_since_save == 0 {
            return None;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_save_at);
        let events_due =
            self.events_since_save >= policy.threshold_events && elapsed >= policy.min_interval;
        let age_due = (elapsed >= policy.threshold_age
            && self.events_since_save >= policy.age_min_events)
            || elapsed >= policy.age_ceiling;
        let reason = if events_due {
            Some(SaveReason::EventsExceeded)
        } else if age_due {
            Some(SaveReason::AgeElapsed)
        } else {
            None
        };
        if reason.is_some() {
            self.last_save_at = now;
            self.events_since_save = 0;
        }
        reason
    }
}

/// Per-shard apply-cadence state machine — the search-freshness
/// counterpart to [`SaveTrigger`].
///
/// Where `SaveTrigger` governs the rare, expensive disk save (50k events /
/// 5 min), this governs the frequent, in-memory body patch that makes a
/// created / renamed / deleted file searchable. Decoupling the two is the
/// whole point: search must go near-live promptly, but the compact-cache disk
/// write should stay rare.
///
/// Phase 5 makes this a **debounce + max-wait** gate rather than a fixed
/// rate-limit, so it is both snappy and CPU-bounded:
///
/// * **debounce / settle** ([`DEFAULT_APPLY_DEBOUNCE_MS`], 250 ms) — once a run
///   of changes has been quiet for the debounce window, apply. An idle→active
///   transition (one saved file, a finished unzip) becomes searchable in well
///   under a second.
/// * **max-wait** ([`DEFAULT_APPLY_INTERVAL_MS`], 2 s) — a burst that never
///   settles is force-applied at the cap, so sustained churn collapses to one
///   ~200 ms apply per cap (~10 % of a core) instead of thrashing.
///
/// On an idle drive nothing is ever pending, so [`Self::evaluate`] is a cheap
/// no-op every poll.
#[derive(Debug)]
pub(crate) struct ApplyTrigger {
    /// When the first not-yet-applied change of the current run arrived, or
    /// `None` when nothing is pending. The **max-wait** cap is measured from
    /// here; `Some` is also the "there is something to apply" guard.
    first_change_at: Option<Instant>,
    /// When the most recent change arrived. The **debounce / settle** window is
    /// measured from here; only meaningful while `first_change_at` is `Some`.
    last_change_at: Instant,
}

impl ApplyTrigger {
    /// Construct a fresh trigger with nothing pending.
    pub(super) fn new() -> Self {
        Self {
            first_change_at: None,
            last_change_at: Instant::now(),
        }
    }

    /// Record that the latest poll observed at least one change: start the
    /// max-wait clock on the first change of a pending run, and (re)arm the
    /// debounce window. The exact count does not matter here — `SaveTrigger`
    /// owns the volume threshold; this gate only bounds latency.
    pub(super) fn record(&mut self) {
        let now = Instant::now();
        if self.first_change_at.is_none() {
            self.first_change_at = Some(now);
        }
        self.last_change_at = now;
    }

    /// Evaluate the debounce + max-wait gate.
    ///
    /// **Returns** `true` (and clears the pending run) when changes are pending
    /// AND either the burst has **settled** (no change for `debounce`) or the
    /// run has been pending past `max_wait`. Returns `false` — without
    /// clearing — otherwise, so an unsettled, not-yet-capped run keeps
    /// accumulating. Must be evaluated every poll (including quiet ones) so the
    /// settle can fire on the first quiet tick after a burst ends.
    pub(super) fn evaluate(&mut self, debounce: Duration, max_wait: Duration) -> bool {
        let Some(first) = self.first_change_at else {
            return false;
        };
        let now = Instant::now();
        let settled = now.saturating_duration_since(self.last_change_at) >= debounce;
        let capped = now.saturating_duration_since(first) >= max_wait;
        if settled || capped {
            self.first_change_at = None;
            true
        } else {
            false
        }
    }

    /// Reset the trigger because a **save** tick just drained + applied the
    /// buffer (a save subsumes an apply), so the loop doesn't redundantly
    /// re-apply the just-drained run.
    pub(super) const fn reset_after_save(&mut self) {
        self.first_change_at = None;
    }
}
