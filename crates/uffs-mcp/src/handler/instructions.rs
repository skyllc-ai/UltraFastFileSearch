// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Agent instructions returned in the MCP `initialize` response.
//!
//! Designed to be read by LLM agents, not humans.  Teaches the agent
//! how to use UFFS tools effectively in the fewest possible queries.

// ── Agent instructions ────────────────────────────────────────────────
//
// This is the first thing every MCP agent sees.  It must be:
// • Concise enough to fit in a context window without crowding.
// • Comprehensive enough that an agent can answer filesystem questions
//   without reading any other docs.
// • Actionable — every sentence should help the agent decide which tool
//   to call and with which parameters.

/// Server instructions returned in the `initialize` response.
///
/// Designed to be read by LLM agents, not humans.  Teaches the agent
/// how to use UFFS tools effectively in the fewest possible queries.
pub(crate) const AGENT_INSTRUCTIONS: &str = "\
UFFS — Ultra Fast File Search.  Indexes NTFS drives via the Master File \
Table and serves sub-millisecond queries over millions of files.

TOOLS (all read-only):
• uffs_search   — Search files/dirs.  Supports glob (*.pdf), regex (>pattern), \
  substring (invoice), and match-all (*).  40+ filter parameters: size, date, \
  extension, type, path, NTFS attributes, bulkiness, treesize, descendants.
• uffs_aggregate — Server-side analytics.  Use presets for one-call answers: \
  overview, by_type, by_extension, by_drive, by_size, by_age, storage, \
  activity, top_folders, duplicates, media, cleanup.
• uffs_facet_values — Discover distinct values of a field (extension, type, \
  drive) with counts and byte totals.  Use BEFORE searching to understand \
  what exists.
• uffs_info     — Full metadata for a single file/directory by path.
• uffs_drives   — List indexed drives with record counts.
• uffs_status   — Daemon health, uptime, memory, loading progress, and the \
  running UFFS server version (see STAYING CURRENT).

QUERY STRATEGY (minimize round-trips):
1. Start with uffs_aggregate preset='overview' to get the lay of the land.
2. Use uffs_facet_values to discover what extensions/types/drives exist.
3. Use uffs_search with specific filters to drill down.
4. Combine multiple filters in ONE call — they are ANDed.  More filters = \
   fewer results = faster.

KEY PARAMETERS for uffs_search:
• pattern: '*' (match-all), '*.ext' (glob), 'word' (substring), '>regex'
  KEYWORD-OR — for a topic that could be any of several words, use ONE \
  regex, NEVER N separate searches: '>(solar|energy|utility|pge|sunrun)'. \
  Regex is CASE-INSENSITIVE by default.
• match_path: true → match `pattern` against the full path, not just the name
• filter: 'files', 'dirs', or 'all'
• ext: 'pdf' or collection aliases: pictures, documents, videos, music, \
  archives, code  (documents = pdf, doc/docx, xls/xlsx, ppt/pptx, csv, txt, …)
• type_filter: semantic category: picture, document, archive, code, video, \
  audio, executable, database, config, log, system
• min_size / max_size: bytes (1073741824 = 1 GB)
• newer / older: '7d', '24h', '2w', '2026-01-15', 'today', 'last_30d'
• newer_created / older_created / newer_accessed / older_accessed
• path_contains: scope to a subtree ('Users\\\\name' or 'Users/name')
• path_excludes: drop noise DIRS — comma-separated dir globs matched against \
  the path, record dropped if it matches ANY: \
  '*appdata*,*.cargo*,*.rustup*,*node_modules*,*downloads*'
• exclude: drop by FILENAME glob (not path) — e.g. '~$*' for Office temp files
• drives: ['C'] or ['C','D'] to scope to specific drives
• sort: 'modified', '-size', 'name', '-treesize', '-descendants', '-bulkiness'
• limit: max results (default 50, cap 100)
• projection: columns to return — name, ext, type, size, modified, path, drive, \
  created, accessed, allocated, treesize, descendants, tree_allocated
• whole_word: true → the WHOLE file name must match the pattern \
  ('Cargo.toml' finds only names that are exactly that, any case)
• attr: NTFS attributes — 'hidden', 'system', 'compressed', 'encrypted', etc.
• min_descendants / max_descendants: filter dirs by child count

KEY PARAMETERS for uffs_aggregate:
• preset: one-word shortcut — overview, by_type, by_extension, by_drive, \
  by_size, by_age, storage, activity, top_folders, duplicates, media, cleanup
• aggregations: array of custom power-syntax specs for full control. \
  10 kinds: count, stats:FIELD, terms:FIELD, hist:FIELD, datehist:FIELD, \
  range:FIELD, missing:FIELD, distinct:FIELD, rollup:path, duplicates:KEY+KEY. \
  Options: top=N, sample=N, interval=N, calendar=day|week|month|quarter|year, \
  depth=N, bins=A..B+C..D, sub=kind:field.
