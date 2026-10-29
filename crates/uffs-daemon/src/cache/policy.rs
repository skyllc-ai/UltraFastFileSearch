// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tier-transition policy for the demote/promote controller.
//!
//! Phase 3 (the static fallback) and Phase 6 (the adaptive layer) of
//! the memory-tiering work
//! (`docs/refactor/memory-tiering-implementation-plan.md`).
//!
//! ## Static defaults (Phase 3)
//!
//! Without a `daemon.toml` and without env-var overrides, every
//! shard demotes through the same fixed ladder:
//!
//! * **Hot → Warm** at [`HOT_TO_WARM_IDLE_SECS`] (1 min idle, default).
//! * **Warm → Parked** at [`WARM_TO_PARKED_IDLE_SECS`] (5 min idle, default).
//! * **Parked → Cold** at [`PARKED_TO_COLD_IDLE_SECS`] (24 h idle, default).
//!
//! These defaults give a fast Hot → Parked test cycle (~6 min) so
//! the Phase 4 re-promote path is exercisable during normal dev
//! iteration without dragging in `UFFS_*_IDLE_SECS` env-var
//! overrides on every launch.  Production users who want longer
//! retention windows can still set the overrides; the env-var
//! pathway is unchanged.
//!
//! Each default is overridable at daemon startup via env var.
//! The override is read once and cached; restart the daemon to
//! pick up a change.
//!
//! ```text
//! UFFS_HOT_TO_WARM_IDLE_SECS=60      \
//! UFFS_WARM_TO_PARKED_IDLE_SECS=360  \
//! UFFS_PARKED_TO_COLD_IDLE_SECS=900  \
//!     uffs --daemon start --drive C,D,E,F,G,M,S
//! ```
//!
//! ## Adaptive layer (Phase 6)
//!
//! The Phase 6 [`hot_ttl`] / [`warm_ttl`] / [`parked_ttl`] functions
//! return a per-shard `Duration` whose floor is the configured
//! `*_ttl_base_secs` and whose bonus scales with the shard's
//! query-rate EMA (`queries / minute`, half-life 60 s on the
//! `DriveStats` side; converted from queries/sec at the call site
//! by multiplying by 60).  The plan §5.2 formulas:
//!
//! * `hot_ttl   = (base + 60·log2(rate)).clamp(base, cap)` — a hot shard with
//!   sustained traffic stays hot longer, capped at 1 hr.
//! * `warm_ttl  = (base + 600·log2(rate)).clamp(base, cap)` — a warm shard with
//!   steady access keeps its body resident longer.
//! * `parked_ttl` — no rate dependence; the parked → cold edge is a wall-clock
//!   TTL because a shard that's been idle 24 h is idle regardless of whether it
//!   once had a high query rate.
//!
//! `log2(rate)` uses `max(0.0)` so a rate < 1 q/min collapses the
//! bonus to zero (floor ≡ base).  The `cap` ceiling protects
//! against pathological rates and is reached only at astronomically
//! high inputs (`rate ≈ 2^55` for the hot tier with 5-min base /
//! 1-hr cap).  See the unit tests in this module for the canonical
//! reference points.
//!
//! ## Decision function
//!
//! [`next_state_for_idle_with_thresholds`] is the single decision
//! function consumed by
//! `crate::cache::registry::ShardRegistry::demote_idle_shards`; it
//! takes the demote-edge thresholds as an explicit
//! [`TierThresholds`] struct so the demote controller can size them
//! per-drive from the live rate EMA + the user's `daemon.toml`
//! `[tiers]` section.  Returns `None` when the shard is not yet
//! idle past its current tier's TTL or when the tier is already at
//! the bottom.

use core::time::Duration;
use std::sync::OnceLock;

use super::shard::ShardState;

