// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs_search` tool — file search across all indexed NTFS drives.

use core::fmt::Write as _;

use rmcp::model::{CallToolResult, ContentBlock, Resource};
use schemars::JsonSchema;
use serde::Deserialize;
use uffs_client::connect::UffsClient;
use uffs_client::protocol::SearchParams;

use crate::error::BridgeError;
use crate::roots::{self, RootsState};
use crate::schemas::{SearchOutput, SearchRowOutput};

/// Maximum inline rows the MCP surface will return per page.
///
/// 100 rows is the practical ceiling for LLM consumption.  Beyond ~50 rows
/// the model can no longer reason about individual entries — the right tool
/// becomes `uffs_aggregate` (duplicates, `by_extension`, etc.).  Keeping the
/// cap low also avoids exceeding host tool-result size limits (Claude Code
/// rejects results > ~200 KB).
pub(crate) const HARD_CAP: u32 = 100;

/// Maximum number of `resource_link` entries emitted in the response.
///
/// Resource links let host UIs offer clickable detail views, but emitting
/// one per row at scale balloons the payload for no model benefit.  Cap at
/// 20 to stay useful without bloating.
const MAX_RESOURCE_LINKS: usize = 20;

/// When total matches exceed `limit` by this factor, append a hint
/// suggesting the agent use `uffs_aggregate` instead of paging.
const AGGREGATE_HINT_FACTOR: u64 = 5;

