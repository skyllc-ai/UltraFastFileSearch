// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Phase 3 Commit E — `shard.transition` tracing-event contract tests.
//!
//! Split from [`super::idle_demote`] so the state-transition ladder
//! tests stay focused on TTL / multi-drive behaviour while this
//! sibling module owns the operator-facing observability contract:
//!
//! * Plan task 3.9 — every demote / promote emits exactly one
//!   `tracing::event!(target: "shard.transition", ...)` with the `letter` /
//!   `from` / `to` / `reason` / `freed_mb` / `restored_mb` / `last_query_at_ms`
//!   field surface.
//! * PR-f — promote refreshes `last_query_at_ms` so the next idle tick doesn't
//!   immediately re-demote the just-promoted shard (anti-thrash invariant).
//!
//! Shared `EventLog` / `CapturedEvent` capture scaffold lives in
//! [`super::tracing_capture`].

use alloc::sync::Arc;

use super::tracing_capture::{CapturedEvent, EventLog};
use super::{FixedBodyLoader, IndexManager, build_test_drive};

// ── Phase 3 Commit E — tracing-event contract (plan task 3.9) ──────

/// Plan task 3.9 — every demote / promote transition emits exactly
/// one `tracing::event!(target: "shard.transition", ...)` event.
///
/// Pins the operator-facing observability contract: the tracing
/// fields (`letter`, `from`, `to`, `reason`, `freed_mb` /
/// `restored_mb`) are part of the public log surface, so a refactor
/// that silently drops or renames them would break dashboards and
/// alerting.  This test captures every event during a
/// demote-then-promote round-trip and asserts on the field values.
///
/// `tokio::test` defaults to a `current_thread` runtime, so the
/// thread-local `tracing::subscriber::set_default` we install at
/// the top of the test captures every event emitted from inside
/// the test future — including the events from `demote_letter` /
/// `promote_letter` running on the same thread.
#[tokio::test]
async fn shard_transition_events_emitted_on_demote_and_promote() {
    use crate::cache::ShardState;

    let log = EventLog::default();
    // Hold a second registered `Dispatch` for the duration of the test
    // body so `tracing-core` keeps `Dispatchers::has_just_one` at
    // `false`, which forces every callsite-interest rebuild to walk
    // the global `LOCKED_DISPATCHERS` list (containing OUR subscriber)
    // instead of taking the `JustOne` fast path.
    //
    // The `JustOne` fast path (`tracing-core::callsite::Dispatchers::
    // rebuilder` at v0.1.36 line 544–549) calls
    // `dispatcher::get_default(f)`, which is THREAD-LOCAL.  When a
    // sibling test on a different thread fires a `shard.transition`
    // callsite for the first time (registering it against
    // `NoSubscriber` because that thread never called `set_default`),
    // the per-callsite `Interest` cache gets pinned to `never` GLOBALLY,
    // and OUR thread-local subscriber never gets a chance to vote
    // — even if we later call `rebuild_interest_cache`, that rebuild
    // also takes the `JustOne` path and asks the wrong (sibling)
    // thread's default.
    //
    // Holding a dummy `NoSubscriber` Dispatch alive flips
    // `has_just_one` to `false`, so the rebuilder switches to
    // `Rebuilder::Read(LOCKED_DISPATCHERS)` which walks every
    // registered dispatcher (ours + the dummy).  Our subscriber's
    // `register_callsite` is then consulted on every callsite,
    // and the `Interest` cache reflects our `Interest::always`
    // independently of which thread first encountered the callsite.
    let _interest_rebuild_dummy =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(log.clone());
    // Force every existing callsite to re-register against the dispatchers
    // we just expanded with the dummy + our subscriber, in case sibling
    // tests already pinned cache entries to `never` before this test ran.
    tracing::callsite::rebuild_interest_cache();

    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;

    // Demote → expect one demote event.
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    // Promote via ensure_warm_for_dispatch → expect one promote event.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;

    let events = log.events();
    // Filter to the operator-facing observability contract: the
    // INFO-level `shard.transition` events with a tier-transition
    // `reason` (`demote`, `promote`, `usn-refresh`).  Other levels
    // on this target are best-effort observability noise that may
    // legitimately fire on a tier transition without violating the
    // contract — e.g. the Phase 5 `Prefetch::hint failed` warn,
    // which can fire on Linux for synthetic small heap regions
    // even when the promote itself succeeds (and is documented at
    // `crates/uffs-daemon/src/cache/prefetch.rs` to be best-effort
    // with warn-level logging on failure).  Pinning by level +
    // reason makes this test robust to the runtime-topology
    // detail of *which thread* the prefetch hint runs on (the
    // PR-e refactor moved the hint from a `spawn_blocking` closure
    // into the surrounding async task — both paths are observably
    // correct, but only the latter is captured by the
    // `set_default` thread-local subscriber on a `current_thread`
    // runtime).
    let transitions: Vec<&CapturedEvent> = events
        .iter()
        .filter(|event| {
            event.target == "shard.transition"
                && event.level == tracing::Level::INFO
                && matches!(
                    event.field("reason"),
                    Some("demote" | "promote" | "usn-refresh")
                )
        })
        .collect();

    assert_eq!(
        transitions.len(),
        2,
        "expected exactly two INFO `shard.transition` events with reason in \
         {{demote, promote, usn-refresh}} (one demote + one promote), got {}: {:#?}",
        transitions.len(),
        transitions
    );

    // `ShardState`'s `Display` impl emits lowercase variant names
    // (`warm`, `parked`, `cold`, …) — that's the wire contract this
    // test pins.  See `impl fmt::Display for ShardState` in
    // `cache/shard.rs`.
    let demote = *transitions.first().expect("transitions has an element 0");
    assert_eq!(demote.level, tracing::Level::INFO);
    assert_eq!(demote.field("reason"), Some("demote"));
    assert_eq!(demote.field("from"), Some("warm"));
    assert_eq!(demote.field("to"), Some("parked"));
    assert_eq!(demote.field("letter"), Some("C"));
    assert!(
        demote.has_field("freed_mb"),
        "demote event must carry freed_mb field for resident-delta accounting"
    );
    // G4 follow-up: `last_query_at_ms` is now part of the canonical
    // demote event (used to be cascade-only).  Pinning its presence
    // here so a future refactor can't drop the field and silently
    // break operator runbooks that grep for it.
    assert!(
        demote.has_field("last_query_at_ms"),
        "demote event must carry last_query_at_ms field (G4 follow-up)",
    );

    let promote = *transitions.get(1).expect("transitions has an element 1");
    assert_eq!(promote.level, tracing::Level::INFO);
    assert_eq!(promote.field("reason"), Some("promote"));
    assert_eq!(promote.field("from"), Some("parked"));
    assert_eq!(promote.field("to"), Some("warm"));
    assert_eq!(promote.field("letter"), Some("C"));
    assert!(
        promote.has_field("restored_mb"),
        "promote event must carry restored_mb field for resident-delta accounting"
    );
}

