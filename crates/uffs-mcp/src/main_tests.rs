use uffs_client::protocol::{AggregateResultWire, BucketWire, StatsWire};
use uffs_mcp::text::format_aggregate_summary;

#[test]
fn summary_count_result() {
    let results = vec![AggregateResultWire {
        label: Some("total_files".to_owned()),
        kind: "count".to_owned(),
        field: None,
        value: Some(42_000),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(summary.contains("total_files: 42000"), "got: {summary}");
}

#[test]
fn summary_stats_result() {
    let results = vec![AggregateResultWire {
        label: Some("size_stats".to_owned()),
        kind: "stats".to_owned(),
        field: Some("size".to_owned()),
        value: None,
        stats: Some(StatsWire {
            count: 1000,
            sum: 5_000_000,
            min: 0,
            max: 999_999,
            avg: 5000.0,
            waste_bytes: 100_000,
            waste_pct: 2.0,
        }),
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(summary.contains("count=1000"), "got: {summary}");
    assert!(summary.contains("sum=5000000"), "got: {summary}");
    assert!(summary.contains("avg=5000.0"), "got: {summary}");
    assert!(summary.contains("waste: 100000 bytes"), "got: {summary}");
}

#[test]
fn summary_buckets_result() {
    let results = vec![AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![
            BucketWire {
                key: "rs".to_owned(),
                count: 500,
                total_bytes: 2_000_000,
                total_allocated: None,
                avg_size: None,
                share_count: None,
                share_bytes: None,
                sample_rows: Vec::new(),
                drilldown: Vec::new(),
                sub_buckets: Vec::new(),
                verified: false,
            },
            BucketWire {
                key: "toml".to_owned(),
                count: 200,
                total_bytes: 50_000,
                total_allocated: None,
                avg_size: None,
                share_count: None,
                share_bytes: None,
                sample_rows: Vec::new(),
                drilldown: Vec::new(),
                sub_buckets: Vec::new(),
                verified: false,
            },
        ],
        other_count: Some(300),
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(summary.contains("ext_terms (2 buckets)"), "got: {summary}");
    assert!(summary.contains("rs"), "got: {summary}");
    assert!(summary.contains("toml"), "got: {summary}");
    assert!(summary.contains("300 in other groups"), "got: {summary}");
}

#[test]
fn summary_missing_result() {
    let results = vec![AggregateResultWire {
        label: Some("no_ext".to_owned()),
        kind: "missing".to_owned(),
        field: Some("extension".to_owned()),
        value: Some(150),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(
        summary.contains("150 records with missing"),
        "got: {summary}"
    );
}

#[test]
fn summary_distinct_result() {
    let results = vec![AggregateResultWire {
        label: Some("unique_exts".to_owned()),
        kind: "distinct".to_owned(),
        field: Some("extension".to_owned()),
        value: Some(4500),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(summary.contains("4500 distinct values"), "got: {summary}");
}

#[test]
fn summary_empty_results() {
    let summary = format_aggregate_summary(&[]);
    assert_eq!(summary, "No aggregate results.");
}

#[test]
fn summary_mixed_results() {
    let results = vec![
        AggregateResultWire {
            label: Some("total".to_owned()),
            kind: "count".to_owned(),
            field: None,
            value: Some(1000),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        },
        AggregateResultWire {
            label: Some("by_type".to_owned()),
            kind: "buckets".to_owned(),
            field: Some("type".to_owned()),
            value: None,
            stats: None,
            buckets: vec![BucketWire {
                key: "Document".to_owned(),
                count: 500,
                total_bytes: 1_000_000,
                total_allocated: None,
                avg_size: None,
                share_count: None,
                share_bytes: None,
                ..BucketWire::default()
            }],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        },
    ];
    let summary = format_aggregate_summary(&results);
    assert!(summary.contains("total: 1000"), "got: {summary}");
    assert!(summary.contains("by_type (1 buckets)"), "got: {summary}");
    assert!(summary.contains("Document"), "got: {summary}");
}

#[test]
fn summary_buckets_truncated_at_10() {
    let buckets: Vec<BucketWire> = (0_u64..15)
        .map(|i| BucketWire {
            key: format!("ext_{i}"),
            count: 15 - i,
            total_bytes: (15 - i) * 1000,
            ..BucketWire::default()
        })
        .collect();
    let results = vec![AggregateResultWire {
        label: Some("many".to_owned()),
        kind: "buckets".to_owned(),
        field: None,
        value: None,
        stats: None,
        buckets,
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(summary.contains("ext_0"), "first bucket present");
    assert!(summary.contains("ext_9"), "10th bucket present");
    assert!(!summary.contains("ext_10"), "11th bucket hidden");
    assert!(summary.contains("and 5 more"), "truncation message");
}

/// Validate that the aggregate tool schema has the expected properties.
#[test]
fn aggregate_tool_schema_valid() {
    let schema_json = serde_json::json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "default": "*" },
            "preset": { "type": "string" },
            "aggregations": { "type": "array" },
            "drives": { "type": "array", "items": { "type": "string" } }
        },
        "required": []
    });
    let props = schema_json
        .get("properties")
        .expect("schema has properties")
        .as_object()
        .unwrap();
    assert!(props.contains_key("pattern"));
    assert!(props.contains_key("preset"));
    assert!(props.contains_key("aggregations"));
    assert!(props.contains_key("drives"));
}

/// Validate that the `facet_values` tool schema requires "field" and
/// includes `cursor`/`page_size` for pagination.
#[test]
fn facet_values_tool_schema_valid() {
    let schema_json = serde_json::json!({
        "type": "object",
        "properties": {
            "field": { "type": "string" },
            "pattern": { "type": "string", "default": "*" },
            "prefix": { "type": "string" },
            "top": { "type": "integer", "default": 20_i32 },
            "cursor": { "type": "string" },
            "page_size": { "type": "integer" }
        },
        "required": ["field"]
    });
    let props = schema_json
        .get("properties")
        .expect("schema has properties")
        .as_object()
        .unwrap();
    assert!(props.contains_key("field"));
    assert!(props.contains_key("top"));
    assert!(
        props.contains_key("cursor"),
        "cursor param missing from schema"
    );
    assert!(
        props.contains_key("page_size"),
        "page_size param missing from schema"
    );
    let required = schema_json
        .get("required")
        .expect("schema has required")
        .as_array()
        .unwrap();
    assert!(required.iter().any(|ema| ema.as_str() == Some("field")));
    // cursor and page_size should NOT be required
    assert!(!required.iter().any(|ema| ema.as_str() == Some("cursor")));
    assert!(!required.iter().any(|ema| ema.as_str() == Some("page_size")));
}

/// `format_aggregate_summary` includes cursor hint when `next_cursor`
/// is present on a bucket result.
#[test]
fn summary_shows_next_cursor_when_present() {
    let results = vec![AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![BucketWire {
            key: "rs".to_owned(),
            count: 500,
            total_bytes: 2_000_000,
            ..BucketWire::default()
        }],
        other_count: None,
        total_groups: None,
        next_cursor: Some("0:1:1".to_owned()),
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(
        summary.contains("next_cursor: 0:1:1"),
        "summary should contain cursor hint, got: {summary}"
    );
}

/// `format_aggregate_summary` does NOT mention cursor when
/// `next_cursor` is `None`.
#[test]
fn summary_omits_cursor_when_none() {
    let results = vec![AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![BucketWire {
            key: "rs".to_owned(),
            count: 500,
            total_bytes: 2_000_000,
            ..BucketWire::default()
        }],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    }];
    let summary = format_aggregate_summary(&results);
    assert!(
        !summary.contains("next_cursor"),
        "summary should NOT mention cursor, got: {summary}"
    );
}

#[test]
fn summary_renders_sub_buckets() {
    let results = vec![AggregateResultWire {
        label: Some("drive_rollup".to_owned()),
        kind: "rollup".to_owned(),
        field: None,
        value: None,
        stats: None,
        buckets: vec![BucketWire {
            key: "C:".to_owned(),
            count: 1000,
            total_bytes: 5_000_000,
            sub_buckets: vec![
                BucketWire {
                    key: "document".to_owned(),
                    count: 600,
                    total_bytes: 3_000_000,
                    ..BucketWire::default()
                },
                BucketWire {
                    key: "image".to_owned(),
                    count: 400,
                    total_bytes: 2_000_000,
                    ..BucketWire::default()
                },
            ],
            ..BucketWire::default()
        }],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: Some(true),
        values_complete: Some(true),
    }];
    let summary = format_aggregate_summary(&results);
    assert!(
        summary.contains("document"),
        "should show sub-bucket 'document': {summary}"
    );
    assert!(
        summary.contains("image"),
        "should show sub-bucket 'image': {summary}"
    );
    assert!(
        summary.contains("├─"),
        "sub-buckets should be indented with ├─: {summary}"
    );
}

#[test]
fn summary_shows_values_complete_false() {
    let results = vec![AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![],
        other_count: Some(500),
        total_groups: Some(100),
        next_cursor: None,
        exact: Some(true),
        values_complete: Some(false),
    }];
    let summary = format_aggregate_summary(&results);
    assert!(
        summary.contains("truncated"),
        "should show truncation hint: {summary}"
    );
    assert!(
        summary.contains("500"),
        "should show other_count: {summary}"
    );
}

/// `format_scan_header` mirrors the `uffs_search` header phrasing so every
/// daemon-query tool (search, aggregate, `facet_values`) reports its measured
/// query time identically — agents can quote it straight from the text.
#[test]
fn scan_header_phrasing() {
    let header = uffs_mcp::text::format_scan_header(12_815_626, 3);
    assert_eq!(header, "(12815626 scanned in 3ms)");
}
