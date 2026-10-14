// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Protocol round-trip and serde tests.
//! Exception: `file_size_policy` — wire format test suite cohesion.

#![expect(
    clippy::indexing_slicing,
    reason = "test code — indices are verified by test assertions"
)]

use super::*;
use crate::protocol::response::{DaemonStatus, SearchPayload, SearchResponse, SearchRow};

/// D2.2.5: serialize/deserialize round-trip for request.
#[test]
fn request_round_trip() {
    let req = RpcRequest::new(1, "search", Some(serde_json::json!({"pattern": "*.rs"})));
    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: RpcRequest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.method, "search");
    assert_eq!(parsed.id, Some(1));
}

/// D2.2.5: serialize/deserialize round-trip for response.
#[test]
fn response_round_trip() {
    let resp = RpcResponse::success(
        42,
        serde_json::json!({"rows": [], "records_scanned": 0_u64}),
    );
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: RpcResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.id, 42);
}

/// D2.2.5: serialize/deserialize round-trip for error.
#[test]
fn error_round_trip() {
    let err = RpcErrorResponse::error(Some(1), ERR_METHOD_NOT_FOUND, "Method not found");
    let json = serde_json::to_string(&err).expect("serialize");
    let parsed: RpcErrorResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.error.code, ERR_METHOD_NOT_FOUND);
}

/// D2.2.5: `SearchParams` serialize/deserialize.
#[test]
fn search_params_round_trip() {
    let params = SearchParams {
        pattern: "*.rs".to_owned(),
        case_sensitive: true,
        sorts: vec![SearchSortSpec {
            field: "size".to_owned(),
            direction: Some(SearchSortDirection::Desc),
        }],
        limit: Some(100),
        filter_mode: Some(SearchFilterMode::Files),
        projection: vec!["path".to_owned(), "size".to_owned()],
        response_mode: Some(SearchResponseMode::Json),
        ..Default::default()
    };
    let json = serde_json::to_value(&params).expect("serialize");
    let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
    assert_eq!(parsed.pattern, "*.rs");
    assert!(parsed.case_sensitive);
    assert_eq!(parsed.limit, Some(100));
    assert_eq!(parsed.sorts.len(), 1);
    assert_eq!(parsed.filter_mode, Some(SearchFilterMode::Files));
    assert_eq!(parsed.response_mode, Some(SearchResponseMode::Json));
}

/// `normalize_malformed` round-trips, and an older payload that omits it
/// deserializes as `false` (backward-compatible via `#[serde(default)]`).
#[test]
fn search_params_normalize_malformed_round_trip_and_default() {
    let params = SearchParams {
        pattern: "*".to_owned(),
        normalize_malformed: true,
        ..Default::default()
    };
    let json = serde_json::to_value(&params).expect("serialize");
    let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
    assert!(
        parsed.normalize_malformed,
        "the flag must survive the JSON-RPC round trip"
    );

    // A payload from an older client that never knew the field.
    let legacy: SearchParams =
        serde_json::from_value(serde_json::json!({ "pattern": "*" })).expect("legacy deserialize");
    assert!(
        !legacy.normalize_malformed,
        "omitted field defaults off (wire backward-compat)"
    );
}

/// `resolve_lcn_order` round-trips, and an older payload that omits it
/// deserializes as `false` (backward-compatible via `#[serde(default)]`) --
/// mirrors `search_params_normalize_malformed_round_trip_and_default`.
#[test]
fn search_params_resolve_lcn_order_round_trip_and_default() {
    let params = SearchParams {
        pattern: "*".to_owned(),
        resolve_lcn_order: true,
        ..Default::default()
    };
    let json = serde_json::to_value(&params).expect("serialize");
    let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
    assert!(
        parsed.resolve_lcn_order,
        "the flag must survive the JSON-RPC round trip"
    );

    // A payload from an older client that never knew the field.
    let legacy: SearchParams =
        serde_json::from_value(serde_json::json!({ "pattern": "*" })).expect("legacy deserialize");
    assert!(
        !legacy.resolve_lcn_order,
        "omitted field defaults off (wire backward-compat)"
    );
}

/// The CLI surface: `--normalize-malformed` sets the param; absent → off.
#[test]
fn from_cli_args_normalize_malformed_flag() {
    let on = SearchParams::from_cli_args(&["*.tmp".to_owned(), "--normalize-malformed".to_owned()])
        .expect("parse with flag");
    assert!(
        on.normalize_malformed,
        "--normalize-malformed must set the flag"
    );

    let off = SearchParams::from_cli_args(&["*.tmp".to_owned()]).expect("parse without flag");
    assert!(!off.normalize_malformed, "absent flag defaults off");
}

/// The CLI surface: `--resolve-lcn-order` sets the param; absent → off.
/// Diagnostic/manual-verification flag -- see its match-arm comment in
/// `cli_args.rs` for why this exists alongside `uffs-content` setting the
/// same field directly.
#[test]
fn from_cli_args_resolve_lcn_order_flag() {
    let on = SearchParams::from_cli_args(&["*.tmp".to_owned(), "--resolve-lcn-order".to_owned()])
        .expect("parse with flag");
    assert!(
        on.resolve_lcn_order,
        "--resolve-lcn-order must set the flag"
    );

    let off = SearchParams::from_cli_args(&["*.tmp".to_owned()]).expect("parse without flag");
    assert!(!off.resolve_lcn_order, "absent flag defaults off");
}

/// Canonical helpers preserve legacy single-flag sort semantics.
///
/// First field: ascending by default (no `--sort-desc`).
/// Secondary fields: field-type defaults (numeric → desc, string → asc).
/// `--sort-desc` flag flips the first field to descending.
/// `-` prefix forces descending on any individual field.
#[test]
fn canonicalize_legacy_sort_preserves_primary_sort_desc_override() {
    // --sort size,name (no --sort-desc) → first=asc, second=field default
    let specs = SearchParams::canonicalize_legacy_sort("size,name", false);
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].field, "size");
    assert_eq!(
        specs[0].direction,
        Some(SearchSortDirection::Asc),
        "first field defaults to asc without --sort-desc"
    );
    assert_eq!(specs[1].field, "name");
    assert_eq!(
        specs[1].direction,
        Some(SearchSortDirection::Asc),
        "name (string) defaults to asc"
    );

    // --sort name --sort-desc → first field flipped to desc
    let desc_specs = SearchParams::canonicalize_legacy_sort("name", true);
    assert_eq!(desc_specs[0].direction, Some(SearchSortDirection::Desc));
}

/// `-` prefix forces descending on individual sort fields.
#[test]
fn canonicalize_legacy_sort_dash_prefix_descending() {
    // -modified,name → modified=desc, name=asc(default)
    let specs = SearchParams::canonicalize_legacy_sort("-modified,name", false);
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].field, "modified");
    assert_eq!(
        specs[0].direction,
        Some(SearchSortDirection::Desc),
        "dash prefix forces descending"
    );
    assert_eq!(specs[1].field, "name");
    assert_eq!(
        specs[1].direction,
        Some(SearchSortDirection::Asc),
        "name defaults to asc"
    );

    // -size alone
    let single = SearchParams::canonicalize_legacy_sort("-size", false);
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].field, "size");
    assert_eq!(single[0].direction, Some(SearchSortDirection::Desc));
}

/// Secondary numeric fields use field-type defaults (desc for
/// size/time/descendants).
#[test]
fn canonicalize_legacy_sort_secondary_field_defaults() {
    let specs = SearchParams::canonicalize_legacy_sort("name,size,modified", false);
    assert_eq!(specs.len(), 3);
    assert_eq!(
        specs[0].direction,
        Some(SearchSortDirection::Asc),
        "first field = asc"
    );
    assert_eq!(
        specs[1].direction,
        Some(SearchSortDirection::Desc),
        "secondary size defaults to desc"
    );
    assert_eq!(
        specs[2].direction,
        Some(SearchSortDirection::Desc),
        "secondary modified defaults to desc"
    );
}

/// Canonical helpers prefer the new filter field over the legacy one.
#[test]
fn resolved_filter_mode_prefers_canonical_field() {
    let params = SearchParams {
        filter: Some("dirs".to_owned()),
        filter_mode: Some(SearchFilterMode::Files),
        ..Default::default()
    };

    assert_eq!(params.resolved_filter_mode(), SearchFilterMode::Files);
}

/// D2.2.5: `DaemonStatus` serialize/deserialize.
#[test]
fn daemon_status_round_trip() {
    let loading = DaemonStatus::Loading {
        drives_loaded: 3,
        drives_total: 7,
    };
    let json = serde_json::to_string(&loading).expect("serialize");
    let parsed: DaemonStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, loading);

    let ready = DaemonStatus::Ready;
    let ready_json = serde_json::to_string(&ready).expect("serialize");
    let ready_parsed: DaemonStatus = serde_json::from_str(&ready_json).expect("deserialize");
    assert_eq!(ready_parsed, ready);
}