/// Input parameters for the `uffs_search` tool.
///
/// Exposes the full `SearchParams` surface so agents can use any combination
/// of pattern, filters, sorts, and scoping — matching CLI/API parity.
#[expect(
    clippy::struct_excessive_bools,
    reason = "MCP schema mirrors SearchParams which has many boolean filter flags"
)]
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SearchArgs {
    // ── Core search ───────────────────────────────────────────────
    /// Search pattern (glob, regex with `>` prefix, or substring).
    pub pattern: String,
    /// Case-sensitive matching (default: false).
    #[serde(default)]
    pub case_sensitive: bool,
    /// The whole file name must match the pattern, not just contain it
    /// (default: false).
    #[serde(default)]
    pub whole_word: bool,
    /// Match pattern against the full path instead of the file name.
    #[serde(default)]
    pub match_path: bool,

    // ── Sorting ───────────────────────────────────────────────────
    /// Sort field(s): `name`, `size`, `modified`, `created`, `path`,
    /// `extension`, `drive`, `treesize`, `descendants`, `bulkiness`, etc.
    /// Comma-separated for multi-sort; prefix with `-` for descending.
    #[serde(default = "default_sort")]
    pub sort: String,
    /// Sort descending.  Applies to the first sort field when no per-field
    /// direction is specified.  Default: `false` (ascending), matching CLI
    /// behaviour — numeric/date fields still default to descending when
    /// used as secondary sort columns.
    #[serde(default)]
    pub sort_desc: bool,

    // ── Paging ────────────────────────────────────────────────────
    /// Maximum results to return per page (default: 50, hard cap: 100).
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Opaque cursor from a previous response to fetch the next page.
    #[serde(default)]
    pub cursor: Option<String>,

    // ── Type / scope ──────────────────────────────────────────────
    /// Filter: `all`, `files`, `dirs`.
    #[serde(default = "default_filter")]
    pub filter: String,
    /// Limit to specific drive letters (e.g. `["C", "D"]`).
    #[serde(default)]
    pub drives: Vec<String>,
    /// Hide system/metafiles (names starting with `$`).
    #[serde(default)]
    pub hide_system: bool,

    // ── Extension / exclude / path ────────────────────────────────
    /// Filter by file extension(s), comma-separated (e.g. `"rs"`, `"jpg,png"`).
    #[serde(default)]
    pub ext: Option<String>,
    /// Exclude files matching this glob pattern (e.g. `"*.tmp"`).
    #[serde(default)]
    pub exclude: Option<String>,
    /// Only include results whose directory path matches this glob.
    #[serde(default)]
    pub path_contains: Option<String>,
    /// Exclude results whose directory path matches ANY of these globs
    /// (comma-separated, e.g. `"*appdata*,*.cargo*,*.rustup*"`). The one-call
    /// way to strip noise dirs — inverse of `path_contains`.
    #[serde(default)]
    pub path_excludes: Option<String>,

    // ── Size filters ──────────────────────────────────────────────
    /// Minimum file size in bytes.
    #[serde(default)]
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    #[serde(default)]
    pub max_size: Option<u64>,
    /// Minimum allocated size (size-on-disk) in bytes.
    #[serde(default)]
    pub min_allocated: Option<u64>,
    /// Maximum allocated size (size-on-disk) in bytes.
    #[serde(default)]
    pub max_allocated: Option<u64>,

    // ── Time filters ──────────────────────────────────────────────
    /// Modified after (e.g. `"7d"`, `"30d"`, `"2025-01-01"`, `"today"`).
    #[serde(default)]
    pub newer: Option<String>,
    /// Modified before (e.g. `"365d"`, `"last_year"`).
    #[serde(default)]
    pub older: Option<String>,
    /// Created after.
    #[serde(default)]
    pub newer_created: Option<String>,
    /// Created before.
    #[serde(default)]
    pub older_created: Option<String>,
    /// Accessed after.
    #[serde(default)]
    pub newer_accessed: Option<String>,
    /// Accessed before.
    #[serde(default)]
    pub older_accessed: Option<String>,

    // ── Attribute filters ─────────────────────────────────────────
    /// NTFS attribute filter (e.g. `"hidden"`, `"system,!hidden"`,
    /// `"compressed"`).
    #[serde(default)]
    pub attr: Option<String>,

    // ── Type category filter ──────────────────────────────────────
    /// File type category (e.g. `"code"`, `"document"`, `"picture"`,
    /// `"executable"`, `"system"`).
    #[serde(default)]
    pub type_filter: Option<String>,

    // ── Directory metrics ─────────────────────────────────────────
    /// Minimum number of descendants (for directories).
    #[serde(default)]
    pub min_descendants: Option<u32>,
    /// Maximum number of descendants (for directories).
    #[serde(default)]
    pub max_descendants: Option<u32>,
    /// Minimum tree size in bytes (recursive size of directory subtree).
    #[serde(default)]
    pub min_treesize: Option<u64>,
    /// Maximum tree size in bytes.
    #[serde(default)]
    pub max_treesize: Option<u64>,
    /// Minimum tree allocated size in bytes.
    #[serde(default)]
    pub min_tree_allocated: Option<u64>,
    /// Maximum tree allocated size in bytes.
    #[serde(default)]
    pub max_tree_allocated: Option<u64>,

    // ── Derived metric filters ────────────────────────────────────
    /// Minimum bulkiness percentage (e.g. `200` = 200% allocated/logical).
    #[serde(default)]
    pub min_bulkiness: Option<u64>,
    /// Maximum bulkiness percentage.
    #[serde(default)]
    pub max_bulkiness: Option<u64>,
    /// Minimum file name length (characters).
    #[serde(default)]
    pub min_name_length: Option<u32>,
    /// Maximum file name length (characters).
    #[serde(default)]
    pub max_name_length: Option<u32>,
    /// Minimum full path length (characters).
    #[serde(default)]
    pub min_path_length: Option<u32>,
    /// Maximum full path length (characters).
    #[serde(default)]
    pub max_path_length: Option<u32>,

    // ── Projection ────────────────────────────────────────────────
    /// Columns for the human-readable text table.
    /// Choose from: `name`, `ext`, `type`, `size`, `modified`, `path`, `drive`,
    /// `allocated`, `created`, `accessed`, `flags`, `descendants`, `treesize`,
    /// `tree_allocated`.
    /// Default: `name,ext,type,size,modified,path`.
    /// `structuredContent` always includes all fields regardless.
    #[serde(default)]
    pub projection: Vec<String>,
}

/// Default sort field.
fn default_sort() -> String {
    "modified".to_owned()
}

/// Default result limit.
const fn default_limit() -> u32 {
    50
}

/// Default filter.
fn default_filter() -> String {
    "all".to_owned()
}

/// Derive the file extension from a filename (lowercase, no leading dot).
///
/// Dot-gated to match the MFT indexer's `intern_extension` and the search
/// engine's `extract_extension_after_dot`: dotless names (`README`),
/// hidden dotfiles (`.bash_history`, `.gitignore`), and trailing-dot
/// names (`foo.`) all return the empty string.  Keeping the displayed
/// `ext` aligned with the sort key prevents `--sort extension` from
/// emitting rows whose `ext` field disagrees with their position
/// (regression: T62 on Windows where `.bash_history` placed at row 1
/// reported `ext = "bash_history"` and broke the ascending invariant).
fn extract_ext(name: &str) -> String {
    let Some(dot_pos) = name.rfind('.') else {
        return String::new();
    };
    if dot_pos == 0 || dot_pos + 1 >= name.len() {
        return String::new();
    }
    name.get(dot_pos + 1..)
        .map_or_else(String::new, str::to_ascii_lowercase)
}