• pattern / drives: scope aggregation to a subset (same as search).
• page_size: enable paginated buckets. Response includes next_cursor.
• cursor: opaque token from previous response to fetch the next page.
POWER MOVE: stack multiple specs in ONE call — \
  aggregations=['count','stats:size','terms:type,top=10'] runs all in one pass.
AGGREGATABLE fields (stats/hist/range): size, allocated, modified, created, \
  accessed, descendants, treesize, tree_allocated, bulkiness, name_length, \
  path_length.
GROUPABLE fields (terms/rollup): extension, type, drive, name, directory, \
  hidden, system, compressed, encrypted, read_only, archive, sparse, reparse, \
  temporary, offline.

KEY PARAMETERS for uffs_facet_values:
• field: 'extension', 'type', or 'drive'
• top: number of values to return (default 20)
• pattern: scope to files matching a search pattern
• prefix: filter values by prefix (e.g. 'doc' → doc, docx, docm)
• page_size / cursor: pagination (same as aggregate)

RESOURCES (readable even if tools are unavailable):
• uffs://cookbook — ESSENTIAL: ~30 curated example queries with ready-to-use \
  arguments objects organized by workflow.  Read this FIRST to learn patterns.
• uffs://schema/search — JSON Schema for uffs_search parameters.
• uffs://schema/fields — Complete field catalog with types and capabilities.
• uffs://presets/aggregate — List of aggregate presets with descriptions.
• uffs://drives — Live drive listing.
• uffs://status — Daemon health.
• uffs://info/{path} — Dynamic resource template for file metadata \
  (percent-encode path: C:\\Windows → C%3A%5CWindows).

COMMON USER REQUESTS (natural language -> tool call):
• Find my file        -> uffs_search pattern='filename'
• Where is folder X?  -> uffs_search pattern='X' filter='dirs'
• What eats space?    -> uffs_aggregate preset='overview', then sort='-size'
• Clean up disk       -> uffs_aggregate preset='cleanup', then old+large search
• Recent files        -> uffs_search newer='7d' sort='-modified'
• Recently opened     -> uffs_search newer_accessed='7d' sort='-accessed'
• Find duplicates     -> uffs_aggregate preset='duplicates'
• Empty folders       -> uffs_search filter='dirs' max_descendants=0
• Long path problems  -> uffs_search min_path_length=260 sort='-path_length'
• Hidden files        -> uffs_search attr='hidden' sort='-size'
• Find config files   -> uffs_search ext='json,toml,yaml,yml,ini,env,cfg'
• Big photos/videos   -> uffs_search ext='pictures,videos' min_size=52428800 sort='-size'
• Recent executables  -> uffs_search type_filter='executable' newer='7d'
• Old large files     -> uffs_search min_size=104857600 older='365d' sort='-size'
• Inventory a drive   -> uffs_aggregate preset='overview' drives=['X']
• Topic files in a folder, minus dev noise (ONE call, not many):
    My solar/energy spreadsheets under my home dir ->
    uffs_search pattern='>(solar|energy|utility|electric|pge|sunrun|sunpower|tesla)' \
      ext='xls,xlsx' path_contains='Users\\\\name' \
      path_excludes='*appdata*,*.cargo*,*.rustup*,*downloads*' sort='-modified'
NOTE: UFFS does NOT search inside file contents — it searches file names, \
paths, and metadata (the keyword regex matches the FILENAME — e.g. an invoice \
named after the installer/utility).  For content search, suggest ripgrep.

PROMPTS (guided multi-step workflows):
find_large_files, find_by_extension, disk_usage_report, cleanup_report, \
recent_changes, duplicate_investigation, subtree_analysis.

STAYING CURRENT:
uffs_status reports `server_version` — the UFFS build serving you. UFFS ships \
frequent improvements and can self-update. Treat `uffs --update` as a quick, \
safe, one-shot refresh the USER runs themselves (it stops/swaps binaries) — so \
if it's worth staying current, SUGGEST it to the user; never run it yourself \
(these tools are read-only). Note: an update refreshes the on-disk binaries, \
but YOUR session keeps its current `server_version` until its host relaunches \
the MCP server — so a version bump shows up on your next launch, not mid-session.
";

#[cfg(test)]
mod tests {
    use super::AGENT_INSTRUCTIONS;
    use crate::tools::search::HARD_CAP;

    /// The limit the instructions promise is the limit the search tool
    /// enforces.
    #[test]
    fn instructions_state_the_search_hard_cap() {
        assert!(
            AGENT_INSTRUCTIONS.contains(&format!("cap {HARD_CAP})")),
            "AGENT_INSTRUCTIONS must state the uffs_search hard cap of {HARD_CAP}"
        );
    }
}