/// D2.2.5: `SearchResponse` round-trip with `InlineRows` payload.
///
/// Pins the default delivery channel: the daemon returns a
/// `Vec<SearchRow>` inline as a JSON array inside the RPC envelope.
/// Verifies that the tagged enum (`{"kind":"inline_rows","data":[…]}`)
/// serialises cleanly and round-trips without losing fields.
#[test]
fn search_response_inline_rows_round_trip() {
    let resp = SearchResponse {
        payload: SearchPayload::InlineRows(vec![SearchRow {
            drive: uffs_mft::platform::DriveLetter::C,
            path: "C:\\test.rs".to_owned(),
            name: "test.rs".to_owned(),
            size: 1024,
            is_directory: false,
            modified: 1_700_000_000_000_000,
            created: 1_700_000_000_000_000,
            accessed: 1_700_000_000_000_000,
            flags: 0x20,
            allocated: 4096,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
            malformed: false,
            malformed_path: false,
            name_hex: None,
            file_reference: 0,
        }]),
        total_count: 1,
        records_scanned: 1_000_000,
        duration_ms: 8,
        promotion_ms: None,
        truncated: false,
        profile: None,
        applied_sorts: vec![SearchSortSpec {
            field: "modified".to_owned(),
            direction: Some(SearchSortDirection::Desc),
        }],
        applied_projection: vec!["path".to_owned(), "size".to_owned()],
        response_mode: Some(SearchResponseMode::Rows),
        projected_rows: Some(vec![serde_json::Map::from_iter([
            (
                "path".to_owned(),
                serde_json::Value::String("C:\\test.rs".to_owned()),
            ),
            ("size".to_owned(), serde_json::Value::from(1024_u64)),
        ])]),
        aggregations: vec![],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    // Tagged-enum discriminator must be present on the wire —
    // without it the client can't dispatch on variant.
    assert!(
        json.contains("\"kind\":\"inline_rows\""),
        "inline_rows variant must be tagged on the wire: {json}"
    );

    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    let SearchPayload::InlineRows(rows) = &parsed.payload else {
        panic!("expected InlineRows variant, got {:?}", parsed.payload);
    };
    assert_eq!(rows.len(), 1);
    let first_row = rows.first().expect("at least one row");
    assert_eq!(first_row.name, "test.rs");
    assert_eq!(parsed.duration_ms, 8);
    assert_eq!(parsed.applied_sorts.len(), 1);
    assert_eq!(parsed.applied_projection.len(), 2);
    assert!(parsed.projected_rows.is_some());
}

/// WI-4.4: `name_hex` is forensic evidence keyed on name validity, not on
/// column projection — so the DEFAULT `--format json` output (no `--columns`)
/// carries it for every malformed row and omits it for well-formed ones.
///
/// This pins the exact CLI conversion (`serde_json::to_value(&SearchRow)` in
/// `uffs-cli`'s `main.rs`, then `write_json`): a malformed row's hex must be
/// present without the caller having to request the `name_hex` column.
#[test]
fn search_row_default_json_carries_name_hex_for_malformed_only() {
    let base = SearchRow {
        drive: uffs_mft::platform::DriveLetter::G,
        path: "G:\\corrupted\\file.txt".to_owned(),
        name: "file.txt".to_owned(),
        size: 0,
        is_directory: false,
        modified: 0,
        created: 0,
        accessed: 0,
        flags: 0x20,
        allocated: 0,
        descendants: 0,
        treesize: 0,
        tree_allocated: 0,
        malformed: false,
        malformed_path: false,
        name_hex: None,
        file_reference: 0,
    };

    // Well-formed row: no hex evidence, so the key is dropped entirely.
    let well_formed = serde_json::to_value(&base).expect("SearchRow serialises");
    assert!(
        well_formed.get("name_hex").is_none(),
        "well-formed rows must omit name_hex from default JSON: {well_formed}"
    );

    // Malformed row (lone-high-surrogate leaf `ED A0 80`): hex must appear in
    // the default object with no projection requested.
    let malformed = SearchRow {
        malformed: true,
        malformed_path: true,
        name_hex: Some("eda080".to_owned()),
        ..base
    };
    let value = serde_json::to_value(&malformed).expect("SearchRow serialises");
    assert_eq!(
        value.get("name_hex").and_then(serde_json::Value::as_str),
        Some("eda080"),
        "malformed rows must carry name_hex in default JSON (no --columns): {value}"
    );
    assert_eq!(
        value.get("malformed").and_then(serde_json::Value::as_bool),
        Some(true),
        "malformed flag must also be present by default: {value}"
    );
}

/// `SearchResponse` round-trip with the `ShmemBlob` payload.
///
/// Covers the binary-transport fast path for large path-only
/// responses: the daemon writes the blob to a shmem file and
/// returns only the path as `{"kind":"shmem_blob","data":"<path>"}`.
/// This test verifies:
///
/// 1. The tagged-enum discriminator serialises cleanly on the wire.
/// 2. The path round-trips exactly — critical because the client opens it
///    verbatim with no quoting or escaping.
/// 3. Absent payloads default to `Empty` via `SearchPayload::default` when the
///    `data` key is missing (forward-compat for future daemons that omit the
///    field for no-match responses).
#[test]
fn search_response_shmem_blob_round_trip() {
    let shmem_path = "C:\\Users\\rnio\\AppData\\Local\\uffs\\shmem\\search_12345_0.bin";
    let resp = SearchResponse {
        payload: SearchPayload::ShmemBlob(shmem_path.to_owned()),
        total_count: 168_295,
        records_scanned: 3_571_389,
        duration_ms: 60,
        promotion_ms: None,
        truncated: false,
        profile: None,
        applied_sorts: vec![],
        applied_projection: vec!["path".to_owned()],
        response_mode: Some(SearchResponseMode::Rows),
        projected_rows: None,
        aggregations: vec![],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    assert!(
        json.contains("\"kind\":\"shmem_blob\""),
        "shmem_blob variant must be tagged on the wire: {json}"
    );
    // The path string lives in the `data` field — not as a
    // separate top-level `paths_blob_shmem` key.  Verifies the
    // v0.5.62 migration from flat-field layout to tagged enum.
    assert!(
        !json.contains("\"paths_blob_shmem\""),
        "legacy `paths_blob_shmem` field must not appear on the \
         wire after the SearchPayload migration: {json}"
    );
    assert!(
        !json.contains("\"rows\":[]"),
        "empty `rows` must not appear on the wire — the payload \
         carries the shape now: {json}"
    );

    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    let SearchPayload::ShmemBlob(path) = &parsed.payload else {
        panic!("expected ShmemBlob variant, got {:?}", parsed.payload);
    };
    assert_eq!(path, shmem_path);
    assert_eq!(
        parsed.total_count, 168_295,
        "total_count must round-trip so the CLI's --profile display \
         can show the row count without mmapping the shmem file"
    );
    assert_eq!(
        parsed.payload.row_count_hint(),
        None,
        "shmem_blob carries raw bytes, not structured rows — \
         row_count_hint must return None so callers fall back to \
         total_count"
    );
}

/// `SearchResponse` round-trip with the `InlineBlob` payload.
///
/// Covers the small-payload path-only fast path: the daemon
/// pre-formats the output and inlines it as a UTF-8 string under
/// `{"kind":"inline_blob","data":"<bytes>"}`.  The CLI writes the
/// blob to stdout with a single `write_all`.
#[test]
fn search_response_inline_blob_round_trip() {
    let blob = "C:\\Windows\\System32\\a.dll\nC:\\Windows\\System32\\b.dll\n";
    let resp = SearchResponse {
        payload: SearchPayload::InlineBlob(blob.to_owned()),
        total_count: 2,
        records_scanned: 250_000,
        duration_ms: 5,
        promotion_ms: None,
        truncated: false,
        profile: None,
        applied_sorts: vec![],
        applied_projection: vec!["path".to_owned()],
        response_mode: Some(SearchResponseMode::Rows),
        projected_rows: None,
        aggregations: vec![],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    assert!(
        json.contains("\"kind\":\"inline_blob\""),
        "inline_blob variant must be tagged on the wire"
    );

    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    let SearchPayload::InlineBlob(parsed_blob) = &parsed.payload else {
        panic!("expected InlineBlob variant, got {:?}", parsed.payload);
    };
    assert_eq!(parsed_blob, blob);
    assert_eq!(
        parsed.payload.row_count_hint(),
        None,
        "inline_blob is opaque bytes — row_count_hint must be None"
    );

    // Empty variant round-trips as `{"kind":"empty"}` — used by
    // no-match queries, --no-output, and --out=file responses.
    let empty_resp = SearchResponse {
        payload: SearchPayload::Empty,
        ..resp
    };
    let empty_json = serde_json::to_string(&empty_resp).expect("serialize");
    assert!(
        empty_json.contains("\"kind\":\"empty\""),
        "Empty variant must still serialise with its tag — otherwise \
         the client's deserializer can't distinguish it from a \
         missing/null payload: {empty_json}"
    );
    let empty_parsed: SearchResponse =
        serde_json::from_str(&empty_json).expect("deserialize empty");
    assert!(
        matches!(empty_parsed.payload, SearchPayload::Empty),
        "Empty payload must round-trip to Empty, not default to \
         another variant"
    );
    assert_eq!(
        empty_parsed.payload.row_count_hint(),
        Some(0),
        "Empty payload's row_count_hint is Some(0) — distinct from \
         blob variants' None so the CLI can differentiate"
    );
}

/// `SearchResponse` round-trip with the `ShmemRows` payload.
///
/// Covers the large multi-column response path: full `SearchRow`
/// records sit in a binary shmem file, and only the path + row
/// count travel in the RPC envelope.  The client reads the file
/// with `read_search_results` which returns an `InlineRows`
/// payload — the transport is invisible to the caller after that
/// resolution step.
#[test]
fn search_response_shmem_rows_round_trip() {
    let shmem_path = "/tmp/uffs/shmem/search_98765_1.bin";
    let resp = SearchResponse {
        payload: SearchPayload::ShmemRows {
            path: shmem_path.to_owned(),
            count: 250_000,
        },
        total_count: 250_000,
        records_scanned: 5_000_000,
        duration_ms: 42,
        promotion_ms: None,
        truncated: false,
        profile: None,
        applied_sorts: vec![],
        applied_projection: vec!["path".to_owned(), "size".to_owned()],
        response_mode: Some(SearchResponseMode::Rows),
        projected_rows: None,
        aggregations: vec![],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    assert!(
        json.contains("\"kind\":\"shmem_rows\""),
        "shmem_rows variant must be tagged on the wire: {json}"
    );
    // Struct variants carry their fields as nested keys under `data`;
    // verify both `path` and `count` serialise together so the client
    // can pre-allocate the receiving Vec without a second RPC.
    assert!(
        json.contains("\"path\"") && json.contains("\"count\""),
        "shmem_rows struct variant must expose path and count \
         together in data: {json}"
    );

    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    let SearchPayload::ShmemRows { path, count } = &parsed.payload else {
        panic!("expected ShmemRows variant, got {:?}", parsed.payload);
    };
    assert_eq!(path, shmem_path);
    assert_eq!(*count, 250_000);
    assert_eq!(
        parsed.payload.row_count_hint(),
        Some(250_000),
        "ShmemRows's row_count_hint uses the `count` field directly \
         — no need to mmap the file just to size a log line"
    );
}

// ── S1C.4 — Aggregate wire type round-trip tests ──────────────────

/// `AggregateSpecWire` round-trip: preset variant.
#[test]
fn aggregate_spec_wire_preset_round_trip() {
    let spec = AggregateSpecWire {
        kind: "preset".to_owned(),
        label: None,
        field: None,
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec![],
        preset: Some("overview".to_owned()),
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "preset");
    assert_eq!(parsed.preset.as_deref(), Some("overview"));
    // Optional fields should be absent in JSON
    assert!(!json.contains("\"label\""));
    assert!(!json.contains("\"field\""));
}

/// `AggregateSpecWire` round-trip: terms variant with all fields.
#[test]
fn aggregate_spec_wire_terms_round_trip() {
    let spec = AggregateSpecWire {
        kind: "terms".to_owned(),
        label: Some("ext_breakdown".to_owned()),
        field: Some("extension".to_owned()),
        top: Some(50),
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec!["count".to_owned(), "total_bytes".to_owned()],
        preset: None,
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "terms");
    assert_eq!(parsed.label.as_deref(), Some("ext_breakdown"));
    assert_eq!(parsed.field.as_deref(), Some("extension"));
    assert_eq!(parsed.top, Some(50));
    assert_eq!(parsed.metrics.len(), 2);
    assert!(parsed.preset.is_none());
}

/// `AggregateSpecWire` round-trip: date histogram variant.
#[test]
fn aggregate_spec_wire_date_histogram_round_trip() {
    let spec = AggregateSpecWire {
        kind: "date_histogram".to_owned(),
        label: Some("modified_monthly".to_owned()),
        field: Some("modified".to_owned()),
        top: None,
        interval: None,
        calendar: Some("month".to_owned()),
        boundaries: vec![],
        metrics: vec!["count".to_owned()],
        preset: None,
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "date_histogram");
    assert_eq!(parsed.calendar.as_deref(), Some("month"));
}

/// `AggregateSpecWire` round-trip: range variant with boundaries.
#[test]
fn aggregate_spec_wire_range_round_trip() {
    let spec = AggregateSpecWire {
        kind: "range".to_owned(),
        label: None,
        field: Some("size".to_owned()),
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![0, 1024, 1_048_576, 1_073_741_824],
        metrics: vec![],
        preset: None,
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.boundaries, vec![0, 1024, 1_048_576, 1_073_741_824]);
}

/// `StatsWire` round-trip.
#[test]
fn stats_wire_round_trip() {
    let stats = StatsWire {
        count: 10_000,
        sum: 5_000_000,
        min: 0,
        max: 1_000_000,
        avg: 500.0,
        waste_bytes: 200_000,
        waste_pct: 4.0,
    };
    let json = serde_json::to_string(&stats).expect("serialize");
    let parsed: StatsWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.count, 10_000);
    assert_eq!(parsed.sum, 5_000_000);
    assert_eq!(parsed.min, 0);
    assert_eq!(parsed.max, 1_000_000);
    assert!((parsed.avg - 500.0).abs() < f64::EPSILON);
    assert_eq!(parsed.waste_bytes, 200_000);
    assert!((parsed.waste_pct - 4.0).abs() < f64::EPSILON);
}

/// `BucketWire` round-trip: all optional fields present.
#[test]
fn bucket_wire_full_round_trip() {
    let bucket = BucketWire {
        key: "rs".to_owned(),
        count: 500,
        total_bytes: 2_000_000,
        total_allocated: Some(2_500_000),
        avg_size: Some(4_000.0_f64),
        share_count: Some(5.0_f64),
        share_bytes: Some(3.2_f64),
        sample_rows: Vec::new(),
        drilldown: Vec::new(),
        sub_buckets: Vec::new(),
        verified: false,
    };
    let json = serde_json::to_string(&bucket).expect("serialize");
    let parsed: BucketWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.key, "rs");
    assert_eq!(parsed.count, 500);
    assert_eq!(parsed.total_bytes, 2_000_000);
    assert_eq!(parsed.total_allocated, Some(2_500_000));
    assert!((parsed.avg_size.expect("avg_size") - 4000.0).abs() < f64::EPSILON);
    assert!((parsed.share_count.expect("share_count") - 5.0).abs() < f64::EPSILON);
    assert!((parsed.share_bytes.expect("share_bytes") - 3.2).abs() < f64::EPSILON);
}

/// `BucketWire` round-trip: only required fields (optional fields absent in
/// JSON).
#[test]
fn bucket_wire_minimal_round_trip() {
    let json_str = r#"{"key":"doc","count":10,"total_bytes":1024}"#;
    let parsed: BucketWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.key, "doc");
    assert_eq!(parsed.count, 10);
    assert_eq!(parsed.total_bytes, 1024);
    assert!(parsed.total_allocated.is_none());
    assert!(parsed.avg_size.is_none());
    assert!(parsed.share_count.is_none());
    assert!(parsed.share_bytes.is_none());
    // Re-serialize and verify optional fields are absent
    let re_json = serde_json::to_string(&parsed).expect("re-serialize");
    assert!(!re_json.contains("total_allocated"));
    assert!(!re_json.contains("avg_size"));
}

/// `AggregateResultWire` round-trip: count kind.
#[test]
fn aggregate_result_wire_count_round_trip() {
    let result = AggregateResultWire {
        label: Some("total_count".to_owned()),
        kind: "count".to_owned(),
        field: None,
        value: Some(1_234_567),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "count");
    assert_eq!(parsed.value, Some(1_234_567));
    assert!(parsed.stats.is_none());
    assert!(parsed.buckets.is_empty());
}

/// `AggregateResultWire` round-trip: stats kind with `StatsWire`.
#[test]
fn aggregate_result_wire_stats_round_trip() {
    let result = AggregateResultWire {
        label: Some("size_stats".to_owned()),
        kind: "stats".to_owned(),
        field: Some("size".to_owned()),
        value: None,
        stats: Some(StatsWire {
            count: 100,
            sum: 50_000,
            min: 10,
            max: 9_000,
            avg: 500.0,
            waste_bytes: 1_000,
            waste_pct: 2.0,
        }),
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "stats");
    let stats = parsed.stats.expect("stats present");
    assert_eq!(stats.count, 100);
    assert_eq!(stats.sum, 50_000);
}

/// `AggregateResultWire` round-trip: terms with buckets + truncation
/// metadata.
#[test]
fn aggregate_result_wire_terms_round_trip() {
    let result = AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![
            BucketWire {
                key: "rs".to_owned(),
                count: 500,
                total_bytes: 2_000_000,
                avg_size: Some(4_000.0_f64),
                ..BucketWire::default()
            },
            BucketWire {
                key: "toml".to_owned(),
                count: 200,
                total_bytes: 50_000,
                avg_size: Some(250.0_f64),
                ..BucketWire::default()
            },
        ],
        other_count: Some(300),
        total_groups: Some(150),
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.kind, "terms");
    assert_eq!(parsed.buckets.len(), 2);
    assert_eq!(parsed.buckets[0].key, "rs");
    assert_eq!(parsed.buckets[1].key, "toml");
    assert_eq!(parsed.other_count, Some(300));
    assert_eq!(parsed.total_groups, Some(150));
}

