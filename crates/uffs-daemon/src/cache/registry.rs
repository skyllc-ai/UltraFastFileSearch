// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! [`ShardRegistry`] — top-level container that replaces the old
//! `Arc<DriveIndex>` field on [`crate::index::IndexManager`].
//!
//! See [`crate::cache`] module docs for the bigger picture.

use alloc::sync::Arc;
use core::fmt;

use uffs_core::compact::DriveCompactIndex;
use uffs_core::search::backend::DriveIndex;

use super::shard::{ShardEntry, ShardState};

/// Why a [`ShardRegistry::demote_letter_with_reason`] call was issued.
///
/// The registry primitive uses this to populate the `reason` field of
/// the canonical `shard.transition` `INFO` event so operators can
/// distinguish TTL-driven idle demotes from operator hibernation by
/// grepping a single field.
///
/// Wire format (must stay stable — operator runbooks grep for these
/// exact strings):
///
/// * [`Self::IdleTtl`] → `reason="demote"` (default).  Preserved as `"demote"`
///   rather than `"idle-ttl"` for backwards compatibility with existing
///   operator runbooks and the Phase 3 task 3.9 observability contract test
///   (`shard_transition_events_emitted_on_demote_and_promote`).
/// * [`Self::OperatorHibernate`] → `reason="operator-hibernate"`.
///
/// `reason="pressure-cascade"` existed until 2026-10-03; the kernel-Low
/// pressure cascade that emitted it was removed by owner ruling (memory
/// pressure is logged, never acted on), so no current build emits it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum DemoteReason {
    /// TTL-driven idle demote — the per-tier `_ttl_secs` config has
    /// elapsed since the last query against this shard.  Emitted by
    /// [`crate::index::IndexManager::demote_idle_shards`].
    IdleTtl,
    /// Operator-driven `hibernate` RPC.  Emitted once per shard
    /// inside the
    /// [`crate::index::IndexManager::hibernate_shards`] write-lock
    /// batch (Phase 8-B).  Distinguishable from the controller-
    /// driven [`Self::IdleTtl`] path by `reason="operator-hibernate"`,
    /// so operator audit logs can separate manual hibernation from
    /// automatic demote activity.
    OperatorHibernate,
}

impl DemoteReason {
    /// The wire string that ends up in the `reason=` field of the
    /// canonical `shard.transition` `INFO` event.
    ///
    /// Used directly via `reason = reason.as_str()` in the tracing
    /// macro (rather than via `%reason` Display formatting) so the
    /// `tracing-subscriber` default formatter routes the value
    /// through `record_str` → Debug-formatted-string → **quoted**
    /// output (`reason="operator-hibernate"`).  The Display path
    /// (`%`) goes through `record_debug` with `format_args`, which
    /// renders the value **unquoted** (`reason=operator-hibernate`)
    /// — incompatible with the legacy operator runbook regexes
    /// authored against `reason="demote"` and `reason="usn-refresh"`.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::IdleTtl => "demote",
            Self::OperatorHibernate => "operator-hibernate",
        }
    }
}

impl fmt::Display for DemoteReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Predicate: can a shard in `from` legally demote to `target`?
///
/// Looser than [`ShardState::can_transition_to`] because the
/// registry-level rebuild in
/// [`ShardRegistry::demote_letter`] is the linearisation point — no
/// stable `Arc<ShardEntry>` reader ever observes an in-place state
/// mutation, so the strict per-`ShardEntry` graph used by the
/// test-only `try_transition` CAS path doesn't apply.
///
/// Demote target must be `Parked` or `Cold`.  Self-demotes
/// (`Parked → Parked`, `Cold → Cold`) are intentionally rejected so
/// a buggy controller can't silently rebuild the registry on every
/// idle tick for an already-demoted shard.
const fn is_legal_demote_target(from: ShardState, target: ShardState) -> bool {
    // Single match arm so clippy's `match_same_arms` is satisfied:
    //
    //   * Hot / Warm: demote to Parked OR Cold.
    //   * Parked: demote to Cold only (the bloom/trie drop step in Phase 4+; no
    //     body to drop here).
    //
    // Everything else (Cold/Cold, Unknown/*, Evicting/*, all
    // self-demotes) falls through to `false`.
    matches!(
        (from, target),
        (
            ShardState::Hot | ShardState::Warm,
            ShardState::Parked | ShardState::Cold
        ) | (ShardState::Parked, ShardState::Cold)
    )
}