/// Default for the `Hot` → `Warm` idle threshold.
///
/// Overridable at daemon startup via [`HOT_TO_WARM_IDLE_ENV`].
/// Phase 4+ extends "no query" to "no bloom-positive query" so cold
/// drives don't re-warm just because their bloom got faulted in by
/// a wildcard scan.
///
/// 10 min (600 s) default — enough time to cover typical interactive
/// usage bursts without demoting mid-session.  The runtime-mmap
/// rebuild on the next query is sub-millisecond so the demotion cost
/// is negligible when it does fire.
///
/// Consumed by [`crate::index::IndexManager::demote_idle_shards`]
/// (Phase 3 Commit D) via [`next_state_for_idle_with_thresholds`]
/// — Phase 6 Commit C feeds it adaptive thresholds derived from
/// this constant via `crate::config::TiersConfig::default()`.
pub(crate) const HOT_TO_WARM_IDLE_SECS: u64 = 600;

/// Default for the `Warm` → `Parked` idle threshold.
///
/// Overridable at daemon startup via [`WARM_TO_PARKED_IDLE_ENV`].
/// `Parked` drops the runtime mmap (the records / names columns
/// are released; bloom + trie persist for Phase 4+ search-skip).
///
/// 30 min (1 800 s) default — keeps the body resident across typical
/// human-scale gaps in usage (lunch break, context switch) while
/// still freeing memory on genuinely idle drives.  The bloom + trie
/// pre-check from Commit F means a Parked re-promote only
/// re-decrypts the compact body when the query actually hits the
/// drive, so the cost of a miss is bounded by query pattern.
pub(crate) const WARM_TO_PARKED_IDLE_SECS: u64 = 1_800;

/// Default for the `Parked` → `Cold` idle threshold.
///
/// Overridable at daemon startup via [`PARKED_TO_COLD_IDLE_ENV`].
/// `Cold` drops bloom + trie too — re-promotion requires a full
/// re-decrypt of the encrypted compact cache, so the threshold is
/// generous (24 h) by default.  Anything shorter risks thrashing
/// under nightly batch processes that scan archives once per day.
pub(crate) const PARKED_TO_COLD_IDLE_SECS: u64 = 86_400;

/// Default cadence for the Phase 5 (#95) background USN refresh
/// controller.  Every `USN_REFRESH_INTERVAL_SECS` (default 5 min)
/// the daemon walks all `Warm` / `Hot` shards and folds live USN
/// journal deltas into their in-memory body so search results
/// reflect the live filesystem state without waiting for the next
/// idle-tier transition.
///
/// 5 min is a deliberate trade-off: short enough that `Warm` shards
/// accumulate at most ~5 min of filesystem churn before a refresh,
/// long enough that the USN journal builds a meaningful batch
/// (per-drive replay is dominated by fixed setup cost, not record
/// count).  Override at daemon startup via [`USN_REFRESH_INTERVAL_ENV`].
pub(crate) const USN_REFRESH_INTERVAL_SECS: u64 = 300;

/// Env var that overrides [`HOT_TO_WARM_IDLE_SECS`].
pub(crate) const HOT_TO_WARM_IDLE_ENV: &str = "UFFS_HOT_TO_WARM_IDLE_SECS";

/// Env var that overrides [`WARM_TO_PARKED_IDLE_SECS`].
pub(crate) const WARM_TO_PARKED_IDLE_ENV: &str = "UFFS_WARM_TO_PARKED_IDLE_SECS";

/// Env var that overrides [`PARKED_TO_COLD_IDLE_SECS`].
pub(crate) const PARKED_TO_COLD_IDLE_ENV: &str = "UFFS_PARKED_TO_COLD_IDLE_SECS";

/// Env var that overrides [`USN_REFRESH_INTERVAL_SECS`].
pub(crate) const USN_REFRESH_INTERVAL_ENV: &str = "UFFS_USN_REFRESH_INTERVAL_SECS";

