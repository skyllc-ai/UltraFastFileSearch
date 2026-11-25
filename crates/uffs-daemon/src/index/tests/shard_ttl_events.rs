// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 6 fix (2026-05-07 24-h soak finding) — `shard.ttl` event
//! shape contract tests.
//!
//! Extracted as a sibling module of [`super::idle_demote`] to keep
//! that file under the workspace's 800-LOC file-size policy
//! (`scripts/ci/check_file_size_policy.sh`).  Companion path
//! coverage on the pure-function helpers (`build_thresholds`,
//! `chosen_ttl_for_state`) lives in the `commit_c_tests` module
//! inside `crate::index::transitions`; this module is the contract
//! pin for the live demote controller.
//!
//! The shared `tracing::Subscriber` capture scaffold (`EventLog` /
//! `CapturedEvent`) lives in [`super::tracing_capture`].

use alloc::sync::Arc;

use super::tracing_capture::{CapturedEvent, EventLog};
use super::{IndexManager, build_test_drive};

/// Phase 6 24-h soak finding: the `shard.ttl` event used to emit
/// only `chosen_ttl_sec` (the outgoing edge of the drive's *current*
/// tier).  Drives in different tiers therefore reported different
/// fields, so a log-driven peer-vs-target audit could not compare
/// like-with-like — the original `chosen_ttl_sec exceeds peers`
/// assertion in the soak validator was structurally impossible to
/// pass under the default Phase 6 ladder (warm cap < parked base).
///
/// Post-fix: every `shard.ttl` event carries **all four** TTL
/// fields — `chosen_ttl_sec` (preserved for back-compat),
/// `hot_ttl_sec`, `warm_ttl_sec`, `parked_ttl_sec` — so consumers
/// can pick a single edge (`warm_ttl_sec` is the most rate-sensitive)
/// and compare across drives regardless of their current tier.
///
/// This test pins the new shape on the **idle-demote path** —
/// the canonical path the 24-h soak validator scrapes.  Companion
/// path coverage (clamp / below-ttl) lives in the `commit_c_tests`
/// module on the pure-function helpers; this integration test is
/// the contract pin for the live demote controller.
#[tokio::test]
async fn shard_ttl_event_emits_all_three_thresholds() {
    // All `use`s up-front to satisfy clippy's `items_after_statements`
    // pedantic gate.
    use crate::cache::ShardState;
    use crate::cache::policy::{
        HOT_TO_WARM_IDLE_SECS, PARKED_TO_COLD_IDLE_SECS, WARM_TO_PARKED_IDLE_SECS,
    };

    // Same dummy-Dispatch + thread-local-default + interest-rebuild
    // dance as `shard_transition_events_emitted_on_demote_and_promote`
    // — see that test's docstring for the rationale.
    let log = EventLog::default();
    let _interest_rebuild_dummy =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(log.clone());
    tracing::callsite::rebuild_interest_cache();

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    // Backdate so the next idle-demote tick fires the Warm→Parked
    // path (emits the canonical `idle-demote` shard.ttl event).
    let last_query_ms = 1_000_000_000_u64;
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, last_query_ms)
            .await
    );
    let now_ms = last_query_ms + WARM_TO_PARKED_IDLE_SECS * 1000;
    mgr.demote_idle_shards(now_ms).await;

    // Verify the Warm→Parked transition actually happened.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Parked
    )]);

    // Find the `idle-demote`-reason `shard.ttl` event for drive C.
    let events = log.events();
    let ttl_events: Vec<&CapturedEvent> = events
        .iter()
        .filter(|event| {
            event.target == "shard.ttl"
                && event.field("drive") == Some("C")
                && event.field("reason") == Some("idle-demote")
        })
        .collect();
    assert_eq!(
        ttl_events.len(),
        1,
        "expected exactly one `idle-demote` shard.ttl event for C, got {}: {:#?}",
        ttl_events.len(),
        ttl_events,
    );
    let event = *ttl_events.first().expect("ttl_events has an element 0");

    // Phase 6 fix contract: all four TTL fields present.
    // `chosen_ttl_sec` stays for back-compat, plus the three new
    // fields that make the log self-describing across tier states.
    assert!(
        event.has_field("chosen_ttl_sec"),
        "shard.ttl must keep emitting chosen_ttl_sec for back-compat \
         (existing dashboards / log greps depend on it)",
    );
    assert!(
        event.has_field("hot_ttl_sec"),
        "shard.ttl must emit hot_ttl_sec — Phase 6 audit contract",
    );
    assert!(
        event.has_field("warm_ttl_sec"),
        "shard.ttl must emit warm_ttl_sec — Phase 6 audit contract",
    );
    assert!(
        event.has_field("parked_ttl_sec"),
        "shard.ttl must emit parked_ttl_sec — Phase 6 audit contract",
    );

    // The default ladder + zero rate produces the documented base
    // values (the bonus formula collapses to 0 at rate_qpm = 0).
    // Pin the per-tier values so a future formula tweak that
    // accidentally wires the wrong threshold to the wrong field
    // would fail decisively.
    assert_eq!(
        event.field("hot_ttl_sec"),
        Some(HOT_TO_WARM_IDLE_SECS.to_string().as_str()),
    );
    assert_eq!(
        event.field("warm_ttl_sec"),
        Some(WARM_TO_PARKED_IDLE_SECS.to_string().as_str()),
    );
    assert_eq!(
        event.field("parked_ttl_sec"),
        Some(PARKED_TO_COLD_IDLE_SECS.to_string().as_str()),
    );
    // chosen_ttl_sec on a Warm-tier drive is the warm→parked edge.
    assert_eq!(
        event.field("chosen_ttl_sec"),
        Some(WARM_TO_PARKED_IDLE_SECS.to_string().as_str()),
        "Warm-tier drive's chosen_ttl_sec must be the warm→parked edge",
    );
}