/// Decode a cursor string into an offset.  Returns 0 for invalid cursors.
fn decode_cursor(cursor: &str) -> u32 {
    cursor
        .strip_prefix("offset:")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(0)
}

/// Encode an offset into an opaque cursor string.
fn encode_cursor(offset: u32) -> String {
    format!("offset:{offset}")
}

/// Execute the search tool.
///
/// # Errors
///
/// Returns [`BridgeError`] if the daemon search call fails.
#[expect(
    clippy::too_many_lines,
    reason = "param mapping from MCP to SearchParams — mirrors CLI arg processing"
)]
pub(crate) async fn run(
    client: &mut UffsClient,
    args: SearchArgs,
    roots_state: &RootsState,
) -> Result<CallToolResult, BridgeError> {
    let mut warnings: Vec<String> = Vec::new();

    // Resolve projection (default: name,ext,type,size,modified,path).
    let default_cols = ["name", "ext", "type", "size", "modified", "path"];
    let projection: Vec<String> = if args.projection.is_empty() {
        default_cols.iter().map(|col| (*col).to_owned()).collect()
    } else {
        args.projection.clone()
    };

    // Clamp limit to the hard cap.
    let requested_limit = args.limit;
    let effective_limit = requested_limit.min(HARD_CAP);
    if requested_limit > HARD_CAP {
        warnings.push(format!(
            "Requested limit {requested_limit} exceeds the MCP hard cap of {HARD_CAP}; \
             clamped to {HARD_CAP}."
        ));
    }

    // Resolve cursor offset.
    let offset = args.cursor.as_deref().map_or(0, decode_cursor);

    // Convert explicit drive strings to typed `DriveLetter` values.
    // Malformed entries (non-ASCII, multi-char) are silently dropped
    // — the prior `char` path took only the first character and
    // forwarded it as-is, so this is a stricter version of the same
    // observable behaviour.
    let explicit_drives: Vec<uffs_mft::platform::DriveLetter> = args
        .drives
        .iter()
        .filter_map(|drv| drv.chars().next())
        .filter_map(|ch| uffs_mft::platform::DriveLetter::parse(ch).ok())
        .collect();

    // Ask the daemon for offset + limit rows so we can skip the first `offset`.
    let daemon_limit = offset.saturating_add(effective_limit);
    let mut search_params = SearchParams {
        pattern: args.pattern,
        case_sensitive: args.case_sensitive,
        whole_word: args.whole_word,
        match_path: args.match_path,
        sort: Some(args.sort),
        sort_desc: args.sort_desc,
        limit: Some(daemon_limit),
        filter: Some(args.filter),
        drives: explicit_drives,
        // Extension / exclude / path.
        ext: args.ext,
        exclude: args.exclude,
        path_contains: args.path_contains,
        path_excludes: args.path_excludes,
        hide_system: args.hide_system,
        // Size bounds.
        min_size: args.min_size,
        max_size: args.max_size,
        min_allocated: args.min_allocated,
        max_allocated: args.max_allocated,
        // Time bounds.
        newer: args.newer,
        older: args.older,
        newer_created: args.newer_created,
        older_created: args.older_created,
        newer_accessed: args.newer_accessed,
        older_accessed: args.older_accessed,
        // Attributes.
        attr: args.attr,
        // Type category.
        type_filter: args.type_filter,
        // Directory metrics.
        min_descendants: args.min_descendants,
        max_descendants: args.max_descendants,
        min_treesize: args.min_treesize,
        max_treesize: args.max_treesize,
        min_tree_allocated: args.min_tree_allocated,
        max_tree_allocated: args.max_tree_allocated,
        // Derived metrics.
        min_bulkiness: args.min_bulkiness,
        max_bulkiness: args.max_bulkiness,
        min_name_len: args
            .min_name_length
            .map(|len| u16::try_from(len).unwrap_or(u16::MAX)),
        max_name_len: args
            .max_name_length
            .map(|len| u16::try_from(len).unwrap_or(u16::MAX)),
        min_path_len: args
            .min_path_length
            .map(|len| u16::try_from(len).unwrap_or(u16::MAX)),
        max_path_len: args
            .max_path_length
            .map(|len| u16::try_from(len).unwrap_or(u16::MAX)),
        ..Default::default()
    };
    search_params.populate_canonical_fields();

    // Apply roots-based scoping (drive + path prefix) when no explicit drives.
    roots::apply_roots_scope(roots_state, &mut search_params);

    // Cold-index contract: return a retryable "warming" error instead of
    // blocking silently for the length of a re-warm (see `tools::warm`).
    // Full params, not just the drive scope: the daemon's plan consults
    // the query's resolved ext filter against each Parked bloom.
    super::warm::warm_gate(client, &search_params).await?;

    tracing::debug!(
        params_json = %serde_json::to_string(&search_params).unwrap_or_default(),
        "uffs_search: sending search request to daemon"
    );

    let t_daemon = std::time::Instant::now();
    let response = client
        .search(&search_params)
        .await
        .map_err(|err| BridgeError::Daemon(format!("Search failed: {err}")))?;

    tracing::info!(
        rows = response.payload.row_count_hint().unwrap_or(0),
        records_scanned = response.records_scanned,
        daemon_ms = response.duration_ms,
        ipc_ms = u64::try_from(t_daemon.elapsed().as_millis()).unwrap_or(u64::MAX),
        truncated = response.truncated,
        "uffs_search: daemon response received"
    );

    let total_count = response.total_count;
    // MCP search always requests a structured `SearchRow` projection;
    // the async client's `search()` transparently resolves any
    // `ShmemRows` payload to `InlineRows`, and blob variants never
    // fire for MCP callers (they only activate for path-only or
    // multi-column CSV stdout on the CLI path).  If something
    // upstream ever breaks these invariants we fail fast with an
    // explicit Daemon error rather than silently hiding rows.
    let rows = response.payload.into_inline_rows().ok_or_else(|| {
        BridgeError::Daemon(
            "unexpected non-rows payload from daemon search — \
             MCP always requests structured rows"
                .into(),
        )
    })?;

    // Slice to the current page (skip `offset` rows).
    let page_rows: Vec<_> = rows
        .into_iter()
        .skip(offset as usize) // u32→usize lossless on 64-bit
        .take(effective_limit as usize) // u32→usize lossless on 64-bit
        .collect();
    // page_rows.len() is bounded by HARD_CAP (100) which fits in u32.
    let page_len = u32::try_from(page_rows.len()).unwrap_or(u32::MAX);
    let end_offset = offset.saturating_add(page_len);
    let has_more = u64::from(end_offset) < total_count;

    let next_cursor = has_more.then(|| encode_cursor(end_offset));

    let output = format_text_output(&FormatContext {
        page_rows: &page_rows,
        projection: &projection,
        warnings: &warnings,
        offset,
        end_offset,
        total_count,
        records_scanned: response.records_scanned,
        duration_ms: response.duration_ms,
        promotion_ms: response.promotion_ms.unwrap_or(0),
        has_more,
        effective_limit,
        next_cursor: next_cursor.clone(),
    });

    // ── Build content: text table + capped resource links ──────────
    let mut content = vec![ContentBlock::text(output)];

    // Emit resource links only for the first MAX_RESOURCE_LINKS rows.
    // These let host UIs offer clickable detail views but the LLM never
    // sees them, so emitting hundreds is pure payload waste.
    for row in page_rows.iter().take(MAX_RESOURCE_LINKS) {
        let info_uri = format!("uffs://info/{}", percent_encode_path(&row.path));
        let resource = Resource::new(info_uri, &row.name)
            .with_description(format!("Full metadata for {}", row.path))
            .with_mime_type("application/json");
        content.push(ContentBlock::resource_link(resource));
    }

    let structured = SearchOutput {
        returned: page_rows.len(),
        total_count,
        records_scanned: response.records_scanned,
        duration_ms: response.duration_ms,
        promotion_ms: response.promotion_ms.unwrap_or(0),
        truncated: has_more,
        next_cursor,
        warnings,
        rows: page_rows
            .iter()
            .map(|row| {
                let ext = extract_ext(&row.name);
                let r#type = if row.is_directory { "dir" } else { "file" }.to_owned();
                SearchRowOutput {
                    drive: row.drive,
                    name: row.name.clone(),
                    ext,
                    r#type,
                    size: row.size,
                    allocated: row.allocated,
                    modified: row.modified,
                    created: row.created,
                    accessed: row.accessed,
                    flags: row.flags,
                    is_directory: row.is_directory,
                    descendants: row.descendants,
                    treesize: row.treesize,
                    tree_allocated: row.tree_allocated,
                    path: row.path.clone(),
                }
            })
            .collect(),
    };

    let mut result = CallToolResult::success(content);
    result.structured_content = Some(serde_json::to_value(structured)?);
    Ok(result)
}

