// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Aggregate handler tests — preset/count/terms/stats/histogram/
//! missing/distinct/rollup/duplicates/raw-power-syntax + the
//! `terms_with_sample` drilldown contract.
//!
//! Drives the JSON-RPC `aggregate` entry-point via
//! [`IndexManager::run_aggregations`] over a single synthetic drive
//! (`build_test_drive` from `super`).

use uffs_client::protocol::AggregateSpecWire;

use super::{AggregationRequest, IndexManager, spec, test_index};

// ── Preset round-trip ────────────────────────────────────────────

#[test]
fn preset_overview_returns_multiple_results() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        preset: Some("overview".to_owned()),
        ..spec("preset")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    // overview preset expands to count + stats + terms etc.
    assert!(
        results.len() >= 3,
        "overview should produce ≥3 results, got {}",
        results.len()
    );
    // First result is typically count
    let count = results
        .iter()
        .find(|result| result.kind == "count")
        .unwrap();
    // 5 files + 1 dir + root = 7 records total
    assert!(count.value.unwrap() >= 5, "count should be ≥5 files");
}

// ── Count ────────────────────────────────────────────────────────

#[test]
fn count_returns_total_records() {
    let index = test_index();
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        None,
        &[spec("count")],
        AggregationRequest::default(),
    );
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "count"
    );
    // root + Projects dir + 5 files = 7
    assert_eq!(
        results.first().expect("results has an element 0").value,
        Some(7)
    );
}

// ── Stats ────────────────────────────────────────────────────────

#[test]
fn stats_size_returns_metrics() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("size".to_owned()),
        ..spec("stats")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "stats"
    );
    let stats = results
        .first()
        .expect("results has an element 0")
        .stats
        .as_ref()
        .unwrap();
    assert!(stats.count > 0);
    // Total of 500+2000+3000+100+10000 = 15600 for files; dirs have size 0
    assert!(stats.sum > 0);
    assert!(stats.min <= stats.max);
}

// ── Terms ────────────────────────────────────────────────────────

#[test]
fn terms_extension_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "buckets"
    );
    // We have rs, md, toml, bin extensions
    assert!(
        results
            .first()
            .expect("results has an element 0")
            .buckets
            .len()
            >= 3,
        "expected ≥3 ext buckets, got {}",
        results
            .first()
            .expect("results has an element 0")
            .buckets
            .len()
    );
    // "rs" should have 2 files (main.rs, lib.rs)
    let rs_bucket = results
        .first()
        .expect("results has an element 0")
        .buckets
        .iter()
        .find(|bucket| bucket.key == "rs");
    assert!(rs_bucket.is_some(), "should have 'rs' bucket");
    assert_eq!(rs_bucket.unwrap().count, 2);
}

// ── Histogram ────────────────────────────────────────────────────

#[test]
fn histogram_size_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("size".to_owned()),
        ..spec("histogram")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "buckets"
    );
    // Should have at least 1 bucket covering the file sizes
    assert!(
        !results
            .first()
            .expect("results has an element 0")
            .buckets
            .is_empty()
    );
}

// ── Date Histogram ───────────────────────────────────────────────

#[test]
fn date_histogram_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("modified".to_owned()),
        calendar: Some("month".to_owned()),
        ..spec("datehist")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "buckets"
    );
}

// ── Missing ──────────────────────────────────────────────────────

#[test]
fn missing_extension_counts_records_without_ext() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        ..spec("missing")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "missing"
    );
    // Root "." and dir "Projects" have no extension → ≥2 missing
    assert!(
        results
            .first()
            .expect("results has an element 0")
            .value
            .unwrap()
            >= 2
    );
}

// ── Distinct ─────────────────────────────────────────────────────

#[test]
fn distinct_extension_counts_unique_values() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        ..spec("distinct")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "distinct"
    );
    // rs, md, toml, bin → 4 distinct extensions
    assert!(
        results
            .first()
            .expect("results has an element 0")
            .value
            .unwrap()
            >= 4
    );
}

// ── Rollup ───────────────────────────────────────────────────────

#[test]
fn rollup_drive_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("drive".to_owned()),
        top: Some(10),
        ..spec("rollup")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    // Rollup → buckets or rollup kind
    assert!(
        !results
            .first()
            .expect("results has an element 0")
            .buckets
            .is_empty()
            || results
                .first()
                .expect("results has an element 0")
                .value
                .is_some()
    );
}

// ── Duplicates ───────────────────────────────────────────────────

#[test]
fn duplicates_returns_result() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        top: Some(10),
        ..spec("duplicates")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    // Should return exactly 1 result (even if 0 duplicates)
    assert_eq!(results.len(), 1);
}

// ── Raw power syntax ─────────────────────────────────────────────

#[test]
fn raw_power_syntax_terms_works() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        label: Some("terms:extension,top=5".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "buckets"
    );
    assert!(
        !results
            .first()
            .expect("results has an element 0")
            .buckets
            .is_empty()
    );
}

// ── Error handling ───────────────────────────────────────────────