/// `SearchParams` round-trip with aggregations + `include_rows`.
#[test]
fn search_params_with_aggregations_round_trip() {
    let params = SearchParams {
        pattern: "*.rs".to_owned(),
        aggregations: vec![
            AggregateSpecWire {
                kind: "preset".to_owned(),
                preset: Some("overview".to_owned()),
                ..AggregateSpecWire::default()
            },
            AggregateSpecWire {
                kind: "count".to_owned(),
                label: Some("total".to_owned()),
                ..AggregateSpecWire::default()
            },
        ],
        include_rows: false,
        ..Default::default()
    };
    let json = serde_json::to_value(&params).expect("serialize");
    let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
    assert_eq!(parsed.aggregations.len(), 2);
    assert!(!parsed.include_rows);
    assert_eq!(parsed.aggregations[0].kind, "preset");
    assert_eq!(parsed.aggregations[0].preset.as_deref(), Some("overview"));
    assert_eq!(parsed.aggregations[1].kind, "count");
    assert_eq!(parsed.aggregations[1].label.as_deref(), Some("total"));
}

/// `SearchResponse` round-trip with aggregations and an `Empty`
/// payload.
///
/// Aggregate-only queries pass `include_rows: false`, so the daemon
/// never materialises `SearchRow` records — the payload lands on
/// [`SearchPayload::Empty`] and the bucket data travels in
/// [`SearchResponse::aggregations`].  This test pins that split so
/// a future refactor can't accidentally merge aggregations into
/// the payload enum.
#[test]
fn search_response_with_aggregations_round_trip() {
    let resp = SearchResponse {
        payload: SearchPayload::Empty,
        total_count: 0,
        records_scanned: 500_000,
        duration_ms: 12,
        promotion_ms: None,
        truncated: false,
        profile: None,
        applied_sorts: vec![],
        applied_projection: vec![],
        response_mode: None,
        projected_rows: None,
        aggregations: vec![
            AggregateResultWire {
                label: Some("total_count".to_owned()),
                kind: "count".to_owned(),
                field: None,
                value: Some(500_000),
                stats: None,
                buckets: vec![],
                other_count: None,
                total_groups: None,
                next_cursor: None,
                exact: None,
                values_complete: None,
            },
            AggregateResultWire {
                label: Some("type_breakdown".to_owned()),
                kind: "terms".to_owned(),
                field: Some("type".to_owned()),
                value: None,
                stats: None,
                buckets: vec![BucketWire {
                    key: "Document".to_owned(),
                    count: 10_000,
                    total_bytes: 500_000_000,
                    avg_size: Some(50_000.0_f64),
                    share_count: Some(2.0_f64),
                    share_bytes: Some(10.0_f64),
                    ..BucketWire::default()
                }],
                other_count: Some(490_000),
                total_groups: Some(12),
                next_cursor: None,
                exact: None,
                values_complete: None,
            },
        ],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
    assert!(
        matches!(parsed.payload, SearchPayload::Empty),
        "aggregate-only queries produce Empty payload so the bucket \
         data is not double-delivered"
    );
    assert_eq!(parsed.aggregations.len(), 2);
    assert_eq!(parsed.aggregations[0].kind, "count");
    assert_eq!(parsed.aggregations[0].value, Some(500_000));
    assert_eq!(parsed.aggregations[1].kind, "terms");
    assert_eq!(parsed.aggregations[1].buckets.len(), 1);
    assert_eq!(parsed.aggregations[1].buckets[0].key, "Document");
    assert_eq!(parsed.aggregations[1].other_count, Some(490_000));
}

/// Deserialize `AggregateSpecWire` from minimal JSON (only required
/// fields).
#[test]
fn aggregate_spec_wire_minimal_json() {
    let json_str = r#"{"kind":"count"}"#;
    let parsed: AggregateSpecWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.kind, "count");
    assert!(parsed.label.is_none());
    assert!(parsed.field.is_none());
    assert_eq!(parsed.boundaries, Vec::<u64>::new());
    assert_eq!(parsed.metrics, Vec::<String>::new());
    assert!(parsed.preset.is_none());
}