/// Bundled context for [`format_text_output`] to avoid too many parameters.
struct FormatContext<'a> {
    /// The page of search result rows.
    page_rows: &'a [uffs_client::protocol::response::SearchRow],
    /// Columns to display.
    projection: &'a [String],
    /// Optional warnings to show before the table.
    warnings: &'a [String],
    /// 0-based offset of the first row in the full result set.
    offset: u32,
    /// 0-based offset just past the last row (exclusive).
    end_offset: u32,
    /// Total number of matching rows.
    total_count: u64,
    /// Number of MFT records scanned.
    records_scanned: usize,
    /// Wall-clock duration of the SCAN in milliseconds — excludes any
    /// tier promotion, which is reported separately.
    duration_ms: u64,
    /// Milliseconds spent paging parked/cold drives back in before the
    /// scan.  Surfaced whenever non-zero: without it a query that spent
    /// 21 s warming and 1 ms scanning reports "1ms", and the cost of
    /// the slow case is invisible precisely when someone is wondering
    /// where the time went.
    promotion_ms: u64,
    /// Whether there are more results beyond this page.
    has_more: bool,
    /// The effective per-page limit (after clamping).
    effective_limit: u32,
    /// Opaque cursor for the next page, if any.
    next_cursor: Option<String>,
}

