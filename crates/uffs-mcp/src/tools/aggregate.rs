// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs_aggregate` tool — server-side aggregation summaries.

use core::fmt::Write as _;

use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use uffs_client::connect::UffsClient;
use uffs_client::protocol::{AggregateSpecWire, SearchParams};

use crate::error::BridgeError;
use crate::roots::{self, RootsState};
use crate::text::{format_aggregate_summary, format_scan_header};

/// Maximum character length for the text response before truncation.
///
/// Keeps aggregate output safely under MCP host tool-result size limits.
/// The summary portion is always included; the JSON block is truncated
/// with a warning when the combined output would exceed this cap.
const MAX_TEXT_CHARS: usize = 50_000;

/// Input parameters for the `uffs_aggregate` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AggregateArgs {
    /// Search pattern to scope aggregation (default: `*`).
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// Named preset (`overview`, `by_type`, `by_extension`, `by_drive`,
    /// `by_size`, `by_age`, `storage`, `activity`, `top_folders`,
    /// `duplicates`, `media`, `cleanup`).
    #[serde(default)]
    pub preset: Option<String>,
    /// Custom aggregate specs in power syntax (e.g. `terms:extension,top=50`).
    #[serde(default)]
    pub aggregations: Vec<String>,
    /// Limit to specific drive letters.
    #[serde(default)]
    pub drives: Vec<String>,
    /// Filter: `all`, `files`, `dirs`.
    #[serde(default)]
    pub filter: Option<String>,
    /// File type category (e.g. `"code"`, `"document"`, `"picture"`,
    /// `"video"`, `"audio"`, `"executable"`, `"system"`).
    #[serde(default)]
    pub type_filter: Option<String>,
    /// Opaque cursor from a previous response's `next_cursor` for bucket
    /// pagination.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Maximum buckets per page.  When set, enables paginated bucket
    /// responses with a `next_cursor` in the output.
    #[serde(default)]
    pub page_size: Option<u16>,
}

/// Default pattern.
fn default_pattern() -> String {
    "*".to_owned()
}

/// Parse the tool's drive-name strings (`"C"`, `"C:"`, …) into typed
/// letters, silently dropping anything unparseable — a bad drive arg
/// scopes to nothing rather than failing the whole aggregation.
fn parse_drive_letters(drives: &[String]) -> Vec<uffs_mft::platform::DriveLetter> {
    drives
        .iter()
        .filter_map(|drv| drv.chars().next())
        .filter_map(|ch| uffs_mft::platform::DriveLetter::parse(ch).ok())
        .collect()
}