/// Deserialize `AggregateResultWire` from minimal JSON.
#[test]
fn aggregate_result_wire_minimal_json() {
    let json_str = r#"{"kind":"count","value":42}"#;
    let parsed: AggregateResultWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.kind, "count");
    assert_eq!(parsed.value, Some(42));
    assert!(parsed.label.is_none());
    assert!(parsed.stats.is_none());
    assert!(parsed.buckets.is_empty());
}

// ── S2G.12: Serde round-trip tests for wire types ─────────────────

#[test]
fn sample_row_wire_round_trip() {
    let mut fields = std::collections::HashMap::new();
    fields.insert("name".to_owned(), "foo.rs".to_owned());
    fields.insert("size".to_owned(), "4096".to_owned());
    let wire = SampleRowWire {
        fields,
        sort_key: Some(4096),
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    let parsed: SampleRowWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&parsed.fields["name"], "foo.rs");
    assert_eq!(&parsed.fields["size"], "4096");
    assert_eq!(parsed.sort_key, Some(4096));
}

#[test]
fn sample_row_wire_no_sort_key() {
    let wire = SampleRowWire {
        fields: std::collections::HashMap::new(),
        sort_key: None,
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    assert!(!json.contains("sort_key"), "sort_key should be omitted");
    let parsed: SampleRowWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.sort_key, None);
}

#[test]
fn drilldown_wire_round_trip() {
    let wire = DrilldownWire {
        field: "extension".to_owned(),
        op: "eq".to_owned(),
        value: serde_json::Value::String("rs".to_owned()),
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    let parsed: DrilldownWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.field, "extension");
    assert_eq!(parsed.op, "eq");
    assert_eq!(parsed.value, serde_json::Value::String("rs".to_owned()));
}

#[test]
fn drilldown_wire_numeric_value() {
    let wire = DrilldownWire {
        field: "size".to_owned(),
        op: "gte".to_owned(),
        value: serde_json::Value::Number(1_024_i64.into()),
    };
    let json = serde_json::to_string(&wire).expect("serialize");
    let parsed: DrilldownWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.value, serde_json::Value::Number(1_024_i64.into()));
}

#[test]
fn bucket_wire_with_samples_round_trip() {
    let mut fields = std::collections::HashMap::new();
    fields.insert("name".to_owned(), "bar.rs".to_owned());
    let bucket = BucketWire {
        key: "rs".to_owned(),
        count: 100,
        total_bytes: 50_000,
        total_allocated: None,
        avg_size: None,
        share_count: None,
        share_bytes: None,
        sample_rows: vec![SampleRowWire {
            fields,
            sort_key: Some(999),
        }],
        drilldown: vec![DrilldownWire {
            field: "extension".to_owned(),
            op: "eq".to_owned(),
            value: serde_json::Value::String("rs".to_owned()),
        }],
        sub_buckets: Vec::new(),
        verified: false,
    };
    let json = serde_json::to_string(&bucket).expect("serialize");
    assert!(json.contains("sample_rows"));
    assert!(json.contains("drilldown"));
    let parsed: BucketWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.sample_rows.len(), 1);
    assert_eq!(parsed.drilldown.len(), 1);
    assert_eq!(&parsed.sample_rows[0].fields["name"], "bar.rs");
    assert_eq!(parsed.drilldown[0].field, "extension");
}

#[test]
fn bucket_wire_empty_samples_omitted() {
    let bucket = BucketWire {
        key: "txt".to_owned(),
        count: 10,
        total_bytes: 1000,
        ..BucketWire::default()
    };
    let json = serde_json::to_string(&bucket).expect("serialize");
    assert!(
        !json.contains("sample_rows"),
        "empty sample_rows should be omitted"
    );
    assert!(
        !json.contains("drilldown"),
        "empty drilldown should be omitted"
    );
}

#[test]
fn bucket_wire_backward_compat_no_sample_fields() {
    // Old JSON without sample_rows/drilldown should deserialize fine.
    let json_str = r#"{"key":"rs","count":50,"total_bytes":1000}"#;
    let parsed: BucketWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.key, "rs");
    assert_eq!(parsed.count, 50);
    assert_eq!(parsed.sample_rows, Vec::<SampleRowWire>::new());
    assert_eq!(parsed.drilldown, Vec::<DrilldownWire>::new());
}