/// Render the promotion cost, or nothing when there was none.
///
/// Silent in the steady state (a warm index promotes nothing), loud
/// when a query just paid tens of seconds to page the index in.
fn warm_note(promotion_ms: u64) -> String {
    if promotion_ms == 0 {
        String::new()
    } else {
        format!(" + {promotion_ms}ms index warm-up")
    }
}

/// Render the human-readable text summary for the search results.
fn format_text_output(ctx: &FormatContext<'_>) -> String {
    let mut output = String::new();
    if ctx.page_rows.is_empty() {
        _ = write!(
            output,
            "0 matches ({} scanned in {}ms{})\n\n",
            ctx.records_scanned,
            ctx.duration_ms,
            warm_note(ctx.promotion_ms),
        );
    } else {
        _ = write!(
            output,
            "Showing {}-{} of {} matches ({} scanned in {}ms{})\n\n",
            ctx.offset + 1,
            ctx.end_offset,
            ctx.total_count,
            ctx.records_scanned,
            ctx.duration_ms,
            warm_note(ctx.promotion_ms),
        );
    }
    for warning in ctx.warnings {
        _ = writeln!(output, "⚠ {warning}");
    }

    if ctx.page_rows.is_empty() {
        output.push_str("No matches found.\n");
    } else {
        // Build projection-aware markdown table.
        let header: Vec<&str> = ctx.projection.iter().map(|col| col_header(col)).collect();
        let separator: Vec<&str> = header.iter().map(|_| "------").collect();
        _ = writeln!(output, "| {} |", header.join(" | "));
        _ = writeln!(output, "| {} |", separator.join(" | "));
        for row in ctx.page_rows {
            let cells: Vec<String> = ctx
                .projection
                .iter()
                .map(|col| col_value(col, row))
                .collect();
            _ = writeln!(output, "| {} |", cells.join(" | "));
        }
        if ctx.has_more {
            // When total matches vastly exceed the page limit, the agent
            // should probably be using aggregate/facet tools rather than
            // paging through hundreds of rows.
            let limit_u64 = u64::from(ctx.effective_limit);
            if ctx.total_count > limit_u64.saturating_mul(AGGREGATE_HINT_FACTOR) {
                _ = write!(
                    output,
                    "\n⚠ {} total matches — far more than the {limit_u64}-row page. \
                     Consider using `uffs_aggregate` (e.g. preset `duplicates`, \
                     `by_extension`, `by_type`) to summarise large result sets \
                     instead of paging through them.\n",
                    ctx.total_count,
                );
            } else if let Some(cursor) = &ctx.next_cursor {
                _ = write!(
                    output,
                    "\n(More results available. Pass `cursor: \"{cursor}\"` to fetch the next page.)\n",
                );
            }
        }
    }
    output
}