/// Default per-search scan budget, in seconds (see
/// [`search_scan_budget_secs`]).
///
/// A scan that outlives it is cancelled cooperatively (every per-drive
/// loop polls `SearchFilters::cancel`) and the client gets a JSON-RPC
/// `ERR_SEARCH_TIMEOUT` error — never a success-shaped empty result.
/// 30 s covers a full `*.*` over ~45 M records on the reference box
/// with headroom; the budget is deliberately far below the client's own
/// `UFFS_CLIENT_TIMEOUT_SECS` deadline (600 s default) so the error reaches
/// the client instead of the client giving up first and orphaning the scan.
/// Paging parked drives back in happens before the scan and does not count
/// against this budget.
pub(crate) const SEARCH_SCAN_BUDGET_SECS: u64 = 30;

/// Env var that overrides [`SEARCH_SCAN_BUDGET_SECS`].
pub(crate) const SEARCH_SCAN_BUDGET_ENV: &str = "UFFS_SEARCH_TIMEOUT_SECS";

/// Read a positive `u64` seconds value from `env_name`, falling back
/// to `default` on any parse error or non-positive value.  Logs a
/// single startup line per override so the effective policy is
/// observable in production.
fn read_env_secs(env_name: &str, default: u64) -> u64 {
    let Ok(raw) = std::env::var(env_name) else {
        return default;
    };
    let parsed: Option<u64> = raw.trim().parse::<u64>().ok().filter(|&n| n > 0);
    let effective = parsed.unwrap_or(default);
    if parsed.is_some() {
        tracing::info!(
            target: "shard.policy",
            env_var = env_name,
            override_secs = effective,
            default_secs = default,
            "policy override active",
        );
    } else {
        tracing::warn!(
            target: "shard.policy",
            env_var = env_name,
            raw = %raw,
            default_secs = default,
            "policy env var unparseable; using default",
        );
    }
    effective
}

/// Effective per-search scan budget (env override or default), in
/// seconds.  Read once; the first search caches it for the daemon's
/// lifetime like the idle thresholds.
#[must_use]
pub(crate) fn search_scan_budget_secs() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| read_env_secs(SEARCH_SCAN_BUDGET_ENV, SEARCH_SCAN_BUDGET_SECS))
}

/// Effective `Hot` → `Warm` idle threshold (env override or default).
#[must_use]
pub(crate) fn hot_to_warm_idle_secs() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| read_env_secs(HOT_TO_WARM_IDLE_ENV, HOT_TO_WARM_IDLE_SECS))
}

/// Effective `Warm` → `Parked` idle threshold (env override or default).
#[must_use]
pub(crate) fn warm_to_parked_idle_secs() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| read_env_secs(WARM_TO_PARKED_IDLE_ENV, WARM_TO_PARKED_IDLE_SECS))
}

/// Effective `Parked` → `Cold` idle threshold (env override or default).
#[must_use]
pub(crate) fn parked_to_cold_idle_secs() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| read_env_secs(PARKED_TO_COLD_IDLE_ENV, PARKED_TO_COLD_IDLE_SECS))
}

/// Effective USN refresh interval (env override or default), in
/// seconds.  Consumed by the Phase 5 (#95) background USN refresh
/// controller spawned from `crate::lib::run_daemon`.
#[must_use]
pub(crate) fn usn_refresh_interval_secs() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| read_env_secs(USN_REFRESH_INTERVAL_ENV, USN_REFRESH_INTERVAL_SECS))
}

/// The three demote-edge thresholds, in seconds, that the
/// controller uses to decide whether a shard should drop a tier.
///
/// Phase 3 used three module-level `OnceLock` cached env-var
/// reads.  Phase 6 makes the thresholds per-shard adaptive (sized
/// by the live query-rate EMA + the user's `daemon.toml`), so the
/// decision function takes the thresholds as data rather than
/// reading them through global state.  The Phase 3 static
/// behaviour is preserved by `crate::config::TiersConfig::default()`
/// (env-var-aware) and is exercised by the unit tests in this
/// module via the test-only `phase3_static_thresholds()` helper.
///
/// All three fields are seconds (`u64`) for direct comparison
/// against `idle_secs = (now_ms - last_query_at_ms) / 1000`; the
/// shared `_secs` postfix is the unit, not name redundancy, hence
/// the `clippy::struct_field_names` expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "the `_secs` postfix is the unit (seconds) carried in every \
              field; renaming away from it would obscure the contract \
              that fields are directly comparable to `idle_secs`."
)]
pub(crate) struct TierThresholds {
    /// Demote edge for `Hot` → `Warm`.
    pub hot_to_warm_secs: u64,
    /// Demote edge for `Warm` → `Parked`.
    pub warm_to_parked_secs: u64,
    /// Demote edge for `Parked` → `Cold`.
    pub parked_to_cold_secs: u64,
}