#[test]
fn aggregate_spec_wire_with_sample() {
    let spec = AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(10),
        sample: Some(3),
        sample_sort: Some("size".to_owned()),
        sample_desc: Some(true),
        ..AggregateSpecWire::default()
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    assert!(json.contains(r#""sample":3"#));
    assert!(json.contains(r#""sample_sort":"size""#));
    let parsed: AggregateSpecWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.sample, Some(3));
    assert_eq!(parsed.sample_sort.as_deref(), Some("size"));
    assert_eq!(parsed.sample_desc, Some(true));
}

#[test]
fn aggregate_spec_wire_no_sample_backward_compat() {
    // Old JSON without sample fields should deserialize fine.
    let json_str = r#"{"kind":"terms","field":"extension","top":10}"#;
    let parsed: AggregateSpecWire = serde_json::from_str(json_str).expect("deserialize");
    assert_eq!(parsed.kind, "terms");
    assert!(parsed.sample.is_none());
    assert!(parsed.sample_sort.is_none());
    assert!(parsed.sample_desc.is_none());
}

// ── Cursor pagination serde ─────────────────────────────────────

/// `SearchParams` with `agg_cursor` and `agg_page_size` round-trips
/// correctly; fields are omitted from JSON when `None`.
#[test]
fn search_params_cursor_pagination_round_trip() {
    let params = SearchParams {
        pattern: "*".to_owned(),
        agg_cursor: Some("0:100:50".to_owned()),
        agg_page_size: Some(50),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).expect("serialize");
    assert!(json.contains(r#""agg_cursor":"0:100:50""#));
    assert!(json.contains(r#""agg_page_size":50"#));

    let parsed: SearchParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.agg_cursor.as_deref(), Some("0:100:50"));
    assert_eq!(parsed.agg_page_size, Some(50));
}

/// `SearchParams` omits cursor fields from JSON when they are `None`.
#[test]
fn search_params_cursor_fields_omitted_when_none() {
    let params = SearchParams {
        pattern: "*.rs".to_owned(),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).expect("serialize");
    assert!(!json.contains("agg_cursor"));
    assert!(!json.contains("agg_page_size"));
}

/// `AggregateResultWire` with a `next_cursor` value round-trips correctly.
#[test]
fn aggregate_result_wire_next_cursor_round_trip() {
    let result = AggregateResultWire {
        label: Some("ext_terms".to_owned()),
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![BucketWire {
            key: "rs".to_owned(),
            count: 500,
            total_bytes: 2_000_000,
            avg_size: Some(4_000.0_f64),
            ..BucketWire::default()
        }],
        other_count: Some(300),
        total_groups: Some(150),
        next_cursor: Some("0:50:50".to_owned()),
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains(r#""next_cursor":"0:50:50""#));

    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.next_cursor.as_deref(), Some("0:50:50"));
}

/// `AggregateResultWire` omits `next_cursor` from JSON when `None`.
#[test]
fn aggregate_result_wire_next_cursor_omitted_when_none() {
    let result = AggregateResultWire {
        label: None,
        kind: "count".to_owned(),
        field: None,
        value: Some(42),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(!json.contains("next_cursor"));
}

/// `exact` and `values_complete` round-trip through JSON.
#[test]
fn aggregate_result_wire_exact_and_values_complete_round_trip() {
    let result = AggregateResultWire {
        label: None,
        kind: "buckets".to_owned(),
        field: Some("extension".to_owned()),
        value: None,
        stats: None,
        buckets: vec![],
        other_count: Some(0),
        total_groups: Some(5),
        next_cursor: None,
        exact: Some(true),
        values_complete: Some(true),
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains(r#""exact":true"#), "json: {json}");
    assert!(json.contains(r#""values_complete":true"#), "json: {json}");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.exact, Some(true));
    assert_eq!(parsed.values_complete, Some(true));
}

/// `exact` and `values_complete` are omitted when `None`.
#[test]
fn aggregate_result_wire_exact_omitted_when_none() {
    let result = AggregateResultWire {
        label: None,
        kind: "count".to_owned(),
        field: None,
        value: Some(42),
        stats: None,
        buckets: vec![],
        other_count: None,
        total_groups: None,
        next_cursor: None,
        exact: None,
        values_complete: None,
    };
    let json = serde_json::to_string(&result).expect("serialize");
    assert!(!json.contains("exact"), "json: {json}");
    assert!(!json.contains("values_complete"), "json: {json}");
}

// ── from_cli_args output-field regression tests ─────────────────────

/// Regression: `--parity-compat` must set `output_columns` to `"parity"`
/// and `output_parity_compat` to `Some(true)`.
#[test]
fn from_cli_args_parity_compat_sets_output_columns() {
    let args: Vec<String> = vec!["*.*", "--format", "custom", "--parity-compat"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.output_columns.as_deref(),
        Some("parity"),
        "--parity-compat must set output_columns to parity"
    );
    assert_eq!(
        params.output_parity_compat,
        Some(true),
        "--parity-compat must set output_parity_compat"
    );
}

/// Regression: when no `--sep` / `--quotes` are provided, `output_separator`
/// and `output_quote` must be `None` so the daemon uses `OutputConfig`
/// defaults (`,` and `"`).  Previously these were `Some("")` which
/// caused the daemon to use empty separators — fields concatenated
/// with no delimiters.
#[test]
fn from_cli_args_default_output_separator_and_quote_are_none() {
    let args: Vec<String> = vec!["*.*"].into_iter().map(String::from).collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert!(
        params.output_separator.is_none(),
        "output_separator must be None when --sep not provided, got {:?}",
        params.output_separator
    );
    assert!(
        params.output_quote.is_none(),
        "output_quote must be None when --quotes not provided, got {:?}",
        params.output_quote
    );
    assert!(
        params.output_pos.is_none(),
        "output_pos must be None when --pos not provided, got {:?}",
        params.output_pos
    );
    assert!(
        params.output_neg.is_none(),
        "output_neg must be None when --neg not provided, got {:?}",
        params.output_neg
    );
}

/// Explicit `--sep` and `--quotes` must populate `Some(...)` values.
#[test]
fn from_cli_args_explicit_sep_and_quotes_populate_some() {
    let args: Vec<String> = vec!["*.*", "--sep", ";", "--quotes", "'"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.output_separator.as_deref(), Some(";"));
    assert_eq!(params.output_quote.as_deref(), Some("'"));
}

/// `--parity-compat` without explicit `--sep` / `--quotes` must leave
/// separator and quote as `None` (daemon uses `OutputConfig` defaults).
#[test]
fn from_cli_args_parity_compat_preserves_default_separator() {
    let args: Vec<String> = vec!["*.*", "--format", "custom", "--parity-compat"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert!(
        params.output_separator.is_none(),
        "parity-compat must not inject an empty separator"
    );
    assert!(
        params.output_quote.is_none(),
        "parity-compat must not inject an empty quote"
    );
}

/// `--header false` must set `output_header` to `Some(false)`.
#[test]
fn from_cli_args_header_false() {
    let args: Vec<String> = ["*.*", "--header", "false"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.output_header, Some(false));
}

/// `--header true` must set `output_header` to `Some(true)`.
#[test]
fn from_cli_args_header_true() {
    let args: Vec<String> = ["*.*", "--header", "true"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.output_header, Some(true));
}

/// `--pos` and `--neg` must populate `output_pos` / `output_neg`.
#[test]
fn from_cli_args_pos_neg_explicit() {
    let args: Vec<String> = ["*.*", "--pos", "+", "--neg", "-"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.output_pos.as_deref(), Some("+"));
    assert_eq!(params.output_neg.as_deref(), Some("-"));
}

/// `--out myfile.csv` must set `output_file` to an absolute path
/// (relative paths are canonicalized via `current_dir().join(...)`).
#[test]
fn from_cli_args_out_file() {
    let args: Vec<String> = ["*.*", "--out", "results.csv"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    let out = params.output_file.expect("output_file must be set");
    assert!(
        out.ends_with("results.csv"),
        "output_file must end with the filename, got: {out}"
    );
    assert!(
        std::path::Path::new(&out).is_absolute(),
        "output_file must be absolute, got: {out}"
    );
}

/// `--out console` must resolve to `None` (console output, no file).
/// Same for aliases: `con`, `term`, `terminal`.
#[test]
fn from_cli_args_out_console_resolves_to_none() {
    // "console" is the only alias that explicitly maps to None in
    // from_cli_args; other values ("con", "term", "terminal") are
    // treated as file names — the CLI layer handles those separately.
    let args: Vec<String> = ["*.*", "--out", "console"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert!(
        params.output_file.is_none(),
        "--out console must set output_file to None"
    );
}

/// `--columns path,name,size` must set `output_columns`.
#[test]
fn from_cli_args_columns_explicit() {
    let args: Vec<String> = ["*.*", "--columns", "path,name,size"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.output_columns.as_deref(), Some("path,name,size"));
}

/// `--columns all` must set `output_columns` to `"all"` (or None).
/// The daemon's `build_output_config` passes this to
/// `OutputConfig::with_columns` which returns `None` for `"all"`, so both are
/// acceptable.
#[test]
fn from_cli_args_columns_all() {
    let args: Vec<String> = ["*.*", "--columns", "all"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    // "all" stored as-is; OutputConfig::with_columns("all") handles it.
    assert_eq!(params.output_columns.as_deref(), Some("all"));
}

/// `--sep TAB` must be passed through verbatim to `output_separator`.
/// The daemon's `build_output_config` calls `OutputConfig::with_separator`
/// which expands `"TAB"` → `"\t"`.
#[test]
fn from_cli_args_sep_special_names_passed_through() {
    let args: Vec<String> = ["*.*", "--sep", "TAB"]
        .into_iter()
        .map(String::from)
        .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.output_separator.as_deref(),
        Some("TAB"),
        "--sep TAB must be stored as 'TAB'; expansion happens in OutputConfig"
    );
}

/// Combining all output options in a single invocation.
#[test]
fn from_cli_args_all_output_options_combined() {
    let args: Vec<String> = vec![
        "*.*",
        "--sep",
        ";",
        "--quotes",
        "'",
        "--header",
        "false",
        "--pos",
        "YES",
        "--neg",
        "NO",
        "--columns",
        "path,name,size,created",
        "--out",
        "dump.csv",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.output_separator.as_deref(), Some(";"));
    assert_eq!(params.output_quote.as_deref(), Some("'"));
    assert_eq!(params.output_header, Some(false));
    assert_eq!(params.output_pos.as_deref(), Some("YES"));
    assert_eq!(params.output_neg.as_deref(), Some("NO"));
    assert_eq!(
        params.output_columns.as_deref(),
        Some("path,name,size,created")
    );
    let out = params.output_file.expect("output_file must be set");
    assert!(
        out.ends_with("dump.csv"),
        "output_file must end with filename, got: {out}"
    );
}

/// `values_complete` is false when `other_count > 0`.
#[test]
fn aggregate_result_wire_values_complete_false() {
    let result = AggregateResultWire {
        label: None,
        kind: "buckets".to_owned(),
        field: Some("type".to_owned()),
        value: None,
        stats: None,
        buckets: vec![],
        other_count: Some(500),
        total_groups: Some(100),
        next_cursor: None,
        exact: Some(true),
        values_complete: Some(false),
    };
    let json = serde_json::to_string(&result).expect("serialize");
    let parsed: AggregateResultWire = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.values_complete, Some(false));
    assert_eq!(parsed.exact, Some(true));
}

// ── *.ext → ext-filter promotion regression tests ──────────────────
//
// These tests pin the parse-time rewrite introduced to restore the
// fat-CLI fast path for `*.<ext>` patterns.  They are paired with the
// dispatch-time safety net in `uffs_core::search::backend::search_index`.

/// Baseline: `*.dll` is promoted to `pattern="*" + ext=Some("dll")`
/// so the daemon routes through `numeric_top_n::ext_fast_path`.
#[test]
fn from_cli_args_ext_glob_promoted() {
    let args: Vec<String> = vec!["*.dll".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, "*",
        "*.dll must be rewritten to pattern=* so is_match_all dispatches to numeric_top_n"
    );
    assert_eq!(
        params.ext.as_deref(),
        Some("dll"),
        "*.dll must set ext=Some(dll) so ext_fast_path fires"
    );
}

/// Parity: `*.dll` and `* --ext dll` produce identical `SearchParams`
/// after parsing.  This is the guarantee that makes the promotion safe.
#[test]
fn from_cli_args_ext_glob_equivalent_to_explicit_ext_flag() {
    let glob_args: Vec<String> = vec!["*.dll".into()];
    let glob = SearchParams::from_cli_args(&glob_args).expect("parse");

    let explicit_args: Vec<String> = vec!["*".into(), "--ext".into(), "dll".into()];
    let explicit = SearchParams::from_cli_args(&explicit_args).expect("parse");

    assert_eq!(glob.pattern, explicit.pattern, "pattern parity");
    assert_eq!(glob.ext, explicit.ext, "ext parity");
    assert_eq!(
        glob.case_sensitive, explicit.case_sensitive,
        "case_sensitive parity"
    );
    assert_eq!(glob.match_path, explicit.match_path, "match_path parity");
}

/// `*.DLL` (uppercase) must lowercase the extension before inserting
/// into the filter — the `ExtensionIndex` is case-folded.
#[test]
fn from_cli_args_ext_glob_lowercases_extension() {
    let args: Vec<String> = vec!["*.DLL".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "*");
    assert_eq!(
        params.ext.as_deref(),
        Some("dll"),
        "extension must be lowercased for case-folded ExtensionIndex lookup"
    );
}

/// `*.tar.gz` is NOT a pure extension glob (dot in the rest) — must
/// stay on the trigram path unchanged.
#[test]
fn from_cli_args_multi_segment_ext_not_promoted() {
    let args: Vec<String> = vec!["*.tar.gz".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, "*.tar.gz",
        "multi-segment extension must not be promoted"
    );
    assert!(params.ext.is_none(), "ext must stay None");
}

/// `*.*` is the "any extension" glob — must NOT be promoted (would
/// change meaning from "has any extension" to "match-all").
#[test]
fn from_cli_args_any_ext_glob_not_promoted() {
    let args: Vec<String> = vec!["*.*".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, "*.*",
        "*.* must stay on trigram path (matches any file with dot)"
    );
    assert!(params.ext.is_none());
}

/// Case-sensitive mode (`--case *.DLL`) must NOT promote — the
/// `ExtensionIndex` is case-folded and would return all `.dll` files,
/// but the user explicitly asked for strict-case `DLL` match.
#[test]
fn from_cli_args_case_sensitive_ext_glob_not_promoted() {
    let args: Vec<String> = vec!["*.DLL".into(), "--case".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, "*.DLL",
        "--case *.DLL must stay on trigram path (case-folded index cannot match strict case)"
    );
    assert!(params.ext.is_none());
    assert!(params.case_sensitive);
}

/// Explicit `--ext` must not be clobbered: `*.dll --ext exe` keeps
/// the user's explicit filter and leaves the pattern alone.
#[test]
fn from_cli_args_explicit_ext_preserved() {
    let args: Vec<String> = vec!["*.dll".into(), "--ext".into(), "exe".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    // User explicitly said --ext exe — promotion must not touch that.
    assert_eq!(params.ext.as_deref(), Some("exe"));
    // Pattern stays as-is since we did NOT promote (ext was already set).
    assert_eq!(params.pattern, "*.dll");
}

/// `path:*.dll` (path-scope) must NOT promote — the glob matches the
/// full path, not just the filename extension.
#[test]
fn from_cli_args_path_scope_ext_glob_not_promoted() {
    let args: Vec<String> = vec!["path:*.dll".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, "*.dll",
        "path: scope strips prefix but pattern stays *.dll (path-aware)"
    );
    assert!(params.match_path);
    assert!(params.ext.is_none());
}

// ── <letter>: → drive-filter promotion regression tests ──────────────
//
// These tests pin the parse-time rewrite that promotes a bare drive
// prefix (`C:<rest>`) into the `drive` filter and leaves `<rest>` for
// downstream sugar (including the ext-glob promotion).  They are paired
// with the dispatch-time safety net in
// `uffs_core::search::backend::{search_index, MultiDriveBackend::search}`.

/// Baseline composition: `C:*.dll` → drive=C + pattern=`*` + ext=`dll`.
/// Both the drive-prefix sugar AND the ext-glob promotion fire in the
/// same parse, confirming they compose correctly in the documented order.
#[test]
fn from_cli_args_drive_prefix_with_ext_glob_promotes_both() {
    let args: Vec<String> = vec!["C:*.dll".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.drives,
        vec![uffs_mft::platform::DriveLetter::C],
        "drive prefix must populate drives filter"
    );
    assert_eq!(
        params.pattern, "*",
        "ext-glob promotion must fire on the stripped rest (*.dll)"
    );
    assert_eq!(
        params.ext.as_deref(),
        Some("dll"),
        "lowercase extension must be extracted"
    );
    assert!(!params.match_path);
    assert!(!params.case_sensitive);
}

/// Non-glob rest: `D:notepad.exe` → drive=D + pattern=`notepad.exe`.
/// Only the drive-prefix sugar fires; the ext-glob promotion is gated
/// on `is_pure_ext_glob` which rejects literal patterns.
#[test]
fn from_cli_args_drive_prefix_with_literal_preserves_pattern() {
    let args: Vec<String> = vec!["D:notepad.exe".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::D]);
    assert_eq!(
        params.pattern, "notepad.exe",
        "literal pattern must be left alone after prefix strip"
    );
    assert!(
        params.ext.is_none(),
        "literal pattern must NOT trigger ext-glob promotion"
    );
}

/// `C:\*.dll` scopes to drive C and globs.  (Previously this returned
/// nothing: the `c:` token was mistaken for a directory segment by the
/// tree walker.)  The drive prefix is split off, the single leftover
/// segment `*.dll` collapses to a name glob, and ext-glob promotion then
/// rewrites it to `pattern="*"` + `ext="dll"`.
#[test]
fn from_cli_args_drive_root_glob_scopes_drive_and_promotes_ext() {
    let args: Vec<String> = vec!["C:\\*.dll".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.drives,
        vec![uffs_mft::platform::DriveLetter::C],
        "C:\\*.dll must scope to drive C"
    );
    assert_eq!(params.pattern, "*");
    assert_eq!(params.ext.as_deref(), Some("dll"));
}

/// Case normalisation: `c:*.log` → drive uppercased to `'C'`, ext
/// lowercased to `"log"`.  Drive letters on NTFS are case-insensitive.
#[test]
fn from_cli_args_drive_prefix_lowercase_letter_is_uppercased() {
    let args: Vec<String> = vec!["c:*.log".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.drives,
        vec![uffs_mft::platform::DriveLetter::C],
        "drive letter must be uppercased regardless of input case"
    );
    assert_eq!(params.pattern, "*");
    assert_eq!(params.ext.as_deref(), Some("log"));
}

/// Explicit `--drive` wins over the inferred prefix.  The prefix is
/// still stripped (the `C:` is sugar, not part of the intended needle),
/// but the drive assignment honours the user's explicit flag.
#[test]
fn from_cli_args_drive_prefix_respects_explicit_drive_flag() {
    let args: Vec<String> = vec!["--drive".into(), "D".into(), "C:*.dll".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.drives,
        vec![uffs_mft::platform::DriveLetter::D],
        "explicit --drive D must win over inferred C prefix"
    );
    // Prefix is still stripped; ext-glob promotion still fires on the rest.
    assert_eq!(params.pattern, "*");
    assert_eq!(params.ext.as_deref(), Some("dll"));
}

// ── Regex alternation → ext-filter promotion regression tests ───────
//
// These tests pin the parse-time rewrite that promotes pure trailing
// extension alternations in regex patterns (`>.*\.(a|b|c)$`) to the
// match-all + `--ext a,b,c` shape.  Paired with the dispatch-time
// safety net in
// `uffs_core::search::dispatch::apply_dispatch_safety_nets` rewrite #3.

/// Baseline: `>.*\.(jpg|png|heic)$` is promoted to `pattern="*"` +
/// `ext=Some("jpg,png,heic")` so the daemon routes through
/// `numeric_top_n::ext_fast_path` instead of compiling a regex and
/// scanning every record.
#[test]
fn from_cli_args_regex_alternation_promoted() {
    let args: Vec<String> = vec![">.*\\.(jpg|png|heic)$".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, "*",
        "regex alternation must be rewritten to match-all for ext_fast_path"
    );
    assert_eq!(
        params.ext.as_deref(),
        Some("jpg,png,heic"),
        "extensions must be extracted and joined in CSV form"
    );
}

/// Single-extension anchored regex `>.*\.rs$` is equivalent to `*.rs`
/// — both must land on the same `(pattern=*, ext=rs)` shape.
#[test]
fn from_cli_args_regex_single_ext_promoted() {
    let args: Vec<String> = vec![">.*\\.rs$".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "*");
    assert_eq!(params.ext.as_deref(), Some("rs"));
}

/// Parity: `>.*\.(jpg|png)$` and `* --ext jpg,png` produce identical
/// `SearchParams` after parsing.  Guarantees the rewrite is lossless.
#[test]
fn from_cli_args_regex_alternation_equivalent_to_explicit_ext_flag() {
    let regex_args: Vec<String> = vec![">.*\\.(jpg|png)$".into()];
    let regex = SearchParams::from_cli_args(&regex_args).expect("parse");

    let explicit_args: Vec<String> = vec!["*".into(), "--ext".into(), "jpg,png".into()];
    let explicit = SearchParams::from_cli_args(&explicit_args).expect("parse");

    assert_eq!(regex.pattern, explicit.pattern, "pattern parity");
    assert_eq!(regex.ext, explicit.ext, "ext parity");
    assert_eq!(
        regex.case_sensitive, explicit.case_sensitive,
        "case_sensitive parity"
    );
    assert_eq!(regex.match_path, explicit.match_path, "match_path parity");
}

/// Uppercase extensions in the regex must be lowercased before going
/// into the CSV filter — the `ExtensionIndex` is case-folded.
#[test]
fn from_cli_args_regex_alternation_lowercases_extensions() {
    let args: Vec<String> = vec![">.*\\.(JPG|PNG)$".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "*");
    assert_eq!(
        params.ext.as_deref(),
        Some("jpg,png"),
        "uppercase regex extensions must be lowercased in the CSV"
    );
}

/// Missing `$` anchor: `>.*\.jpg` matches `.jpg` **anywhere** in the
/// name, which the ext-index cannot replicate.  Must stay on the
/// regex scan path.
#[test]
fn from_cli_args_regex_without_dollar_not_promoted() {
    let args: Vec<String> = vec![">.*\\.jpg".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(
        params.pattern, ">.*\\.jpg",
        "regex without $ anchor must stay on the regex path"
    );
    assert!(params.ext.is_none());
}

/// Multi-segment extension via escaped dot: `>.*\.(tar\.gz|zip)$` has
/// a literal dot inside the alternation — reject the promotion because
/// the ext-index only matches the trailing segment.
#[test]
fn from_cli_args_regex_multi_segment_alternation_not_promoted() {
    let args: Vec<String> = vec![">.*\\.(tar\\.gz|zip)$".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, ">.*\\.(tar\\.gz|zip)$");
    assert!(params.ext.is_none());
}

/// Wildcard inside alternation: `>.*\.(jp.?|png)$` has `.?` inside
/// the alternation — reject the promotion.
#[test]
fn from_cli_args_regex_alternation_with_wildcard_not_promoted() {
    let args: Vec<String> = vec![">.*\\.(jp.?|png)$".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, ">.*\\.(jp.?|png)$");
    assert!(params.ext.is_none());
}

/// Case-sensitive mode (`--case >.*\.(JPG|PNG)$`) must NOT promote —
/// the `ExtensionIndex` is case-folded and would match `.jpg` too.
#[test]
fn from_cli_args_regex_case_sensitive_not_promoted() {
    let args: Vec<String> = vec![">.*\\.(JPG|PNG)$".into(), "--case".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, ">.*\\.(JPG|PNG)$");
    assert!(params.ext.is_none());
    assert!(params.case_sensitive);
}

/// Explicit `--ext` must not be clobbered.  The regex alternation
/// is left intact so the user's explicit filter controls the result.
#[test]
fn from_cli_args_regex_explicit_ext_preserved() {
    let args: Vec<String> = vec![">.*\\.(jpg|png)$".into(), "--ext".into(), "exe".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.ext.as_deref(), Some("exe"));
    assert_eq!(params.pattern, ">.*\\.(jpg|png)$");
}

// ── --no-output precedence tests (Phase 3.1 NUL fast path) ─────────
//
// The thin CLI injects `--no-output` when it detects that stdout is
// redirected to the null device.  The flag sets `include_rows = false`
// so the daemon skips row materialisation + `paths_blob` construction
// + shmem offload.  These four tests pin every corner of the
// precedence table documented above the `include_rows` assignment in
// `cli_args.rs::into_search_params`.

/// Default: no `--no-output`, no `--agg`, no `--rows` → rows included.
#[test]
fn from_cli_args_include_rows_default_is_true() {
    let params = SearchParams::from_cli_args(&["*".to_owned()]).expect("parse");
    assert!(
        params.include_rows,
        "plain search must include rows by default"
    );
}

/// `--no-output` with no aggregation and no `--rows` → rows suppressed.
/// This is the hot path for the NUL-redirected stdout case.
#[test]
fn from_cli_args_no_output_suppresses_rows() {
    let params =
        SearchParams::from_cli_args(&["*".to_owned(), "--no-output".to_owned()]).expect("parse");
    assert!(
        !params.include_rows,
        "--no-output must set include_rows = false so the daemon skips materialisation"
    );
}

/// `--no-output --rows`: explicit `--rows` is higher precedence.  This
/// only happens if a user manually invokes both (CLI auto-injection
/// only adds `--no-output`, never `--rows`), but the precedence must
/// still be deterministic.
#[test]
fn from_cli_args_rows_overrides_no_output() {
    let params = SearchParams::from_cli_args(&[
        "*".to_owned(),
        "--no-output".to_owned(),
        "--rows".to_owned(),
    ])
    .expect("parse");
    assert!(
        params.include_rows,
        "--rows must override --no-output (explicit intent wins over auto-injection)"
    );
}

/// `--agg count --no-output`: aggregation alone already forces
/// `include_rows = false`; adding `--no-output` does not change the
/// result but also must not flip back to `true`.
#[test]
fn from_cli_args_no_output_with_aggregation_stays_false() {
    let params = SearchParams::from_cli_args(&[
        "*".to_owned(),
        "--agg".to_owned(),
        "count".to_owned(),
        "--no-output".to_owned(),
    ])
    .expect("parse");
    assert!(
        !params.include_rows,
        "aggregate-only query must still suppress rows when --no-output is also set"
    );
    assert_eq!(
        params.aggregations.len(),
        1,
        "aggregation must still be forwarded to the daemon"
    );
}

// ── Phase 8-A: tiering RPC wire-format round-trips ─────────────────
//
// These tests pin the wire format for the four new methods scaffolded
// in Phase 8-A (`hibernate` / `preload` / `forget` / `status_drives`).
// Each test runs encode → decode and asserts every field round-trips
// faithfully, matching the harness style of `request_round_trip` and
// `search_params_round_trip` above.  The dispatcher-level
// `ERR_NOT_IMPLEMENTED` behaviour is exercised by the daemon-side
// integration tests; here we lock the data shapes the handlers will
// fill in during sub-phases 8-B / 8-C / 8-D / 8-E.

use crate::protocol::response::{
    DEFAULT_PRELOAD_PIN_MINUTES, DriveTierStatus, ForgetParams, ForgetResponse, HibernateParams,
    HibernateResponse, PreloadParams, PreloadResponse, StatusDrivesParams, StatusDrivesResponse,
};
use crate::protocol::{ERR_DRIVE_BUSY, ERR_NOT_IMPLEMENTED};

/// Lock the numeric values of the two new application error codes so a
/// future protocol-version bump cannot silently renumber them.  Wire
/// callers (uffs-mcp, third-party scripts) match on the integer code,
/// not the message — renumbering would be a hard breakage.
#[test]
fn tiering_error_codes_have_stable_values() {
    assert_eq!(
        ERR_NOT_IMPLEMENTED, -3_i32,
        "ERR_NOT_IMPLEMENTED is the wire contract for the Phase 8-A scaffolding stubs; \
         renumbering would break every client that already grepped daemon logs for it"
    );
    assert_eq!(
        ERR_DRIVE_BUSY, -4_i32,
        "ERR_DRIVE_BUSY is reserved for the Phase 8-D forget refusal path; \
         renumbering would break clients matching on the code"
    );
}

/// Default-preload-pin-minutes constant pins the wire-level default a
/// caller sees when [`PreloadParams::pin_minutes`] is `None`.  Changing
/// it is a wire-format change because operators script around it.
#[test]
fn default_preload_pin_minutes_is_thirty() {
    assert_eq!(
        DEFAULT_PRELOAD_PIN_MINUTES, 30_u32,
        "30-minute default is documented in the memory-tiering plan §5.1 \
         sub-phase 8-C and in user-facing CLI help — change requires plan + docs update"
    );
}

#[test]
fn hibernate_params_round_trip() {
    let params = HibernateParams {
        drives: vec![
            uffs_mft::platform::DriveLetter::C,
            uffs_mft::platform::DriveLetter::D,
            uffs_mft::platform::DriveLetter::E,
        ],
    };
    let json = serde_json::to_string(&params).expect("serialize");
    let parsed: HibernateParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, params);
}

/// Empty `drives` ⇒ "every loaded drive" wire convention.  Default
/// must round-trip cleanly so the daemon's `serde(default)` picks up
/// the empty vector when callers omit the field entirely.
#[test]
fn hibernate_params_empty_drives_means_all() {
    let parsed: HibernateParams = serde_json::from_str("{}").expect("deserialize");
    assert!(
        parsed.drives.is_empty(),
        "omitted drives field deserialises to empty vec (= every loaded drive)"
    );
}

#[test]
fn hibernate_response_round_trip() {
    let resp = HibernateResponse {
        hot_demoted: vec![uffs_mft::platform::DriveLetter::C],
        warm_demoted: vec![
            uffs_mft::platform::DriveLetter::D,
            uffs_mft::platform::DriveLetter::E,
        ],
        parked_demoted: vec![uffs_mft::platform::DriveLetter::F],
        already_cold: vec![
            uffs_mft::platform::DriveLetter::G,
            uffs_mft::platform::DriveLetter::H,
        ],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: HibernateResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, resp);
}

#[test]
fn preload_params_round_trip() {
    let params = PreloadParams {
        drives: vec![uffs_mft::platform::DriveLetter::C],
        pin_minutes: Some(60_u32),
    };
    let json = serde_json::to_string(&params).expect("serialize");
    let parsed: PreloadParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, params);
}

/// Omitted `pin_minutes` deserialises to `None` so the daemon applies
/// [`DEFAULT_PRELOAD_PIN_MINUTES`].  Locks the on-the-wire optionality
/// shape: `null` and absence are both valid no-pin-override signals.
#[test]
fn preload_params_pin_minutes_optional() {
    let from_omit: PreloadParams =
        serde_json::from_str(r#"{"drives":["C"]}"#).expect("deserialize");
    assert_eq!(from_omit.pin_minutes, None);
    let from_null: PreloadParams =
        serde_json::from_str(r#"{"drives":["C"],"pin_minutes":null}"#).expect("deserialize");
    assert_eq!(from_null.pin_minutes, None);
}

#[test]
fn preload_response_round_trip() {
    let resp = PreloadResponse {
        promoted: vec![uffs_mft::platform::DriveLetter::C],
        already_hot: vec![uffs_mft::platform::DriveLetter::D],
        errors: vec!["Z: cache file missing".to_owned()],
        pin_until_unix_ms: 1_800_000_000_000_i64,
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: PreloadResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, resp);
}

#[test]
fn forget_params_round_trip() {
    let params = ForgetParams {
        drives: vec![uffs_mft::platform::DriveLetter::Z],
        force: true,
    };
    let json = serde_json::to_string(&params).expect("serialize");
    let parsed: ForgetParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, params);
}

/// Default `force = false` is the safe path: omitted field
/// deserialises to `false`, so the daemon refuses to forget a non-
/// `Cold` drive unless the operator explicitly opts into the
/// auto-hibernate side effect.
#[test]
fn forget_params_force_defaults_false() {
    let parsed: ForgetParams = serde_json::from_str(r#"{"drives":["Z"]}"#).expect("deserialize");
    assert!(
        !parsed.force,
        "force defaults to false so destructive auto-hibernate is opt-in"
    );
}

#[test]
fn forget_response_round_trip() {
    let resp = ForgetResponse {
        forgotten: vec![uffs_mft::platform::DriveLetter::Z],
        already_absent: vec![uffs_mft::platform::DriveLetter::Y],
        freed_bytes: 1_234_567_890_u64,
        errors: vec!["X: permission denied".to_owned()],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: ForgetResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, resp);
}

/// `StatusDrivesParams` is intentionally fieldless today; round-tripping
/// `{}` keeps the door open for adding fields in a wire-compatible way
/// (existing clients send `{}`, future clients add fields, the daemon
/// honours `serde(default)` for missing ones).
#[test]
fn status_drives_params_empty_round_trip() {
    let params = StatusDrivesParams {};
    let json = serde_json::to_string(&params).expect("serialize");
    let parsed: StatusDrivesParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, params);
}

#[test]
fn status_drives_response_round_trip() {
    let resp = StatusDrivesResponse {
        drives: vec![
            DriveTierStatus {
                letter: uffs_mft::platform::DriveLetter::C,
                tier: "hot".to_owned(),
                resident_bytes: 1_073_741_824_u64, // 1 GiB
                query_rate_per_min: 12.5_f64,
                last_query_at_ms: 1_700_000_000_000_i64,
                promotions_total: 3_u64,
                pin_until_unix_ms: 1_700_001_800_000_i64,
                loading: false,
            },
            DriveTierStatus {
                letter: uffs_mft::platform::DriveLetter::D,
                tier: "cold".to_owned(),
                resident_bytes: 0_u64,
                query_rate_per_min: 0.0_f64,
                last_query_at_ms: 0_i64,
                promotions_total: 0_u64,
                pin_until_unix_ms: 0_i64,
                loading: true,
            },
        ],
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: StatusDrivesResponse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, resp);
    // Float comparison via the parent assert_eq! works because PartialEq
    // on f64 is bit-identical for in-memory round-trips at this size;
    // pin a hard guard on the first drive to make the intent explicit.
    assert!(
        (parsed.drives[0].query_rate_per_min - 12.5).abs() < f64::EPSILON,
        "query_rate_per_min round-trips bit-identical for in-memory JSON"
    );
}

/// End-to-end RPC envelope round-trip: a request encoded with the new
/// `hibernate` method name decodes correctly and dispatches by string
/// match (mirrors `request_round_trip` for `search`).  Catches a
/// future regression where someone wraps the method name in an enum
/// without a `serde(rename_all = "snake_case")` and breaks the wire
/// contract.
#[test]
fn hibernate_request_envelope_round_trip() {
    let req = RpcRequest::new(
        7_u64,
        "hibernate",
        Some(serde_json::json!({"drives": ["C", "D"]})),
    );
    let json = serde_json::to_string(&req).expect("serialize");
    let parsed: RpcRequest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.method, "hibernate");
    assert_eq!(parsed.id, Some(7));
    let inner: HibernateParams = serde_json::from_value(parsed.params.expect("params present"))
        .expect("nested params decode");
    assert_eq!(inner.drives, vec![
        uffs_mft::platform::DriveLetter::C,
        uffs_mft::platform::DriveLetter::D
    ]);
}

// ── Drive-prefix pattern parsing (split_drive_prefix front door) ───
//
// Regression tests for the `C:` / `C:\` / `C:\path` forms that used to
// fall through to a literal `c:` substring (matching `.heiC:${…}` ADS
// streams) or to the tree walker's bogus `c:` directory segment.

/// `C:` → drive C + everything (match-all).
#[test]
fn from_cli_args_bare_drive_is_match_all_on_drive() {
    let args: Vec<String> = vec!["C:".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "*");
    assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
}

/// `C:\` and `C:/` are identical to `C:` — drive C + match-all.
#[test]
fn from_cli_args_drive_root_is_match_all_on_drive() {
    for raw in ["C:\\", "C:/"] {
        let args: Vec<String> = vec![raw.into()];
        let params = SearchParams::from_cli_args(&args).expect("parse");
        assert_eq!(params.pattern, "*", "`{raw}` → match-all");
        assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
    }
}

/// `C:\report` collapses to the same query as `C:report`.
#[test]
fn from_cli_args_drive_root_single_segment_matches_bare_drive_name() {
    let args: Vec<String> = vec!["C:\\report".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "report");
    assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
}

/// `C:\report\*` → drive C + tree-walk body `report\*`.
#[test]
fn from_cli_args_drive_plus_multi_segment_is_tree_body() {
    let args: Vec<String> = vec!["C:\\report\\*".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "report\\*");
    assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
}

/// No-drive path patterns are untouched and stay un-scoped.
#[test]
fn from_cli_args_no_drive_path_pattern_unscoped() {
    let args: Vec<String> = vec!["\\rnio\\*".into()];
    let params = SearchParams::from_cli_args(&args).expect("parse");
    assert_eq!(params.pattern, "\\rnio\\*");
    assert_eq!(params.drives, Vec::<uffs_mft::platform::DriveLetter>::new());
}