/// Execute the aggregate tool.
///
/// # Errors
///
/// Returns [`BridgeError`] if the daemon call fails.
pub(crate) async fn run(
    client: &mut UffsClient,
    args: AggregateArgs,
    roots_state: &RootsState,
) -> Result<CallToolResult, BridgeError> {
    let mut agg_specs: Vec<AggregateSpecWire> = Vec::new();

    // Handle preset parameter.
    if let Some(preset) = &args.preset {
        agg_specs.push(AggregateSpecWire {
            kind: "preset".to_owned(),
            preset: Some(preset.clone()),
            ..default_agg_spec()
        });
    }

    // Handle custom aggregations array.
    for spec_str in &args.aggregations {
        agg_specs.push(AggregateSpecWire {
            kind: "raw".to_owned(),
            label: Some(spec_str.clone()),
            ..default_agg_spec()
        });
    }

    // Default to overview if no specs given.
    if agg_specs.is_empty() {
        agg_specs.push(AggregateSpecWire {
            kind: "preset".to_owned(),
            preset: Some("overview".to_owned()),
            ..default_agg_spec()
        });
    }

    let drives = parse_drive_letters(&args.drives);

    let mut params = SearchParams {
        pattern: args.pattern,
        aggregations: agg_specs,
        include_rows: false,
        // Limit to 1 row — the daemon ignores `include_rows` and always
        // collects rows.  Without an explicit limit, pattern="*" causes
        // the daemon to collect ALL 25M+ matching rows (22+ seconds)
        // before even starting the aggregation.  `limit: Some(1)` makes
        // the row-collection phase nearly instant.
        limit: Some(1),
        drives,
        filter: args.filter,
        type_filter: args.type_filter,
        agg_cursor: args.cursor,
        agg_page_size: args.page_size,
        ..Default::default()
    };

    // Apply roots-based scoping (drive + path prefix) when no explicit drives.
    roots::apply_roots_scope(roots_state, &mut params);

    // Cold-index contract: return a retryable "warming" error instead of
    // blocking silently for the length of a re-warm (see `tools::warm`).
    // Full params, not just the drive scope: the daemon's plan consults
    // the query's resolved ext/type filter against each Parked bloom.
    super::warm::warm_gate(client, &params).await?;

    // Log the exact RPC payload for debugging parity with API validation.
    tracing::info!(
        params_json = %serde_json::to_string(&params).unwrap_or_default(),
        "uffs_aggregate: sending search request to daemon"
    );

    let response = client
        .search(&params)
        .await
        .map_err(|err| BridgeError::Daemon(format!("Aggregate failed: {err}")))?;

    tracing::info!(
        records_scanned = response.records_scanned,
        duration_ms = response.duration_ms,
        agg_count = response.aggregations.len(),
        // Aggregate requests pass `include_rows: false`, so the
        // payload is almost always `Empty` (row_count_hint() == Some(0)).
        // Still use `row_count_hint` for correctness — a legacy caller
        // that leaves `include_rows: true` will see the actual count.
        row_count = response.payload.row_count_hint().unwrap_or(0),
        "uffs_aggregate: daemon response received"
    );

    let summary = format!(
        "{}\n{}",
        format_scan_header(response.records_scanned, response.duration_ms),
        format_aggregate_summary(&response.aggregations)
    );

    // Extract the first non-None next_cursor from the aggregation results.
    let next_cursor = response
        .aggregations
        .iter()
        .find_map(|agg| agg.next_cursor.clone());

    let structured = crate::schemas::AggregateOutput {
        records_scanned: response.records_scanned,
        duration_ms: response.duration_ms,
        aggregations: serde_json::to_value(&response.aggregations)?,
        next_cursor,
    };

    let json = serde_json::to_string_pretty(&structured)?;

    // Build the text response.  The summary (human-readable bullets) is
    // always included in full.  The JSON block is truncated when the
    // combined output would exceed MAX_TEXT_CHARS, with a warning
    // telling the agent how much was omitted.
    let full_json_len = json.len();
    let preamble = format!("{summary}\n\n```json\n");
    let suffix = "\n```";
    let budget = MAX_TEXT_CHARS.saturating_sub(preamble.len() + suffix.len());

    let output = if full_json_len <= budget {
        format!("{preamble}{json}{suffix}")
    } else {
        // Truncate JSON to budget and append a warning.
        #[expect(
            clippy::string_slice,
            reason = "floor_char_boundary guarantees valid UTF-8 boundary"
        )]
        let truncated = &json[..json.floor_char_boundary(budget.saturating_sub(200))];
        let omitted = full_json_len.saturating_sub(truncated.len());
        let mut out = preamble;
        out.push_str(truncated);
        _ = write!(
            out,
            "\n... (truncated — {omitted} of {full_json_len} chars omitted. \
             Use more specific pattern, type_filter, or drives to narrow results.)"
        );
        out.push_str(suffix);
        out
    };

    let mut result = CallToolResult::success(vec![ContentBlock::text(output)]);
    result.structured_content = Some(serde_json::to_value(structured)?);
    Ok(result)
}

/// Create a default `AggregateSpecWire` with all optional fields zeroed.
const fn default_agg_spec() -> AggregateSpecWire {
    AggregateSpecWire {
        kind: String::new(),
        label: None,
        field: None,
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec![],
        preset: None,
        sample: None,
        sample_sort: None,
        sample_desc: None,
        verify: None,
        verify_bytes: None,
        drive: None,
    }
}