/// Phase 6 adaptive `Hot` → `Warm` TTL.
///
/// Plan §5.2 formula: `(base + 60·log2(rate)).clamp(base, cap)`,
/// where `rate` is the per-drive query-rate EMA in queries / minute.
/// `log2(rate).max(0.0)` collapses the bonus to zero when
/// `rate < 1 q/min`, so an idle drive's TTL stays at the floor.
///
/// At the canonical reference points (default `base = 60 s`,
/// `cap = 3600 s`):
///
/// * `rate = 0`     → 60 s (floor).
/// * `rate = 20`    → 60 + 60·log2(20)  ≈ 60 + 259 ≈ 319 s (~5.3 min).
/// * `rate = 1 200` → 60 + 60·log2(1200) ≈ 60 + 614 ≈ 674 s (~11.2 min).
/// * `rate → ∞`     → 3 600 s (cap; reached at `rate ≈ 2^59`, astronomically
///   high — the cap is a guard, not a steady-state).
///
/// Plan §3 task 6.4's "`rate_ema` = 20 → ≈ 14 min" was an
/// illustrative round-number; the literal §5.2 formula gives ~5.3
/// min at the same input.  The unit tests in this module pin the
/// formula's actual output, not the task description's
/// approximation.
#[must_use]
#[expect(
    clippy::float_arithmetic,
    reason = "TTL formula uses log2 + multiply + clamp on a non-negative \
              rate; precision loss bounded by `Duration::from_secs_f64` \
              and the explicit `cap` ceiling."
)]
pub(crate) fn hot_ttl(rate_ema_qpm: f64, base_secs: u64, cap_secs: u64) -> Duration {
    const HOT_BONUS_COEF_SECS: f64 = 60.0;
    let bonus_secs = HOT_BONUS_COEF_SECS * rate_ema_qpm.log2().max(0.0);
    // All `u64 -> f64` conversions go through `uffs_mft::u64_to_f64`
    // (centralized `cast_precision_loss` expect at the helper site).
    let base_f64 = uffs_mft::u64_to_f64(base_secs);
    let total_secs = base_f64 + bonus_secs;
    let capped_secs = total_secs.min(uffs_mft::u64_to_f64(cap_secs)).max(base_f64);
    Duration::from_secs_f64(capped_secs)
}

/// Phase 6 adaptive `Warm` → `Parked` TTL.
///
/// Plan §5.2 formula: `(base + 600·log2(rate)).clamp(base, cap)`.
/// Larger coefficient than [`hot_ttl`] because warm shards are
/// expected to have less bursty traffic — when they do see steady
/// access, the body is worth keeping resident for longer.
///
/// At the canonical reference points (default `base = 300 s`,
/// `cap = 14 400 s`):
///
/// * `rate = 0`     → 300 s (floor; 5 min).
/// * `rate = 4`     → 300 + 600·log2(4)  = 300 + 1 200 = 1 500 s (25 min).
/// * `rate = 1 024` → 300 + 600·log2(1024) = 300 + 6 000 = 6 300 s (105 min).
/// * `rate → ∞`     → 14 400 s (cap; 4 hr).
#[must_use]
#[expect(
    clippy::float_arithmetic,
    reason = "TTL formula uses log2 + multiply + clamp on a non-negative \
              rate; precision loss bounded by `Duration::from_secs_f64` \
              and the explicit `cap` ceiling."
)]
pub(crate) fn warm_ttl(rate_ema_qpm: f64, base_secs: u64, cap_secs: u64) -> Duration {
    const WARM_BONUS_COEF_SECS: f64 = 600.0;
    let bonus_secs = WARM_BONUS_COEF_SECS * rate_ema_qpm.log2().max(0.0);
    let base_f64 = uffs_mft::u64_to_f64(base_secs);
    let total_secs = base_f64 + bonus_secs;
    let capped_secs = total_secs.min(uffs_mft::u64_to_f64(cap_secs)).max(base_f64);
    Duration::from_secs_f64(capped_secs)
}