/// Registry of per-drive shards plus a cached `Arc<DriveIndex>` over
/// the active subset (Warm/Hot tiers).
///
/// The cached `active_index` makes [`Self::active_index`] an
/// `Arc::clone` — search dispatch reads it under the daemon's
/// `RwLock<Arc<ShardRegistry>>` and never builds a `Vec` per query.
/// Mutations (`add` / `replace` / `remove`) return a *new* registry
/// with the rebuilt active subset; the daemon swaps the outer `Arc`.
///
/// Phase 1 keeps every shard in [`ShardState::Warm`] so the active
/// subset always equals the full registry.  Phase 3 starts demoting,
/// at which point the active subset shrinks below the full shard list.
pub(crate) struct ShardRegistry {
    /// Every loaded shard, in load order.  `Arc<ShardEntry>` so a
    /// registry rebuild is a Vec clone of pointers, not bodies.
    shards: Vec<Arc<ShardEntry>>,
    /// Pre-computed `DriveIndex` over shards in [`ShardState::Warm`]
    /// or [`ShardState::Hot`].  Cheap to clone for search dispatch.
    active_index: Arc<DriveIndex>,
}

impl ShardRegistry {
    /// Empty registry — no shards, empty active index.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            shards: Vec::new(),
            active_index: Arc::new(DriveIndex::new()),
        }
    }

    /// Build a registry from an explicit shard vector, recomputing the
    /// active subset.
    ///
    /// Used internally by [`Self::add`], [`Self::replace`], and
    /// [`Self::remove`]; exposed for tests that want to seed a
    /// registry directly.
    #[must_use]
    pub(crate) fn from_shards(shards: Vec<Arc<ShardEntry>>) -> Self {
        // Filter on tier first so the active subset matches the
        // documented "Warm | Hot" contract; then filter_map on `body`
        // because Phase-3 `ShardEntry::body()` returns `Option` —
        // `Parked` / `Cold` shards lift the body and would yield
        // `None` here.  The double filter is intentionally redundant
        // (every `Warm` / `Hot` shard has `Some(body)` today) so a
        // future "Warm with body in transit" state can't silently
        // contribute an empty entry to the active index.
        let drives: Vec<Arc<DriveCompactIndex>> = shards
            .iter()
            .filter(|shard| matches!(shard.state(), ShardState::Warm | ShardState::Hot))
            .filter_map(|shard| shard.body())
            .collect();
        Self {
            shards,
            active_index: Arc::new(DriveIndex { drives }),
        }
    }

    /// Insert a fresh `Warm` shard for `body.letter` and return the
    /// rebuilt registry.  The previous registry is left untouched.
    ///
    /// The shard's identity is `body.letter` so callers don't have to
    /// thread the letter separately and can't accidentally store a
    /// shard whose letter disagrees with its body.
    #[must_use]
    pub(crate) fn add(&self, body: Arc<DriveCompactIndex>) -> Self {
        let letter = body.letter;
        let mut shards = self.shards.clone();
        shards.push(Arc::new(ShardEntry::new_warm(letter, body)));
        Self::from_shards(shards)
    }

    /// Replace the shard whose drive letter case-insensitively
    /// matches `match_letter` (if any) with a fresh `Warm` entry, and
    /// return the rebuilt registry.
    ///
    /// The new shard's identity is `body.letter` (canonical case from
    /// the index payload), preserving the pre-Phase-1 contract where
    /// `DriveIndex { drives: vec![Arc::new(new_drive)] }` always
    /// identified the new entry by `new_drive.letter`.  When no
    /// existing shard matches `match_letter`, this is equivalent to
    /// [`Self::add`].
    ///
    /// The match is **case-insensitive** to preserve the behavior of
    /// the pre-Phase-1 `IndexManager::replace_drive`, which used
    /// `eq_ignore_ascii_case` to handle drive letters that flow
    /// through the daemon in either case.
    #[must_use]
    pub(crate) fn replace(
        &self,
        match_letter: uffs_mft::platform::DriveLetter,
        body: Arc<DriveCompactIndex>,
    ) -> Self {
        let new_letter = body.letter;
        let mut shards: Vec<Arc<ShardEntry>> = self
            .shards
            .iter()
            .filter(|shard| shard.drive != match_letter)
            .cloned()
            .collect();
        shards.push(Arc::new(ShardEntry::new_warm(new_letter, body)));
        Self::from_shards(shards)
    }

    /// Drop the shard for `drive` (if any) and return the rebuilt
    /// registry.  No-op when `drive` is not in the registry.
    ///
    /// Match is case-insensitive — see [`Self::replace`] for the
    /// rationale.
    ///
    /// Phase 8-D `forget` activates this as a production consumer:
    /// [`crate::index::IndexManager::forget_drives`] calls `remove`
    /// after the eviction guard checks pass.
    #[must_use]
    pub(crate) fn remove(&self, drive: uffs_mft::platform::DriveLetter) -> Self {
        let shards: Vec<Arc<ShardEntry>> = self
            .shards
            .iter()
            .filter(|shard| shard.drive != drive)
            .cloned()
            .collect();
        Self::from_shards(shards)
    }

    /// Iterate over every shard in load order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<ShardEntry>> {
        self.shards.iter()
    }

    /// Snapshot of the active (`Warm`/`Hot`) subset as an
    /// `Arc<DriveIndex>` for the search backend.  Cheap clone.
    #[must_use]
    pub(crate) fn active_index(&self) -> Arc<DriveIndex> {
        Arc::clone(&self.active_index)
    }

    /// Total number of loaded shards (active + demoted).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Phase 3 consumer (status renderer with active vs demoted \
                      shard counts); exercised by registry tests under \
                      `cfg(test)`."
        )
    )]
    #[must_use]
    pub(crate) const fn len(&self) -> usize {
        self.shards.len()
    }

    /// Demote the shard for `letter` to `target` (`Parked` or
    /// `Cold`) with the default [`DemoteReason::IdleTtl`] reason.
    ///
    /// Convenience wrapper over [`Self::demote_letter_with_reason`]
    /// for the production idle-demote controller and every test that
    /// doesn't care about the exact `reason` field on the resulting
    /// `shard.transition` event.
    ///
    /// Wired into the production demote path by
    /// [`crate::index::IndexManager::demote_idle_shards`] (Phase 3
    /// Commit D).  Operator hibernation uses
    /// [`Self::demote_letter_with_reason`] directly so its events
    /// carry `reason="operator-hibernate"` instead of the default
    /// `reason="demote"`.
    #[must_use]
    pub(crate) fn demote_letter(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        target: ShardState,
    ) -> Option<Self> {
        self.demote_letter_with_reason(letter, target, DemoteReason::IdleTtl)
    }

    /// Demote the shard for `letter` to `target` (`Parked` or
    /// `Cold`), dropping the body and emitting a single
    /// `shard.transition` `INFO` event with the supplied `reason`
    /// in its `reason` field.  Returns the rebuilt registry, or
    /// `None` when:
    ///
    /// * `letter` is not registered;
    /// * `target` is not a demote-legal tier (must be `Parked` or `Cold`);
    /// * the existing shard's state is not a legal "from" for the requested
    ///   demote (see [`is_legal_demote_target`]).
    ///
    /// The per-drive `Arc<DriveStats>` is shared with the
    /// replacement shard so query counters survive the rebuild —
    /// Commit C's idle-timer relies on `last_query_at_ms` staying
    /// stable across demote/promote cycles.
    ///
    /// Registry-level rebuilds bypass the per-`ShardEntry`
    /// `can_transition_to` graph (which guards the test-only
    /// `try_transition` CAS path).  No stable `Arc<ShardEntry>`
    /// reader ever observes an in-place state mutation here: the
    /// caller's old `Arc` keeps reading the old state forever, the
    /// new `Arc` reads the new state forever, and the registry's
    /// `Vec` swap is the linearisation point.
    ///
    /// **Single canonical event.**  Every demote (TTL idle or
    /// operator hibernate) emits exactly one `INFO`-level
    /// `shard.transition` event from this method; the discriminator
    /// lives in its `reason` field and `last_query_at_ms` is included
    /// for every demote so operator runbooks get a uniform schema.
    #[must_use]
    pub(crate) fn demote_letter_with_reason(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        target: ShardState,
        reason: DemoteReason,
    ) -> Option<Self> {
        // Locate the matching shard by enumerating once: returns
        // `(pos, &Arc<ShardEntry>)` so we never index into
        // `self.shards` (clippy::indexing_slicing).
        let (pos, old_arc) = self
            .shards
            .iter()
            .enumerate()
            .find(|(_, shard)| shard.drive == letter)?;
        let from_state = old_arc.state();
        if !is_legal_demote_target(from_state, target) {
            return None;
        }
        // Compute the body heap we're about to release, for the
        // tracing event.  Done before constructing the new entry so
        // the read happens against the still-mounted body.
        let freed_mb = old_arc.body().map_or(0_u64, |body| {
            (body.heap_size_bytes().total / 1_048_576) as u64
        });
        // Capture the LRU timestamp before we rebuild — useful in the
        // canonical event so callers don't need to emit a
        // second event of their own just to log this field.
        let last_query_at_ms = old_arc.stats.last_query_at_ms();
        let stats = Arc::clone(&old_arc.stats);
        let drive = old_arc.drive;
        let new_entry = match target {
            ShardState::Parked => {
                // Phase 4 Commit F — extract the bloom + trie from the
                // existing full body so the parked shard can answer the
                // search-skip pre-check without re-loading from disk.
                // The legality check (`is_legal_demote_target`) only
                // permits `Hot | Warm` → `Parked`, both of which carry
                // a body, so the `body()` Option is `Some`.  An absent
                // body would indicate a torn registry; defend with a
                // log and skip the demote rather than panic.
                let Some(body) = old_arc.body() else {
                    tracing::error!(
                        target: "shard.transition",
                        letter = %letter,
                        from = %from_state,
                        to = %target,
                        reason = reason.as_str(),
                        "Hot/Warm shard had no body during demote; skipping",
                    );
                    return None;
                };
                let parked_body = Arc::new(body.to_parked_body());
                ShardEntry::new_parked(drive, stats, parked_body)
            }
            ShardState::Cold => ShardEntry::new_cold(drive, stats),
            // Filtered out by `is_legal_demote_target` above; this
            // arm is unreachable in practice, exists only so the
            // match is exhaustive without an `unreachable!`.
            ShardState::Unknown | ShardState::Warm | ShardState::Hot | ShardState::Evicting => {
                return None;
            }
        };
        // Build the rebuilt shards Vec via enumerate-and-replace so
        // the indexing-into-Vec is hidden inside `iter().map()` (no
        // raw `vec[pos] = ...` site for clippy to flag).  The old
        // entry's Arc is dropped by the closure when its branch isn't
        // taken; the body Arc inside it follows.
        let new_arc = Arc::new(new_entry);
        let shards: Vec<Arc<ShardEntry>> = self
            .shards
            .iter()
            .enumerate()
            .map(|(i, existing)| {
                if i == pos {
                    Arc::clone(&new_arc)
                } else {
                    Arc::clone(existing)
                }
            })
            .collect();
        tracing::info!(
            target: "shard.transition",
            letter = %letter,
            from = %from_state,
            to = %target,
            freed_mb,
            last_query_at_ms,
            reason = reason.as_str(),
        );
        Some(Self::from_shards(shards))
    }

    /// Promote the shard for `letter` from `Parked` / `Cold` back
    /// to `Warm`, attaching `body` and emitting a single
    /// `shard.transition` tracing event.  Returns the rebuilt
    /// registry, or `None` when:
    ///
    /// * `letter` is not registered;
    /// * the existing shard's state is not `Parked` / `Cold` (a request to
    ///   promote an already-warm shard is a caller bug).
    ///
    /// The per-drive `Arc<DriveStats>` from the demoted shard is
    /// shared with the new `Warm` entry so the round-trip
    /// demote-and-back preserves query counters.
    ///
    /// Wired into the search hot path by
    /// [`crate::index::IndexManager::ensure_warm_for_dispatch`]
    /// (Phase 3 Commit C).
    #[must_use]
    pub(crate) fn promote_letter(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        body: Arc<DriveCompactIndex>,
    ) -> Option<Self> {
        let (pos, old_arc) = self
            .shards
            .iter()
            .enumerate()
            .find(|(_, shard)| shard.drive == letter)?;
        let from_state = old_arc.state();
        if !matches!(from_state, ShardState::Parked | ShardState::Cold) {
            return None;
        }
        let restored_mb = (body.heap_size_bytes().total / 1_048_576) as u64;
        let stats = Arc::clone(&old_arc.stats);
        // Source is Parked or Cold (checked above): the body was re-read
        // from the on-disk cache, which is what the counter measures.
        stats.record_promote_from_demoted();
        let drive = old_arc.drive;
        let new_arc = Arc::new(ShardEntry::new_warm_with_stats(drive, body, stats));
        let shards: Vec<Arc<ShardEntry>> = self
            .shards
            .iter()
            .enumerate()
            .map(|(i, existing)| {
                if i == pos {
                    Arc::clone(&new_arc)
                } else {
                    Arc::clone(existing)
                }
            })
            .collect();
        tracing::info!(
            target: "shard.transition",
            letter = %letter,
            from = %from_state,
            to = %ShardState::Warm,
            restored_mb,
            reason = "promote",
        );
        Some(Self::from_shards(shards))
    }

    /// Promote the shard for `letter` to `Hot`, attaching `body`
    /// and emitting a single `shard.transition` tracing event.
    /// Returns the rebuilt registry, or `None` when:
    ///
    /// * `letter` is not registered;
    /// * the existing shard's state is `Hot` (caller must extend the pin via
    ///   [`crate::cache::shard::ShardEntry::pin_until`] on the live
    ///   `Arc<ShardEntry>` instead of rebuilding);
    /// * the existing shard's state is `Unknown` or `Evicting` (controller-only
    ///   states that the operator-driven preload path must not pre-empt).
    ///
    /// Phase 8-C — paired with
    /// [`crate::index::IndexManager::preload_drive`].  The caller
    /// has already loaded `body` (Cold/Parked source state) or
    /// cloned the existing one (Warm source state), and pre-faulted
    /// it via [`crate::cache::prefetch::Prefetch::hint`].  The pin
    /// timestamp lives on the new `ShardEntry`'s atomic
    /// `pin_until_ms`; arming it after the registry swap avoids
    /// surfacing a half-pinned intermediate state to concurrent
    /// readers.
    ///
    /// The per-drive `Arc<DriveStats>` from the previous shard is
    /// shared with the new `Hot` entry so the round-trip
    /// Cold/Parked/Warm → Hot preserves query counters and
    /// `last_query_at_ms`.
    #[must_use]
    pub(crate) fn promote_letter_to_hot(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        body: Arc<DriveCompactIndex>,
    ) -> Option<Self> {
        let (pos, old_arc) = self
            .shards
            .iter()
            .enumerate()
            .find(|(_, shard)| shard.drive == letter)?;
        let from_state = old_arc.state();
        if !matches!(
            from_state,
            ShardState::Parked | ShardState::Cold | ShardState::Warm
        ) {
            return None;
        }
        let restored_mb = (body.heap_size_bytes().total / 1_048_576) as u64;
        let stats = Arc::clone(&old_arc.stats);
        let drive = old_arc.drive;
        // Count the expensive promotes: a `Parked` or `Cold` source
        // means the caller re-read the body from the on-disk cache.
        // An already-Warm preload only flips the tier marker of a body
        // in RAM and does not count.
        if matches!(from_state, ShardState::Parked | ShardState::Cold) {
            stats.record_promote_from_demoted();
        }
        let new_arc = Arc::new(ShardEntry::new_hot_with_stats(drive, body, stats));
        let shards: Vec<Arc<ShardEntry>> = self
            .shards
            .iter()
            .enumerate()
            .map(|(i, existing)| {
                if i == pos {
                    Arc::clone(&new_arc)
                } else {
                    Arc::clone(existing)
                }
            })
            .collect();
        tracing::info!(
            target: "shard.transition",
            letter = %letter,
            from = %from_state,
            to = %ShardState::Hot,
            restored_mb,
            reason = "preload",
        );
        Some(Self::from_shards(shards))
    }

    /// Phase 5 (#95) — replace the body of a `Warm` / `Hot` shard
    /// with a fresher one (typically the output of
    /// [`uffs_core::compact_loader::load_drive_with_usn_refresh`]).
    /// Returns the rebuilt registry on success, or `None` when:
    ///
    /// * `letter` is not registered;
    /// * the existing shard's state is `Parked` / `Cold` / `Unknown` /
    ///   `Evicting` — these tiers don't have an in-memory body to refresh.  A
    ///   `Parked` shard gets a USN-refreshed body via the normal
    ///   `promote_letter` path on its next search-hot-path touch.
    ///
    /// The per-drive [`Arc<DriveStats>`] is preserved so query
    /// counters / `last_query_at_ms` survive the swap.  The new
    /// shard always lands in `Warm`; if the previous state was
    /// `Hot`, the next search query will re-promote it via the
    /// natural state-machine — the brief Hot → Warm → Hot bounce is
    /// invisible in production telemetry (sub-millisecond gap on
    /// the next dispatch).
    ///
    /// Wired into the production refresh path by
    /// [`crate::spawn_journal_loops_for_warm_shards`].
    #[must_use]
    pub(crate) fn replace_warm_body(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        body: Arc<DriveCompactIndex>,
    ) -> Option<Self> {
        let (pos, old_arc) = self
            .shards
            .iter()
            .enumerate()
            .find(|(_, shard)| shard.drive == letter)?;
        let from_state = old_arc.state();
        if !matches!(from_state, ShardState::Warm | ShardState::Hot) {
            return None;
        }
        let refreshed_mb = (body.heap_size_bytes().total / 1_048_576) as u64;
        let stats = Arc::clone(&old_arc.stats);
        let drive = old_arc.drive;
        let new_arc = Arc::new(ShardEntry::new_warm_with_stats(drive, body, stats));
        let shards: Vec<Arc<ShardEntry>> = self
            .shards
            .iter()
            .enumerate()
            .map(|(i, existing)| {
                if i == pos {
                    Arc::clone(&new_arc)
                } else {
                    Arc::clone(existing)
                }
            })
            .collect();
        // Warm → Warm with a fresher body is a body swap, not a tier
        // transition; it fires on every USN apply tick, so it stays
        // below INFO and leaves the `shard.transition` INFO stream to
        // real demotes and promotes.
        tracing::debug!(
            target: "shard.transition",
            letter = %letter,
            from = %from_state,
            to = %ShardState::Warm,
            refreshed_mb,
            reason = "usn-refresh",
        );
        Some(Self::from_shards(shards))
    }

    /// `true` iff the registry has no shards at all.
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.shards.is_empty()
    }

    /// `true` iff any shard exists for `drive` (regardless of tier).
    #[must_use]
    pub(crate) fn contains(&self, drive: uffs_mft::platform::DriveLetter) -> bool {
        self.shards.iter().any(|shard| shard.drive == drive)
    }

    /// Drive letters of every loaded shard in load order.
    #[must_use]
    pub(crate) fn loaded_letters(&self) -> Vec<uffs_mft::platform::DriveLetter> {
        self.shards.iter().map(|shard| shard.drive).collect()
    }
}

