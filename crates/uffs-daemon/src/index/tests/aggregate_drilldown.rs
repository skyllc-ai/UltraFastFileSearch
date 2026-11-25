// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Drilldown / wire-roundtrip / pagination / cache /
//! auto-concurrency tests for [`IndexManager::run_aggregations`].
//!
//! Sibling of [`super::aggregate`] — split out so the parent file
//! stays under the 800 LOC policy.  Both files share the same
//! `test_index` + `spec` fixtures from `super`.

use uffs_client::protocol::AggregateSpecWire;
use uffs_core::aggregate::AggregateFilter;
use uffs_core::aggregate::spec::AggregateKind;
use uffs_core::search::field::FieldId;

use super::{AggregationRequest, IndexManager, spec, test_index};

#[test]
fn rollup_path_with_sample_via_wire() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "rollup".to_owned(),
        field: Some("path".to_owned()),
        top: Some(5),
        sample: Some(2),
        ..spec("rollup")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(
        !results.is_empty(),
        "rollup:path with sample should return results"
    );
    let result = results
        .iter()
        .find(|result| result.kind == "rollup")
        .expect("should have a rollup result");
    // Sample rows should be bounded.
    for bucket in &result.buckets {
        assert!(
            bucket.sample_rows.len() <= 2,
            "rollup bucket '{}' should have ≤2 sample rows, got {}",
            bucket.key,
            bucket.sample_rows.len()
        );
    }
}

#[test]
fn convert_wire_spec_terms_with_sample_fields() {
    let ws = AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(5),
        sample: Some(3),
        sample_sort: Some("size".to_owned()),
        sample_desc: Some(true),
        ..spec("terms")
    };
    let converted = IndexManager::convert_wire_spec(&ws).unwrap();
    assert_eq!(converted.len(), 1);
    assert!(
        matches!(
            &converted.first().expect("converted has an element 0").kind,
            AggregateKind::Terms { .. }
        ),
        "expected Terms variant"
    );
    if let AggregateKind::Terms { sample, .. } =
        &converted.first().expect("converted has an element 0").kind
    {
        let top_hits = sample.as_ref().expect("sample should be Some");
        assert_eq!(top_hits.count, 3);
        assert_eq!(top_hits.sort_field, FieldId::Size);
        assert!(top_hits.sort_desc, "sort_desc should be true");
    }
}

#[test]
fn query_predicates_forwarded_to_drilldown() {
    use uffs_core::aggregate::finalize::{DrilldownPredicate, DrilldownValue};
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(3),
        sample: Some(1),
        ..spec("terms")
    }];
    let predicates = vec![DrilldownPredicate {
        field: "name".to_owned(),
        op: "glob".to_owned(),
        value: DrilldownValue::String("*.rs".to_owned()),
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest {
            query_predicates: predicates,
            ..AggregationRequest::default()
        });
    let result = results
        .iter()
        .find(|result| result.kind == "buckets")
        .expect("should have buckets");
    // Each bucket's drilldown should include the query predicate for "name".
    for bucket in &result.buckets {
        let has_name_pred = bucket
            .drilldown
            .iter()
            .any(|pred| pred.field == "name" && pred.op == "glob");
        assert!(
            has_name_pred,
            "bucket '{}' should have query predicate for 'name' in drilldown",
            bucket.key
        );
    }
}

#[test]
fn raw_power_syntax_rollup_drive() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "raw".to_owned(),
        label: Some("rollup:drive,top=5".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(
        !results.is_empty(),
        "raw rollup:drive should return results"
    );
}

#[test]
fn raw_power_syntax_hist_size() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "raw".to_owned(),
        label: Some("hist:size,interval=1048576".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(!results.is_empty(), "raw hist:size should return results");
}

#[test]
fn raw_power_syntax_stats_size() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "raw".to_owned(),
        label: Some("stats:size".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(!results.is_empty(), "raw stats:size should return results");
    let stats = results.iter().find(|result| result.kind == "stats");
    assert!(stats.is_some(), "should have a stats result");
}

// ── Cursor pagination ─────────────────────────────────────────────

#[test]
fn page_size_paginates_terms_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    // Request page_size=2 → first page should have ≤2 buckets.
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest {
            agg_page_size: Some(2),
            ..AggregationRequest::default()
        });
    let terms = results
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();
    assert!(
        terms.buckets.len() <= 2,
        "expected ≤2 buckets, got {}",
        terms.buckets.len()
    );
    // With 4 extensions and page_size=2, next_cursor should be present.
    assert!(
        terms.next_cursor.is_some(),
        "expected next_cursor for first page of 4 extensions with page_size=2"
    );
}