/// Phase 6 `Parked` → `Cold` TTL.
///
/// No rate dependence: a shard that's been idle 24 h is idle
/// regardless of whether it once had a high query rate.  Returned
/// as a `Duration` for symmetry with [`hot_ttl`] / [`warm_ttl`] so
/// the demote controller can size all three thresholds in one
/// pass.
#[must_use]
pub(crate) const fn parked_ttl(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// Decide whether a shard in `state` that has been idle for
/// `idle_secs` seconds should demote to a colder tier given the
/// adaptive `thresholds`.
///
/// Pure decision function: takes the three demote-edge thresholds
/// as data rather than reading them through `OnceLock`-cached
/// global state, so the demote controller can size each
/// `(drive, state)` pair independently from the live rate EMA.
///
/// Returns the target state on demote, or `None` when:
///
/// * the shard has not been idle long enough for its current tier, or
/// * the shard is already at the bottom of the tier ladder
///   ([`ShardState::Cold`], [`ShardState::Unknown`], [`ShardState::Evicting`])
///   — only the controller produces those states, the policy never selects them
///   as a *target*.
///
/// `Hot` skips straight to `Warm` rather than `Parked` so a brief
/// idle window doesn't re-cost a runtime-mmap rebuild on the next
/// query.
///
/// Phase 6's `crate::index::IndexManager::demote_idle_shards`
/// calls this function directly with per-shard thresholds derived
/// from the live rate EMA + the user's `daemon.toml`; the unit
/// tests in this module wrap it via `next_state_for_idle`
/// (test-only) for the static-policy boundary checks.
#[must_use]
pub(crate) const fn next_state_for_idle_with_thresholds(
    state: ShardState,
    idle_secs: u64,
    thresholds: &TierThresholds,
) -> Option<ShardState> {
    match state {
        ShardState::Hot if idle_secs >= thresholds.hot_to_warm_secs => Some(ShardState::Warm),
        ShardState::Warm if idle_secs >= thresholds.warm_to_parked_secs => Some(ShardState::Parked),
        ShardState::Parked if idle_secs >= thresholds.parked_to_cold_secs => Some(ShardState::Cold),
        // Two distinct cases collapsed into one arm because they
        // share the same outcome:
        //   * Hot / Warm / Parked: idle window not exceeded for the current
        //     tier (the guard above this arm failed).
        //   * Cold / Unknown / Evicting: floor + controller-only states; the
        //     policy never targets these.
        ShardState::Hot
        | ShardState::Warm
        | ShardState::Parked
        | ShardState::Cold
        | ShardState::Unknown
        | ShardState::Evicting => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase-3 static thresholds derived from the env-var-aware
    /// getters — the `OnceLock`-cached values that
    /// `crate::config::TiersConfig::default()` uses to seed the
    /// daemon's adaptive ladder when no `daemon.toml` is present.
    /// Test-only so the production module can stay focused on the
    /// adaptive helpers wired by Phase 6 Commit C.
    fn phase3_static_thresholds() -> TierThresholds {
        TierThresholds {
            hot_to_warm_secs: hot_to_warm_idle_secs(),
            warm_to_parked_secs: warm_to_parked_idle_secs(),
            parked_to_cold_secs: parked_to_cold_idle_secs(),
        }
    }

    /// Single-arg test wrapper around
    /// [`next_state_for_idle_with_thresholds`] that pins the
    /// Phase-3 static-policy thresholds.  Lets the boundary tests
    /// below stay readable (`next_state_for_idle(state, secs)`)
    /// without re-stating the threshold struct on every call.
    fn next_state_for_idle(state: ShardState, idle_secs: u64) -> Option<ShardState> {
        next_state_for_idle_with_thresholds(state, idle_secs, &phase3_static_thresholds())
    }

    #[test]
    fn hot_demotes_to_warm_at_threshold() {
        assert_eq!(
            next_state_for_idle(ShardState::Hot, HOT_TO_WARM_IDLE_SECS),
            Some(ShardState::Warm)
        );
        assert_eq!(
            next_state_for_idle(ShardState::Hot, HOT_TO_WARM_IDLE_SECS + 1),
            Some(ShardState::Warm)
        );
    }

    #[test]
    fn hot_does_not_demote_below_threshold() {
        assert_eq!(next_state_for_idle(ShardState::Hot, 0), None);
        assert_eq!(
            next_state_for_idle(ShardState::Hot, HOT_TO_WARM_IDLE_SECS - 1),
            None
        );
    }

    #[test]
    fn warm_demotes_to_parked_at_threshold() {
        assert_eq!(
            next_state_for_idle(ShardState::Warm, WARM_TO_PARKED_IDLE_SECS),
            Some(ShardState::Parked)
        );
    }

    #[test]
    fn warm_does_not_demote_below_threshold() {
        // Crucially below the warm-to-parked threshold even though
        // it's already past the hot-to-warm one — `Warm` shards don't
        // care about the hot threshold.
        assert_eq!(
            next_state_for_idle(ShardState::Warm, HOT_TO_WARM_IDLE_SECS),
            None
        );
        assert_eq!(
            next_state_for_idle(ShardState::Warm, WARM_TO_PARKED_IDLE_SECS - 1),
            None
        );
    }

    #[test]
    fn parked_demotes_to_cold_at_threshold() {
        assert_eq!(
            next_state_for_idle(ShardState::Parked, PARKED_TO_COLD_IDLE_SECS),
            Some(ShardState::Cold)
        );
    }

    #[test]
    fn parked_does_not_demote_below_threshold() {
        assert_eq!(
            next_state_for_idle(ShardState::Parked, WARM_TO_PARKED_IDLE_SECS),
            None
        );
        assert_eq!(
            next_state_for_idle(ShardState::Parked, PARKED_TO_COLD_IDLE_SECS - 1),
            None
        );
    }

    #[test]
    fn floor_states_never_demote() {
        for state in [ShardState::Cold, ShardState::Unknown, ShardState::Evicting] {
            for idle_secs in [
                0,
                HOT_TO_WARM_IDLE_SECS,
                WARM_TO_PARKED_IDLE_SECS,
                PARKED_TO_COLD_IDLE_SECS,
                u64::MAX,
            ] {
                assert_eq!(
                    next_state_for_idle(state, idle_secs),
                    None,
                    "{state:?} at {idle_secs}s should never demote"
                );
            }
        }
    }

    /// Pin the ladder so a future tweak can't accidentally make
    /// `PARKED_TO_COLD_IDLE_SECS` shorter than
    /// `WARM_TO_PARKED_IDLE_SECS`, which would mean a parked shard
    /// could demote to Cold faster than a warm one demotes to
    /// Parked.  Compile-time `const _: ()` so the invariant is
    /// enforced at build time, not at test run.
    const _: () = assert!(HOT_TO_WARM_IDLE_SECS < WARM_TO_PARKED_IDLE_SECS);
    const _: () = assert!(WARM_TO_PARKED_IDLE_SECS < PARKED_TO_COLD_IDLE_SECS);

    /// `read_env_secs` falls back to the supplied default when the
    /// env var is unset.  Pins the contract that a non-overridden
    /// daemon uses the in-source thresholds.  Uses a deliberately
    /// unique env-var name (`UFFS_TEST_ENV_NEVER_SET`) so the test
    /// is independent of any `UFFS_*_IDLE_SECS` value the developer
    /// happens to have exported in their shell.
    #[test]
    fn read_env_secs_falls_back_when_env_unset() {
        let val = read_env_secs("UFFS_TEST_ENV_NEVER_SET_12345", 42);
        assert_eq!(val, 42, "unset env var → default");
    }

    // ── Phase 6 — adaptive TTL formulas (plan task 6.1, 6.4) ──────

    /// Phase 3 default thresholds packaged as a [`TierThresholds`]
    /// — the shape Phase 6's adaptive ladder collapses to when
    /// `rate_ema = 0` (no traffic) AND the user's `daemon.toml`
    /// is missing.  Pins task 6.8 ("missing `daemon.toml` →
    /// defaults match Phase 3 static behavior").
    const PHASE3_DEFAULT_THRESHOLDS: TierThresholds = TierThresholds {
        hot_to_warm_secs: HOT_TO_WARM_IDLE_SECS,
        warm_to_parked_secs: WARM_TO_PARKED_IDLE_SECS,
        parked_to_cold_secs: PARKED_TO_COLD_IDLE_SECS,
    };

    /// Floor: a drive with zero rate EMA (no recent traffic) must
    /// give back exactly the configured `base_secs`, not a value
    /// nudged by `log2(0) = -∞`.  The formula's `.max(0.0)` clamp
    /// is what makes this work — exercised here because a
    /// regression on that clamp would produce `Duration::MAX` or
    /// similar nonsense rather than a clean floor.
    #[test]
    fn hot_ttl_at_zero_rate_returns_base() {
        assert_eq!(hot_ttl(0.0, 60, 3600), Duration::from_mins(1));
        assert_eq!(hot_ttl(0.5, 60, 3600), Duration::from_mins(1));
        assert_eq!(hot_ttl(1.0, 60, 3600), Duration::from_mins(1));
    }

    /// Plan task 6.4 reference point at `rate_ema = 20 q/min`.
    /// The literal §5.2 formula gives `60 + 60·log2(20) ≈ 60 +
    /// 259.3 ≈ 319.3 s`, which rounds to 319 s as a `Duration`.
    /// The plan task description's "≈ 14 min" was an illustrative
    /// round-number; this test pins the actual formula output.
    #[test]
    fn hot_ttl_at_rate_20_qpm_matches_formula() {
        let ttl = hot_ttl(20.0, 60, 3600);
        // 60 + 60*log2(20) = 60 + 259.31568... = 319.31568...
        let secs = ttl.as_secs_f64();
        assert!(
            (319.0_f64..=320.0_f64).contains(&secs),
            "hot_ttl(20 q/min) = {secs:.3} s; expected ≈ 319.32 s",
        );
    }

    /// Cap: a pathologically high rate must not produce a TTL
    /// longer than the configured ceiling.  Exercise at
    /// `rate = f64::MAX`, where `log2(f64::MAX)` is finite (~1024)
    /// but the bonus `60 × 1024 = 61 440 s` would blow past the
    /// 1 hr cap if the `.min(cap)` clamp regressed.
    #[test]
    fn hot_ttl_above_cap_returns_cap() {
        assert_eq!(
            hot_ttl(f64::MAX, 60, 3600),
            Duration::from_hours(1),
            "hot_ttl at f64::MAX rate must clamp to the cap",
        );
        // Realistic cap-hit: `log2(rate) > (cap - base) / coef = 59`
        // → `rate > 2^59 ≈ 5.76e17`.  Use a slightly higher number
        // to make the cap-hit unambiguous.
        assert_eq!(hot_ttl(2.0_f64.powi(60), 60, 3600), Duration::from_hours(1),);
    }

    /// `warm_ttl` mirrors `hot_ttl` but with the heavier `600·`
    /// coefficient.  At `rate = 4 q/min`, `log2(4) = 2` so
    /// `bonus = 600 × 2 = 1 200 s` and `total = 300 + 1 200 = 1 500
    /// s` (25 min) — a clean integer reference point.
    #[test]
    fn warm_ttl_at_rate_4_qpm_matches_formula() {
        let ttl = warm_ttl(4.0, 300, 14_400);
        assert_eq!(ttl, Duration::from_mins(25));
    }

    /// `warm_ttl` floor at zero / sub-1 rate.
    #[test]
    fn warm_ttl_at_zero_rate_returns_base() {
        assert_eq!(warm_ttl(0.0, 300, 14_400), Duration::from_mins(5));
        assert_eq!(warm_ttl(1.0, 300, 14_400), Duration::from_mins(5));
    }

    /// `warm_ttl` cap clamp.
    #[test]
    fn warm_ttl_above_cap_returns_cap() {
        assert_eq!(warm_ttl(f64::MAX, 300, 14_400), Duration::from_hours(4));
    }

    /// `parked_ttl` is a pure pass-through; pin the contract
    /// (no rate dependence, no clamping) so a future "let's add
    /// adaptivity here too" tweak has to update this test
    /// deliberately.
    #[test]
    fn parked_ttl_is_a_pass_through() {
        assert_eq!(parked_ttl(0), Duration::ZERO);
        assert_eq!(parked_ttl(86_400), Duration::from_hours(24));
        assert_eq!(parked_ttl(u64::MAX), Duration::from_secs(u64::MAX));
    }

    // ── Phase 6 — `next_state_for_idle_with_thresholds` ───────────

    /// `next_state_for_idle_with_thresholds` at the Phase-3
    /// defaults must agree with the legacy `next_state_for_idle`
    /// at every threshold boundary.  Pins task 6.8 ("missing
    /// `daemon.toml` → defaults match Phase 3 static behavior")
    /// at the decision-function level.
    #[test]
    fn next_state_with_phase3_thresholds_matches_legacy() {
        for state in [
            ShardState::Hot,
            ShardState::Warm,
            ShardState::Parked,
            ShardState::Cold,
        ] {
            for idle_secs in [
                0,
                HOT_TO_WARM_IDLE_SECS - 1,
                HOT_TO_WARM_IDLE_SECS,
                WARM_TO_PARKED_IDLE_SECS - 1,
                WARM_TO_PARKED_IDLE_SECS,
                PARKED_TO_COLD_IDLE_SECS - 1,
                PARKED_TO_COLD_IDLE_SECS,
                u64::MAX,
            ] {
                assert_eq!(
                    next_state_for_idle_with_thresholds(
                        state,
                        idle_secs,
                        &PHASE3_DEFAULT_THRESHOLDS
                    ),
                    next_state_for_idle(state, idle_secs),
                    "divergence at state={state:?} idle_secs={idle_secs}",
                );
            }
        }
    }

    /// A short-TTL `TierThresholds` (1 / 2 / 3 secs) demonstrates
    /// that the adaptive variant respects per-call thresholds
    /// rather than the cached Phase-3 statics.  Belt-and-braces
    /// guard against accidentally reading the global `OnceLock`s
    /// in the new code path.
    #[test]
    fn next_state_with_thresholds_respects_per_call_values() {
        let aggressive = TierThresholds {
            hot_to_warm_secs: 1,
            warm_to_parked_secs: 2,
            parked_to_cold_secs: 3,
        };
        assert_eq!(
            next_state_for_idle_with_thresholds(ShardState::Hot, 1, &aggressive),
            Some(ShardState::Warm),
        );
        assert_eq!(
            next_state_for_idle_with_thresholds(ShardState::Warm, 2, &aggressive),
            Some(ShardState::Parked),
        );
        assert_eq!(
            next_state_for_idle_with_thresholds(ShardState::Parked, 3, &aggressive),
            Some(ShardState::Cold),
        );
        assert_eq!(
            next_state_for_idle_with_thresholds(ShardState::Hot, 0, &aggressive),
            None,
        );
    }
}