impl Default for ShardRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::min_ident_chars,
        reason = "test code — short names like `a`/`b` are clearer than \
                  long names in equality assertions."
    )]

    use super::*;

    /// Empty registry has zero shards and an empty active index.
    #[test]
    fn empty_registry_is_empty() {
        let reg = ShardRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.active_index().drives.len(), 0);
        assert_eq!(
            reg.loaded_letters(),
            Vec::<uffs_mft::platform::DriveLetter>::new()
        );
        assert!(!reg.contains(uffs_mft::platform::DriveLetter::C));
    }

    /// `Default` matches `new()`.
    #[test]
    fn default_matches_new() {
        let a = ShardRegistry::default();
        let b = ShardRegistry::new();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.is_empty(), b.is_empty());
    }

    /// `from_shards(empty)` produces the empty registry.
    #[test]
    fn from_shards_empty_is_empty() {
        let reg = ShardRegistry::from_shards(Vec::new());
        assert!(reg.is_empty());
        assert_eq!(reg.active_index().drives.len(), 0);
    }

    /// `remove` on a missing letter is a no-op that still produces a
    /// valid empty registry — pins the no-op contract that
    /// `IndexManager::replace_drive` relies on for fresh inserts.
    #[test]
    fn remove_missing_is_noop() {
        let reg = ShardRegistry::new().remove(uffs_mft::platform::DriveLetter::Z);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.active_index().drives.len(), 0);
    }

    // Tests that exercise mutation paths with real `DriveCompactIndex`
    // bodies live in `crate::index::tests` next to the existing
    // `build_test_drive` helper — see
    // `shard_registry_add_replace_remove_round_trip` and
    // `shard_registry_records_query_per_search`.
}