// Phase 6 fix (2026-05-07 24-h soak finding) — `shard.ttl` event
// shape regression test extracted to the sibling
// [`super::shard_ttl_events`] module to keep this file under the
// workspace's 800-LOC file-size policy.

// ── PR-f — promote-side `mark_loaded_at` regression test ──────────

/// Pin the PR-f fix at
/// `@/Users/rnio/Private/Github/UltraFastFileSearch/crates/uffs-daemon/src/
/// index/mod.rs:1208` — promoting a Parked shard whose `last_query_at_ms` is
/// older than `WARM_TO_PARKED_IDLE_SECS` must NOT cause an
/// immediate-re-demote on the very next idle tick.
///
/// **Background:** the v0.5.85 Windows soak captured a clear
/// promote-then-immediate-demote thrash in the daemon log (lines
/// 5540 → 5733 of `LOG/windows 0.5.85`):
///
/// ```text
/// 21:46:59.819  letter=G from=parked to=warm restored_mb=1
/// 21:47:04.754  letter=G from=warm   to=parked freed_mb=1
///                              ↑ only 4.9 s of "warm" life
/// 21:47:11      letter=G from=parked to=warm restored_mb=1   ← thrash
/// ```
///
/// Three drives (G/F/M) were re-demoted within 0.5 – 5 s of their
/// promotes, then re-promoted seconds later when the search
/// finally completed `ensure_warm_for_dispatch` and ran
/// `record_search_dispatch`.
///
/// **Root cause:** `IndexManager::ensure_warm_for_dispatch`'s
/// per-letter promote loop did the registry write-swap but did
/// not stamp `last_query_at_ms` on the freshly-promoted shard.
/// The shard inherited its pre-park value from the previous
/// `Arc<DriveStats>` (preserved by `new_warm_with_stats`).  When
/// the shard had been Parked for > 5 min, that inherited value
/// was already past `WARM_TO_PARKED_IDLE_SECS`, so the very next
/// 30-s `demote_idle_shards` tick (firing while the search was
/// still awaiting other concurrent loads) re-demoted the shard
/// before the eventual `record_search_dispatch` could refresh it.
///
/// **Fix:** call `shard.stats.mark_loaded_at(now_ms)` right after
/// the promote write-swap, mirroring the seed in
/// `Self::add_drive` and `Self::replace_drive`.
///
/// **Test sequence:**
///
/// 1. Add C with a fresh load timestamp (Phase-3 default).
/// 2. Park C, then backdate `last_query_at_ms` to a deep-past value (year 2001,
///    ≪ `WARM_TO_PARKED_IDLE_SECS` ago by any real wall clock).  This
///    faithfully reproduces the v0.5.85 state: a parked shard whose timestamp
///    is from a long time ago.
/// 3. Promote C via `ensure_warm_for_dispatch`.
/// 4. Immediately fire one `demote_idle_shards(unix_now_ms())` tick — the same
///    race-window the Windows soak hit.
/// 5. Assert C is still Warm.  Without PR-f, `idle_secs` would be
///    `(unix_now_ms() - 1_000_000_000) / 1000 ≫ 300`, so the shard would
///    re-demote.  With PR-f, `last_query_at_ms ≈ unix_now_ms()` after the
///    promote, so `idle_secs ≈ 0` and the shard stays Warm.
#[tokio::test]
async fn promote_resets_idle_clock_against_thrash() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let body = Arc::new(build_test_drive());
    let loader = Arc::new(FixedBodyLoader {
        body: Arc::clone(&body),
    });
    let mgr = IndexManager::with_body_loader_for_test(None, tx, loader);
    mgr.add_drive(build_test_drive()).await;

    // ── Step 1–2: Park C and backdate `last_query_at_ms` to
    // simulate a shard that has been Parked for a very long time
    // (the production thrash precondition).
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );
    let ancient_ts_ms = 1_000_000_000_u64; // 2001-09-09T01:46:40Z
    assert!(
        mgr.backdate_last_query_at_ms_for_test(uffs_mft::platform::DriveLetter::C, ancient_ts_ms)
            .await,
        "test fixture must be able to backdate last_query_at_ms",
    );

    // ── Step 3: Promote.  PR-f bumps `last_query_at_ms` to
    // ~`unix_now_ms()` inside the registry write-swap.
    mgr.ensure_warm_for_dispatch(&[uffs_mft::platform::DriveLetter::C], &[])
        .await;
    assert_eq!(
        mgr.shard_states_for_test().await,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Warm)],
        "ensure_warm_for_dispatch must promote Parked → Warm",
    );

    // ── Step 4: Idle tick at "real now".  Without PR-f the demote
    // controller sees the still-stale ancient_ts_ms → idle_secs
    // ~25 years → re-demote.  With PR-f the promote refreshed the
    // timestamp → idle_secs ≈ 0 → no demote.
    let now_ms = crate::cache::unix_now_ms();
    mgr.demote_idle_shards(now_ms).await;

    // ── Step 5: Assert no re-demote.  Pre-PR-f this assertion
    // fails: states_post_tick == [(uffs_mft::platform::DriveLetter::C,
    // Parked)]. Post-PR-f the shard stays Warm.
    assert_eq!(
        mgr.shard_states_for_test().await,
        vec![(uffs_mft::platform::DriveLetter::C, ShardState::Warm)],
        "promote must refresh `last_query_at_ms` so the next idle \
         tick sees a fresh idle clock; without the PR-f \
         `mark_loaded_at(now_ms)` bump, this re-demotes immediately \
         (the v0.5.85 Windows soak thrash captured at \
         `LOG/windows 0.5.85` lines 5540 → 5733)",
    );
}
