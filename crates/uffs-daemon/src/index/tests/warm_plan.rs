// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `IndexManager::promote_plan` / `warm_plan` parity tests (v0.6.38).
//!
//! Split from [`super::ensure_warm`] to hold that file under the
//! workspace 800-LOC ceiling; shares its bloom fixture
//! ([`super::ensure_warm::build_test_drive_with_tight_bloom`]) so the
//! plan tests and the dispatch tests exercise identical shards.

use alloc::sync::Arc;

use super::IndexManager;
use super::body_loader_fakes::PanickingBodyLoader;
use super::ensure_warm::build_test_drive_with_tight_bloom;
// ── `warm_plan` RPC parity with dispatch (v0.6.38) ─────────────────
//
// The MCP cold-index gate consumes `promote_plan`/`warm_plan` instead
// of inferring readiness from the tier marker; these pin that the plan
// is bit-exact with what `ensure_warm_for_dispatch` would promote —
// bloom pre-check included (field 2026-08-24: the tier-only gate
// declared a bloom-skippable Parked drive E: "not ready" and
// force-promoted its body for an ext-filtered query the bloom had
// already answered).

/// Bloom miss ⇒ the plan is empty: the gate must pass the query
/// through, because dispatch would promote nothing.
#[tokio::test]
async fn promote_plan_is_empty_on_bloom_miss() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanickingBodyLoader));
    mgr.add_drive(build_test_drive_with_tight_bloom()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // `csv` is novel to the fixture (its extensions are md/rs/toml/bin).
    let plan = mgr
        .promote_plan(&[uffs_mft::platform::DriveLetter::C], &["csv".to_owned()])
        .await;
    assert!(
        plan.is_empty(),
        "bloom miss must yield an empty plan — got {plan:?}"
    );
}

/// Bloom hit and no-ext-filter both plan the promote; a Warm shard
/// never appears in the plan.
#[tokio::test]
async fn promote_plan_names_drives_dispatch_would_promote() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanickingBodyLoader));
    mgr.add_drive(build_test_drive_with_tight_bloom()).await;

    // Warm shard: plan must be empty regardless of filter.
    assert!(
        mgr.promote_plan(&[], &[]).await.is_empty(),
        "a Warm shard must never be planned for promotion"
    );

    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    // Ext present in the fixture → bloom hit → planned.
    assert_eq!(
        mgr.promote_plan(&[], &["rs".to_owned()]).await,
        vec![uffs_mft::platform::DriveLetter::C],
        "bloom hit must plan the promote"
    );
    // No ext filter → Phase-3 always-promote behaviour.
    assert_eq!(
        mgr.promote_plan(&[], &[]).await,
        vec![uffs_mft::platform::DriveLetter::C],
        "no ext filter must plan every Parked shard in scope"
    );
    // Out-of-scope drive filter → not planned.
    assert!(
        mgr.promote_plan(&[uffs_mft::platform::DriveLetter::D], &[])
            .await
            .is_empty(),
        "a drive outside the query scope must not be planned"
    );
}

/// `warm_plan` resolves the ext-term set from wire `SearchParams`
/// through the same filter pipeline the search uses — the RPC-facing
/// half of the parity contract.
#[tokio::test]
async fn warm_plan_resolves_ext_filter_like_the_search_does() {
    use crate::cache::ShardState;

    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::with_body_loader_for_test(None, tx, Arc::new(PanickingBodyLoader));
    mgr.add_drive(build_test_drive_with_tight_bloom()).await;
    assert!(
        mgr.demote_letter_for_test(uffs_mft::platform::DriveLetter::C, ShardState::Parked)
            .await
    );

    let mut params_novel = uffs_client::protocol::SearchParams {
        pattern: "*".to_owned(),
        ext: Some("csv".to_owned()),
        ..Default::default()
    };
    params_novel.populate_canonical_fields();
    assert!(
        mgr.warm_plan(&params_novel).await.is_empty(),
        "--ext csv (novel to the fixture) must plan nothing"
    );

    let mut params_hit = uffs_client::protocol::SearchParams {
        pattern: "*".to_owned(),
        ext: Some("rs".to_owned()),
        ..Default::default()
    };
    params_hit.populate_canonical_fields();
    assert_eq!(
        mgr.warm_plan(&params_hit).await,
        vec![uffs_mft::platform::DriveLetter::C],
        "--ext rs (present in the fixture) must plan the promote"
    );
}