/// Map a projection column name to a display header.
fn col_header(col: &str) -> &'static str {
    match col {
        "name" => "Name",
        "ext" => "Ext",
        "type" => "Type",
        "size" => "Size",
        "allocated" => "Allocated",
        "modified" => "Modified",
        "created" => "Created",
        "accessed" => "Accessed",
        "path" => "Path",
        "drive" => "Drive",
        "flags" => "Flags",
        "descendants" => "Descendants",
        "treesize" => "Tree Size",
        "tree_allocated" => "Tree Allocated",
        _ => "?",
    }
}

/// Extract a column value from a search row for the human-readable table.
fn col_value(col: &str, row: &uffs_client::protocol::response::SearchRow) -> String {
    match col {
        "name" => row.name.clone(),
        "ext" => extract_ext(&row.name),
        "type" => {
            if row.is_directory {
                "dir".to_owned()
            } else {
                "file".to_owned()
            }
        }
        "size" => uffs_client::protocol::response::format_size(row.size),
        "allocated" => uffs_client::protocol::response::format_size(row.allocated),
        "modified" => uffs_client::protocol::response::format_time(row.modified),
        "created" => uffs_client::protocol::response::format_time(row.created),
        "accessed" => uffs_client::protocol::response::format_time(row.accessed),
        "path" => row.path.clone(),
        "drive" => row.drive.to_string(),
        "flags" => format!("0x{:08X}", row.flags),
        "descendants" => row.descendants.to_string(),
        "treesize" => uffs_client::protocol::response::format_size(row.treesize),
        "tree_allocated" => uffs_client::protocol::response::format_size(row.tree_allocated),
        _ => String::new(),
    }
}

/// Percent-encode a Windows path for use in a `uffs://info/{path}` URI.
///
/// Encodes backslashes, spaces, and other URI-unsafe characters while keeping
/// drive letters and forward slashes readable.
#[must_use]
pub(crate) fn percent_encode_path(path: &str) -> String {
    // Normalize backslashes to forward slashes for URI compatibility.
    let normalized = path.replace('\\', "/");
    // Percent-encode characters that aren't URI-safe path characters.
    // Keep: alphanumeric, `-`, `_`, `.`, `/`, `:`
    let mut encoded = String::with_capacity(normalized.len());
    for ch in normalized.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '/' | ':' => {
                encoded.push(ch);
            }
            ' ' => encoded.push_str("%20"),
            _ => {
                // Percent-encode multi-byte UTF-8 chars byte-by-byte.
                let mut buf = [0_u8; 4];
                for byte in ch.encode_utf8(&mut buf).bytes() {
                    _ = write!(encoded, "%{byte:02X}");
                }
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::extract_ext;

    /// Regression: dot-gated extraction so the MCP-emitted `ext` field
    /// (in both the markdown table and `structuredContent.rows[*].ext`)
    /// matches the daemon's sort key
    /// (`uffs_core::search::sorting::build_row_sort_key` →
    /// `extract_extension_after_dot`).  Pre-fix, naive `rsplit_once('.')`
    /// produced `ext = "bash_history"` for `.bash_history`, but the sort
    /// engine placed the row in the empty-extension bucket — the
    /// resulting disagreement broke Windows MCP T62 `--sort extension`
    /// (`scripts/tests/definitions/03-sort.toml`) because the validator
    /// reads `ext` and finds it non-monotonic.
    #[test]
    fn extract_ext_returns_empty_for_dotfiles() {
        assert_eq!(extract_ext(".bash_history"), "");
        assert_eq!(extract_ext(".gitignore"), "");
        assert_eq!(extract_ext(".env"), "");
    }

    #[test]
    fn extract_ext_returns_empty_for_dotless_names() {
        assert_eq!(extract_ext("README"), "");
        assert_eq!(extract_ext("Makefile"), "");
        assert_eq!(extract_ext(""), "");
    }

    #[test]
    fn extract_ext_returns_empty_for_trailing_dot() {
        assert_eq!(extract_ext("foo."), "");
        assert_eq!(extract_ext("archive.tar."), "");
    }

    #[test]
    fn extract_ext_returns_lowercase_segment_after_last_dot() {
        assert_eq!(extract_ext("report.txt"), "txt");
        assert_eq!(extract_ext("archive.tar.gz"), "gz");
        assert_eq!(extract_ext("$RECYCLE.BIN"), "bin");
        assert_eq!(
            extract_ext(
                "amd64_microsoft-windows-mdmappinstaller_31bf3856ad364e35_10.0.26100.8115_none_3591783d4bfd6e96"
            ),
            "8115_none_3591783d4bfd6e96"
        );
    }
}
