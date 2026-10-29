// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 7 24-h soak harness contract — pin the literal log message
//! text that `scripts/dev/long-soak.rs::validate_phase7` greps for.
//!
//! The Phase 7 24-h soak validator's "Encrypted-cache refresh fired
//! during soak" assertion (see `scripts/dev/long-soak.rs:1244`) is a
//! line-oriented regex scrape against `daemon.log`, anchored on the
//! literal substring `"compact-cache save"`.  That substring lives
//! in the `tracing::info!` call inside
//! [`super::super::process_tick`] — the **only** place in the daemon
//! that emits the message:
//!
//! ```text
//! INFO Journal poll: triggered background compact-cache save \
//!     drive=F reason=AgeElapsed cursor=151008
//! ```
//!
//! The 2026-05-11 24-h Phase 7 soak captured 11 such events across
//! drives F / S — the save pipeline was healthy, but the pre-fix
//! validator hunted for `USN refresh tick|trigger_save|threshold.*\
//! save|encrypted cache refresh` which matches **none** of them.
//! The harness fix (`scripts/dev/long-soak.rs:1244`) re-anchored on
//! `compact-cache save`; this test pins the daemon side so a
//! future log-message rename fails CI before reaching another
//! 24-h soak.
//!
//! Three contracts:
//!
//! 1. Target = `uffs_daemon::cache::journal_loop` (the default module-path
//!    target — proves the `RUST_LOG=uffs_daemon=info` filter the harness sets
//!    is sufficient to enable the event).
//! 2. Level = `INFO` (so a "downgrade to DEBUG" refactor doesn't silently fall
//!    below the `uffs_daemon=info` env-filter cutoff).
//! 3. Message text **contains** the literal substring `compact-cache save` —
//!    the validator's regex anchor.

#![expect(
    clippy::indexing_slicing,
    reason = "test code — assertions index into known-shape vectors"
)]

use core::time::Duration;

use super::super::{
    ApplyTrigger, JournalLoopConfig, PatchSink, SaveReason, SaveTrigger, process_tick,
};
use super::{RecordingSink, one_change};
use crate::index::tests::tracing_capture::{CapturedEvent, EventLog};

/// Phase 7 fix (2026-05-13 24-h soak finding) — pin the literal
/// `"compact-cache save"` substring in the INFO-level tracing event
/// emitted by [`super::super::process_tick`] when a
/// [`SaveTrigger`] threshold crosses.
///
/// See module-level docs for the full rationale (validator regex
/// drift, 24-h soak silent miss, etc.).
#[test]
fn compact_cache_save_log_message_pins_string_target_and_level() {
    // Same dummy-Dispatch + thread-local-default + interest-rebuild
    // dance as `crate::index::tests::idle_demote::
    // shard_transition_events_emitted_on_demote_and_promote` — see
    // that test's docstring for the parallel-test interest-cache
    // race rationale.  `process_tick` is synchronous and runs on
    // the test thread, so the thread-local subscriber captures its
    // events directly (no `spawn_blocking` to defeat the thread-
    // local default).
    let log = EventLog::default();
    let _interest_rebuild_dummy =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(log.clone());
    tracing::callsite::rebuild_interest_cache();

    let sink = RecordingSink::new();
    let mut save_trigger = SaveTrigger::new();
    let mut apply_trigger = ApplyTrigger::new();

    // Force the events-threshold path: three changes against a
    // threshold of one crosses on the first evaluate() call.
    // Age threshold set generously so it can't be the path that
    // fires (we want a deterministic `EventsExceeded` reason), and
    // the apply interval is disabled so the save path is the only one
    // that can fire on this tick.
    let config = JournalLoopConfig {
        save_threshold_events: 1, // tight — crosses on the first evaluate
        save_threshold_age: Duration::from_hours(1), // generous
        save_min_interval: Duration::ZERO, // no gap: the first crossing saves
        save_age_min_events: 0,
        save_age_ceiling: Duration::MAX,
        apply_interval: Duration::from_hours(1), // disabled for this test
        apply_debounce: Duration::from_hours(1),
        ..JournalLoopConfig::default()
    };
    let changes = [one_change(10), one_change(11), one_change(12)];
    process_tick(
        &sink as &dyn PatchSink,
        uffs_mft::platform::DriveLetter::C,
        100, // cursor
        &changes,
        &mut save_trigger,
        &mut apply_trigger,
        &config,
    );

    // The sink saw the trigger_save callback once with the expected
    // (letter, reason) pair — this proves the threshold crossed and
    // fired a save.  This is the behavioral contract; the *log
    // message* below is the soak-harness contract.
    let save_calls = sink.save_calls();
    assert_eq!(
        save_calls.as_slice(),
        &[(
            uffs_mft::platform::DriveLetter::C,
            SaveReason::EventsExceeded
        )],
        "trigger_save must fire exactly once with EventsExceeded reason; got {save_calls:?}",
    );
    // The cursor passed to process_tick must be handed through to the
    // sink so it can persist it in lockstep with the body save.
    assert_eq!(
        sink.save_cursors().as_slice(),
        &[100],
        "process_tick must forward the tick cursor to trigger_save",
    );

    // Find the INFO event the soak harness greps for.
    let events = log.events();
    let save_events: Vec<&CapturedEvent> = events
        .iter()
        .filter(|event| {
            event.level == tracing::Level::INFO
                && event.field("drive") == Some("C")
                && event
                    .field("message")
                    .is_some_and(|msg| msg.contains("compact-cache save"))
        })
        .collect();
    assert!(
        !save_events.is_empty(),
        "expected at least one INFO-level event containing the literal \
         `compact-cache save` substring (the Phase 7 soak harness's \
         `scripts/dev/long-soak.rs::validate_phase7` regex anchor); \
         got {} events total: {:#?}",
        events.len(),
        events,
    );
    let event = save_events[0];

    // Contract 1: target = the default module-path target.  The
    // soak harness sets `RUST_LOG=uffs_daemon=info,...` so the
    // env filter cutoff is the `uffs_daemon=` prefix.  A future
    // refactor that moved this event under a custom target (e.g.
    // `target: "shard.refresh"`) would change which env-filter
    // rule enables it — pin the current target to catch such
    // moves before the harness silently regresses.
    assert_eq!(
        event.target, "uffs_daemon::cache::journal_loop",
        "compact-cache-save event must stay on the default module-path \
         target; moving it under a custom target requires updating the \
         soak harness's RUST_LOG env in `scripts/dev/long-soak.rs::run_phase7`",
    );

    // Contract 2: level = INFO.  The harness's env-filter uses
    // `uffs_daemon=info`; demoting to DEBUG silently drops the
    // event below the cutoff and the soak fails to observe the
    // save pipeline.
    assert_eq!(
        event.level,
        tracing::Level::INFO,
        "compact-cache-save event must stay at INFO — Phase 7 soak \
         harness's `uffs_daemon=info` RUST_LOG cutoff depends on it",
    );

    // Contract 3: literal substring the validator's regex anchors
    // on.  `Regex::new(r\"compact-cache save\")` in
    // `scripts/dev/long-soak.rs::validate_phase7`.
    let msg = event
        .field("message")
        .expect("filtered set guarantees message field");
    assert!(
        msg.contains("compact-cache save"),
        "message must contain the literal `compact-cache save` substring; \
         got: {msg:?}",
    );

    // Structured fields the operator runbook references — pin
    // presence so a future "drop the redundant cursor field"
    // refactor doesn't quietly break log-driven diagnostics.
    assert!(event.has_field("drive"));
    assert!(event.has_field("reason"));
    assert!(event.has_field("cursor"));
}