#[test]
fn cursor_returns_next_page() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];

    // First page.
    let (page1, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest {
            agg_page_size: Some(2),
            ..AggregationRequest::default()
        });
    let terms1 = page1
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();
    let cursor = terms1
        .next_cursor
        .as_deref()
        .expect("first page should have next_cursor");

    // Second page using cursor from first.
    let (page2, _matched_page2) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest {
            agg_cursor: Some(cursor),
            agg_page_size: Some(2),
            ..AggregationRequest::default()
        });
    let terms2 = page2
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();

    // Second page should have different keys than first page.
    let keys1: Vec<&str> = terms1
        .buckets
        .iter()
        .map(|bucket| bucket.key.as_str())
        .collect();
    let keys2: Vec<&str> = terms2
        .buckets
        .iter()
        .map(|bucket| bucket.key.as_str())
        .collect();
    assert!(
        !keys2.is_empty(),
        "second page should have at least 1 bucket"
    );
    for key in &keys2 {
        assert!(
            !keys1.contains(key),
            "second page key `{key}` should not appear in first page"
        );
    }
}

#[test]
fn page_size_does_not_affect_non_bucket_results() {
    let index = test_index();
    let specs = [spec("count")];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest {
            agg_page_size: Some(2),
            ..AggregationRequest::default()
        });
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "count"
    );
    // Count results should not have next_cursor.
    assert!(
        results
            .first()
            .expect("results has an element 0")
            .next_cursor
            .is_none(),
        "count results should never have next_cursor"
    );
}

#[test]
fn no_pagination_returns_all_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    // Without pagination, all buckets returned.
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    let terms = results
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();
    assert!(
        terms.buckets.len() >= 4,
        "expected ≥4 extension buckets without pagination, got {}",
        terms.buckets.len()
    );
    assert!(
        terms.next_cursor.is_none(),
        "no pagination should mean no next_cursor"
    );
}

#[test]
fn last_page_has_no_next_cursor() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    // Page size of 100 is bigger than our 4 extensions → single page.
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest {
            agg_page_size: Some(100),
            ..AggregationRequest::default()
        });
    let terms = results
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();
    assert!(
        terms.next_cursor.is_none(),
        "page_size larger than total buckets should produce no next_cursor"
    );
}

// ── Aggregate cache integration ──────────────────────────────────

#[test]
fn cache_hit_on_identical_second_call() {
    use uffs_core::aggregate::AggregateCache;

    let index = test_index();
    let cache = AggregateCache::default_ttl();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];

    // First call populates the cache (miss).
    let (first, _) =
        IndexManager::run_aggregations(&index, Some(&cache), &specs, AggregationRequest::default());
    let stats_after_first = cache.stats();
    assert_eq!(stats_after_first.misses, 1, "first call should miss");
    assert_eq!(stats_after_first.hits, 0, "first call cannot hit");
    assert_eq!(stats_after_first.entries, 1, "miss should populate cache");

    // Second call with identical inputs must hit.
    let (second, _) =
        IndexManager::run_aggregations(&index, Some(&cache), &specs, AggregationRequest::default());
    let stats_after_second = cache.stats();
    assert_eq!(stats_after_second.hits, 1, "second call must hit");
    assert_eq!(
        stats_after_second.misses, 1,
        "miss count must not increase on a hit"
    );

    // Response shape is identical.
    assert_eq!(
        first.len(),
        second.len(),
        "hit must return same number of results as miss"
    );
    let first_buckets = first
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();
    let second_buckets = second
        .iter()
        .find(|result| result.kind == "buckets")
        .unwrap();
    assert_eq!(
        first_buckets.buckets.len(),
        second_buckets.buckets.len(),
        "hit must return same bucket count as miss"
    );
}

#[test]
fn cache_miss_when_filter_differs() {
    use uffs_core::aggregate::AggregateCache;

    let index = test_index();
    let cache = AggregateCache::default_ttl();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];

    // Populate cache with the unfiltered query.
    let (_initial, _): (Vec<_>, u64) =
        IndexManager::run_aggregations(&index, Some(&cache), &specs, AggregationRequest::default());
    assert_eq!(cache.stats().entries, 1);

    // Identical query with a different filter must NOT hit.
    let narrower = AggregateFilter {
        extensions: vec!["rs".to_owned()],
        ..AggregateFilter::default()
    };
    let (_narrow, _): (Vec<_>, u64) =
        IndexManager::run_aggregations(&index, Some(&cache), &specs, AggregationRequest {
            record_filter: narrower,
            ..AggregationRequest::default()
        });
    let stats = cache.stats();
    assert_eq!(
        stats.hits, 0,
        "different filter must be treated as a different key"
    );
    assert_eq!(stats.misses, 2, "should log a second miss");
    assert_eq!(stats.entries, 2, "second miss should create a second entry");
}