/// Phase 6 fix (2026-05-13 24-h soak finding) — pin the **catch-all
/// below-ttl** event shape so the soak harness's `shard.ttl=trace`
/// log-scrape can never silently regress.
///
/// The Phase 6 soak validator's `warm_ttl_sec exceeds peers`
/// assertion in `scripts/dev/long-soak.rs::validate_phase6` greps
/// the daemon log for `shard.ttl` events with a `warm_ttl_sec=…`
/// field.  Under the post-fix daemon, drive C (clamped at
/// `min_tier="WARM"`) sits in Warm with `idle_secs ≈ 0` during
/// the synthetic-load window — the demote-eval ladder never
/// reaches the DEBUG-level `idle-demote` / `min-tier-clamp`
/// arms; only the **catch-all `(None, _)` arm** in
/// [`crate::index::transitions::evaluate_idle_demote`] fires, and
/// it does so at TRACE.  Three contracts must hold together:
///
/// 1. The catch-all event's `target` is `shard.ttl` (so the `shard.ttl=trace`
///    env filter actually opts it in).
/// 2. The event's level is `TRACE` (so a future "promote to DEBUG" refactor
///    doesn't double the production log volume, and a future "demote to DEBUG"
///    refactor doesn't break the soak harness's `shard.ttl=trace` env filter
///    contract).
/// 3. The event's message text contains `"Adaptive idle-demote evaluation: not
///    yet idle past TTL"` *literally* — operator runbooks and the soak
///    harness's line-oriented scrape both anchor on this string.  Renaming it
///    should require a deliberate downstream update.
/// 4. The event carries `reason="below-ttl"` plus the four TTL fields
///    (`chosen_ttl_sec` / `hot_ttl_sec` / `warm_ttl_sec` / `parked_ttl_sec`) —
///    same Phase 6 audit contract as the sibling `idle-demote` arm pinned
///    above.
#[tokio::test]
async fn below_ttl_event_pins_target_level_message_and_reason() {
    use crate::cache::ShardState;

    // Same dummy-Dispatch + thread-local-default + interest-rebuild
    // dance as the sibling tests — see `idle_demote.rs::
    // shard_transition_events_emitted_on_demote_and_promote` for
    // the parallel-test interest-cache-race rationale.
    let log = EventLog::default();
    let _interest_rebuild_dummy =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(log.clone());
    tracing::callsite::rebuild_interest_cache();

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;

    // Drive C is freshly Warm; `add_drive` seeded `last_query_at_ms`
    // to `unix_now_ms()` via `mark_loaded_at` (per
    // `mark_loaded_at_seeds_freshly_added_drive` in `idle_demote.rs`),
    // so `idle_secs ≈ 0` and `next_state_for_idle_with_thresholds`
    // returns `None` — exactly the below-ttl catch-all path.
    let now_ms = crate::cache::unix_now_ms();
    mgr.demote_idle_shards(now_ms).await;

    // The drive must NOT have demoted — that's the precondition
    // for the below-ttl branch to be the one that fired.
    let states = mgr.shard_states_for_test().await;
    assert_eq!(states, vec![(
        uffs_mft::platform::DriveLetter::C,
        ShardState::Warm
    )]);

    let events = log.events();
    let ttl_events: Vec<&CapturedEvent> = events
        .iter()
        .filter(|event| {
            event.target == "shard.ttl"
                && event.field("drive") == Some("C")
                && event.field("reason") == Some("below-ttl")
        })
        .collect();
    assert!(
        !ttl_events.is_empty(),
        "expected at least one `below-ttl` shard.ttl event for C, got {} events total: {:#?}",
        events.len(),
        events,
    );
    let event = *ttl_events.first().expect("ttl_events has an element 0");

    // Contract 1 + 2: target = "shard.ttl" at TRACE level so the
    // soak harness's `shard.ttl=trace` env filter is both necessary
    // (downgrading to DEBUG breaks the contract) and sufficient
    // (it opts in to this exact callsite).
    assert_eq!(event.target, "shard.ttl");
    assert_eq!(
        event.level,
        tracing::Level::TRACE,
        "below-ttl event must stay at TRACE — Phase 6 soak harness's \
         `shard.ttl=trace` RUST_LOG depends on it; promoting to DEBUG \
         doubles production log volume, demoting silently breaks the soak",
    );

    // Contract 3: literal message text the soak harness can grep
    // for, and that operator runbooks reference verbatim.
    assert_eq!(
        event.field("message"),
        Some("Adaptive idle-demote evaluation: not yet idle past TTL"),
        "below-ttl event's message must match the literal string the \
         Phase 6 soak harness `scripts/dev/long-soak.rs::validate_phase6` \
         and operator runbooks anchor on; renaming requires a deliberate \
         downstream sweep",
    );

    // Contract 4: reason + all four TTL fields present.
    assert_eq!(event.field("reason"), Some("below-ttl"));
    assert!(
        event.has_field("chosen_ttl_sec"),
        "below-ttl event must keep emitting chosen_ttl_sec for back-compat",
    );
    assert!(
        event.has_field("hot_ttl_sec"),
        "below-ttl event must emit hot_ttl_sec — Phase 6 audit contract",
    );
    assert!(
        event.has_field("warm_ttl_sec"),
        "below-ttl event must emit warm_ttl_sec — Phase 6 audit contract \
         (the soak harness's `parse_max_ttl_field` reads this field to \
         verify the adaptive bonus formula engaged under load)",
    );
    assert!(
        event.has_field("parked_ttl_sec"),
        "below-ttl event must emit parked_ttl_sec — Phase 6 audit contract",
    );
}