#[test]
fn unknown_kind_skipped_gracefully() {
    let index = test_index();
    let specs = [spec("bogus_kind")];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(results.is_empty(), "unknown kind should produce no results");
}

#[test]
fn missing_field_skipped_gracefully() {
    let index = test_index();
    // stats requires a field but none provided
    let specs = [spec("stats")];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(
        results.is_empty(),
        "missing field should produce no results"
    );
}

// ── Multiple specs in one call ───────────────────────────────────

#[test]
fn multiple_specs_return_multiple_results() {
    let index = test_index();
    let specs = [
        spec("count"),
        AggregateSpecWire {
            field: Some("size".to_owned()),
            ..spec("stats")
        },
        AggregateSpecWire {
            field: Some("extension".to_owned()),
            top: Some(5),
            ..spec("terms")
        },
    ];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert_eq!(results.len(), 3, "should return one result per spec");
    assert_eq!(
        results.first().expect("results has an element 0").kind,
        "count"
    );
    assert_eq!(
        results.get(1).expect("results has an element 1").kind,
        "stats"
    );
    assert_eq!(
        results.get(2).expect("results has an element 2").kind,
        "buckets"
    );
}

// ── S1H.2: uffs --stats daemon-path parity ─────────────────────────

#[test]
fn stats_overview_preset_wire_roundtrip() {
    // Simulate the exact wire spec that `uffs --stats` (no path)
    // sends to the daemon, and verify it produces correct results.
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "preset".to_owned(),
        preset: Some("overview".to_owned()),
        ..AggregateSpecWire::default()
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());

    // Overview preset expands to multiple results.
    assert!(
        results.len() >= 3,
        "overview should produce ≥3 results, got {}",
        results.len()
    );

    // Must include a count result.
    let count = results
        .iter()
        .find(|result| result.kind == "count")
        .unwrap();
    assert_eq!(count.value, Some(7)); // root + dir + 5 files

    // Must include a stats result with valid metrics.
    let stats = results
        .iter()
        .find(|result| result.kind == "stats")
        .unwrap();
    let summary = stats.stats.as_ref().unwrap();
    assert!(summary.count > 0);
    assert!(summary.sum > 0);

    // Must include a buckets result (extension or type terms).
    let has_buckets = results.iter().any(|result| result.kind == "buckets");
    assert!(has_buckets, "overview should include bucket results");
}

// ── S2G.13: terms with sample=2 produces sample_rows + drilldown ──

#[test]
fn terms_with_sample_produces_sample_rows_and_drilldown() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(5),
        sample: Some(2),
        sample_sort: None,
        sample_desc: None,
        ..spec("terms")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(!results.is_empty(), "should have results");

    let bucket_result = results
        .iter()
        .find(|result| result.kind == "buckets")
        .expect("should have a buckets result");
    assert!(
        !bucket_result.buckets.is_empty(),
        "should have at least one bucket"
    );

    // At least one bucket should have sample_rows (our synthetic index
    // has files with extensions, so TopHits should find records).
    let has_samples = bucket_result
        .buckets
        .iter()
        .any(|bucket| !bucket.sample_rows.is_empty());
    assert!(
        has_samples,
        "at least one bucket should have sample rows with sample=2"
    );

    // Verify sample row constraints.
    for bucket in &bucket_result.buckets {
        assert!(
            bucket.sample_rows.len() <= 2,
            "sample rows should be bounded by sample=2, got {}",
            bucket.sample_rows.len()
        );
    }

    // Every bucket should have a drilldown predicate for the bucket key.
    for bucket in &bucket_result.buckets {
        assert!(
            !bucket.drilldown.is_empty(),
            "bucket '{}' should have drilldown predicates",
            bucket.key
        );
        let has_key_pred = bucket
            .drilldown
            .iter()
            .any(|pred| pred.field == "extension");
        assert!(
            has_key_pred,
            "bucket '{}' should have an extension drilldown predicate",
            bucket.key
        );
    }
}

#[test]
fn terms_without_sample_has_empty_sample_rows() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(5),
        ..spec("terms")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    let bucket_result = results
        .iter()
        .find(|result| result.kind == "buckets")
        .expect("should have buckets");
    // No sample was requested → all sample_rows should be empty.
    for bucket in &bucket_result.buckets {
        assert!(
            bucket.sample_rows.is_empty(),
            "bucket '{}' should not have sample rows without sample spec",
            bucket.key
        );
    }
}

// ── Stage 2 gap-fill: daemon integration tests ────────────────────

#[test]
fn rollup_drive_via_wire() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "rollup".to_owned(),
        field: Some("drive".to_owned()),
        top: Some(10),
        ..spec("rollup")
    }];
    let (results, _matched) =
        IndexManager::run_aggregations(&index, None, &specs, AggregationRequest::default());
    assert!(!results.is_empty(), "rollup:drive should return results");
    let result = results
        .iter()
        .find(|result| result.kind == "rollup")
        .expect("should have a rollup result");
    assert!(
        !result.buckets.is_empty(),
        "rollup:drive should have buckets"
    );
}