#[test]
fn cache_invalidated_by_index_version_bump() {
    use uffs_core::aggregate::AggregateCache;

    let index = test_index();
    let cache = AggregateCache::default_ttl();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];

    // Populate cache.
    let (_seed, _): (Vec<_>, u64) =
        IndexManager::run_aggregations(&index, Some(&cache), &specs, AggregationRequest::default());
    assert_eq!(cache.stats().entries, 1);

    // Simulate a drive mutation: version bump → cache invalidation.
    cache.set_index_version(1);
    assert!(
        cache.is_empty(),
        "set_index_version must drop stale entries"
    );

    // Second call is a miss again because the previous entry was
    // invalidated by the version change.
    let (_post_bump, _): (Vec<_>, u64) =
        IndexManager::run_aggregations(&index, Some(&cache), &specs, AggregationRequest::default());
    let stats = cache.stats();
    assert_eq!(
        stats.hits, 0,
        "call after version bump must not see a cached entry"
    );
    assert!(stats.misses >= 2, "post-bump call must be recorded as miss");
}

// ── auto-concurrency target ────────────────────────────────────────────────
//
// Lock in the `max(2, (cpus × 26) / (drives × 10))` formula for a handful of
// representative box shapes.  If anyone ever wants to retune the factor,
// these assertions will fail loudly and the developer has to update both the
// formula *and* the docstring (which quotes the 24/7 = 8 measurement).

#[test]
fn auto_concurrency_target_24x7_landed_on_8() {
    // The whole point of the 2.6× factor: the user's 24-core / 7-drive
    // Windows box's empirical sweet spot (v0.5.45 sweep, 2026-04-18).
    assert_eq!(IndexManager::auto_concurrency_target(24, 7), 8);
}

#[test]
fn auto_concurrency_target_common_box_shapes() {
    // (cpus, drives, expected_permits) — derived from
    //   max(2, floor((cpus × 26) / (drives × 10)))
    let cases: &[(usize, usize, usize)] = &[
        // Laptops & desktops.
        (4, 1, 10),  //  4 × 26 /  10 = 10   (single-drive 4-core)
        (8, 1, 20),  //  8 × 26 /  10 = 20   (single-drive 8-core)
        (8, 2, 10),  //  8 × 26 /  20 = 10
        (12, 2, 15), // 12 × 26 /  20 = 15
        (16, 2, 20), // 16 × 26 /  20 = 20
        // Developer workstations (the Mac box).
        (16, 7, 5), // 16 × 26 /  70 =  5
        // The calibration target: user's Windows box.
        (24, 7, 8), // 24 × 26 /  70 =  8   ← landed on 8 by design
        // Big storage servers.
        (32, 14, 5), // 32 × 26 / 140 =  5
        (64, 2, 83), // 64 × 26 /  20 = 83
        // Edge cases.
        (1, 1, 2),    //  1 × 26 /  10 =  2 (floor of 2 kicks in at ceil)
        (2, 1, 5),    //  2 × 26 /  10 =  5
        (24, 100, 2), // 24 × 26 /1000 =  0 → floor to 2
    ];
    for &(cpus, drives, expected) in cases {
        assert_eq!(
            IndexManager::auto_concurrency_target(cpus, drives),
            expected,
            "auto_concurrency_target({cpus}, {drives}) should be {expected}"
        );
    }
}

#[test]
fn auto_concurrency_target_zero_drives_treated_as_one() {
    // Drives count of zero is clamped up to 1 so the daemon can still
    // admit queries during the pre-load window (there is nothing to
    // scan yet, so a permit is essentially free).
    assert_eq!(
        IndexManager::auto_concurrency_target(16, 0),
        IndexManager::auto_concurrency_target(16, 1),
        "drives = 0 must be treated identically to drives = 1"
    );
}

#[test]
fn auto_concurrency_target_floor_prevents_zero_permits() {
    // Pathological shape: many drives, few CPUs.  Raw ratio rounds to
    // zero — the floor of 2 must still apply so the daemon can make
    // progress.
    assert_eq!(IndexManager::auto_concurrency_target(2, 64), 2);
    assert_eq!(IndexManager::auto_concurrency_target(1, 200), 2);
}
