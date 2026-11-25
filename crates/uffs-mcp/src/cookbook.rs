// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Agent cookbook — curated MCP tool call examples organized by workflow.
//!
//! This is the backing data for the `uffs://cookbook` resource.  It is
//! intentionally a single file because the examples form a cohesive
//! narrative — splitting by arbitrary line count would fragment readability.
//!
//! Exception: `file_size_policy` — declarative JSON data, single-file cohesion.

use serde_json::{Value, json};

/// Build the `uffs://cookbook` JSON string.
///
/// A curated set of example MCP tool calls organized by workflow, designed
/// to teach agents how to compose effective UFFS queries.  Each entry
/// includes the tool name, a ready-to-use `arguments` object, and a
/// human-readable explanation of what the query does and why each
/// parameter was chosen.
///
/// The cookbook is structured so an agent can:
/// 1. Scan categories to find the right workflow.
/// 2. Copy the `arguments` object directly into a `tools/call`.
/// 3. Adapt the example by changing one or two parameters.
#[must_use]
pub(crate) fn cookbook_json() -> String {
    let cookbook = json!({
        "description": "UFFS MCP Cookbook — ready-to-use example tool calls for common \
            file-system questions. Each entry has a tool name, arguments object, and \
            explanation. Copy the arguments into a tools/call request and adapt as needed.",
        "tips": [
            // ── Search tips ─────────────────────────────────────────
            "pattern '*' is the match-all — use it when your intent is pure filtering \
             (by size, date, type, etc.) rather than name matching.",
            "Combine filters freely — they are ANDed. More filters = fewer results = faster.",
            "sort accepts a leading '-' for descending: '-size' = largest first.",
            "path_contains scopes results to a subtree. Use backslash separators: 'Users\\rnio'.",
            "ext accepts collection aliases: 'pictures', 'documents', 'archives', 'code', \
             'videos', 'music' — each expands to ~10 extensions automatically.",
            "type_filter is like ext but semantic: 'picture', 'document', 'archive', 'code', \
             'video', 'audio', 'executable', 'database', 'config', 'log', 'system'.",
            "projection controls which columns appear in results. Default is \
             name,ext,type,size,modified,path. Add 'drive' for multi-drive queries, \
             'created' for age analysis, 'treesize'/'descendants' for directory analysis.",
            "limit defaults to 50. Set higher (up to 500) when you need comprehensive \
             results, lower (5-10) for quick checks.",
            "whole_word=true makes the WHOLE file name match the pattern: 'test.txt' \
             matches only names that are exactly 'test.txt' (any case), not \
             'test.txt.bak' or 'my_test.txt'. Globs already match the whole name.",
            "drives: ['C'] scopes to a single drive. Omit for all drives.",
            // ── Aggregation tips ────────────────────────────────────
            "ALWAYS start with uffs_aggregate preset='overview' — one call gives you \
             total count, size stats, type breakdown, drive breakdown, and monthly histogram. \
             This is worth 5-6 separate uffs_search calls.",
            "Use uffs_facet_values to discover what extensions, types, or drives exist \
             before constructing a targeted search.",
            "Aggregation power syntax: 'kind:field,option=value'. 10 kinds available: \
             count, stats, terms, hist, datehist, range, missing, distinct, rollup, duplicates.",
            "Stack MULTIPLE aggregation specs in ONE call: \
             aggregations=['count','stats:size','terms:type,top=10']. \
             All execute in a single pass — much faster than separate calls.",
            "AGGREGATABLE fields (for stats/hist/range): size, allocated, modified, created, \
             accessed, descendants, treesize, tree_allocated, bulkiness, name_length, path_length.",
            "GROUPABLE fields (for terms/rollup): extension, type, drive, name, \
             and boolean flags: directory, hidden, system, compressed, encrypted, \
             read_only, archive, sparse, reparse, temporary, offline.",
            "terms with sample=N includes sample files per bucket — eliminates a follow-up \
             uffs_search call. Example: 'terms:extension,top=10,sample=3' shows 3 files per extension.",
            "rollup with sub=kind:field adds a sub-aggregation inside each bucket. \
             Example: 'rollup:path,depth=1,top=10,sub=terms:type' shows type breakdown per folder.",
            "Pagination: use page_size=N on uffs_aggregate or uffs_facet_values to get \
             paginated results. Response includes next_cursor — pass it back as cursor \
             to get the next page. Repeat until next_cursor is null.",
            "Every aggregation bucket includes a drilldown predicate — the exact filter \
             to re-query just that bucket's contents via uffs_search.",
            // ── High-value lesser-known filters ─────────────────────────
            "min_path_length=260 finds files exceeding Windows MAX_PATH — a top \
             cleanup and troubleshooting query for IT admins.",
            "max_descendants=0 with filter='dirs' finds empty folders — one of the \
             most requested cleanup queries across all search tools.",
            "newer_accessed / older_accessed use the NTFS last-access timestamp. \
             Great for 'what was actually used recently?' queries. Also: \
             newer_created / older_created for file birth time.",
            "min_name_length / max_name_length filter by filename length. Useful for \
             finding files with suspiciously long or very short names.",
            "UFFS does NOT search inside file contents — only names, paths, and \
             metadata. If a user asks for content search, suggest ripgrep (rg).",
            "The 'Real-World Workflows' category in this cookbook includes \
             'user_would_say' fields — match the user's natural language to these \
             patterns for instant query composition.",
            // ── Resource tips ───────────────────────────────────────
            "uffs://info/{path} is a dynamic resource template. Percent-encode the path: \
             C:\\Windows → C%3A%5CWindows. Returns full metadata without a tool call.",
            "If your host doesn't support tools/call, read uffs://cookbook — it has \
             ready-to-use JSON arguments objects you can copy directly."
        ],
        "categories": cookbook_categories()
    });
    serde_json::to_string_pretty(&cookbook).unwrap_or_else(|_| "{}".to_owned())
}

/// Build the categorized example entries.
fn cookbook_categories() -> Value {
    json!([
        cookbook_quick_find(),
        cookbook_size_triage(),
        cookbook_time_filters(),
        cookbook_type_and_ext(),
        cookbook_path_scoping(),
        cookbook_subtree_analysis(),
        cookbook_cleanup(),
        cookbook_aggregation(),
        cookbook_facets(),
        cookbook_advanced(),
        cookbook_real_world_workflows()
    ])
}

/// Quick-find examples — the #1 reason people use file search.
fn cookbook_quick_find() -> Value {
    json!({
        "category": "Quick Find",
        "description": "Known-item lookup: you know roughly what the file is called.",
        "examples": [
            {
                "title": "Find all PDFs",
                "tool": "uffs_search",
                "arguments": { "pattern": "*.pdf", "filter": "files", "limit": 50 },
                "explanation": "Glob *.pdf matches any file ending in .pdf. \
                    filter=files excludes directories."
            },
            {
                "title": "Substring search — find paths containing 'invoice'",
                "tool": "uffs_search",
                "arguments": { "pattern": "invoice", "limit": 50 },
                "explanation": "A pattern without wildcards is a substring match against \
                    the full path. Finds 'invoice.pdf', 'invoices/' directory, \
                    'Q1_invoice_draft.docx', etc."
            },
            {
                "title": "Exact-name search — 'Makefile' but not 'Makefile.bak'",
                "tool": "uffs_search",
                "arguments": { "pattern": "Makefile", "whole_word": true, "limit": 50 },
                "explanation": "whole_word=true makes the whole file name match: \
                    finds 'Makefile' (any case) but NOT 'Makefile.bak' or 'GNUmakefile'."
            },
            {
                "title": "Regex — find date-stamped CSV files",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": ">[0-9]{4}-[0-9]{2}-[0-9]{2}",
                    "ext": "csv",
                    "limit": 50
                },
                "explanation": "Prefix > activates regex mode. Combined with ext=csv \
                    to narrow to CSV files only. Finds '2026-04-10_report.csv'."
            },
            {
                "title": "Find a specific file on a specific drive",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*.pst",
                    "drives": ["D"],
                    "filter": "files",
                    "sort": "-size",
                    "limit": 5
                },
                "explanation": "Search for Outlook PST files on D: only, largest first. \
                    drives=['D'] scopes to one drive. sort='-size' = descending size."
            }
        ]
    })
}

/// Size triage — storage management and finding space hogs.
fn cookbook_size_triage() -> Value {
    json!({
        "category": "Size Triage",
        "description": "Find the biggest files and directories for storage cleanup.",
        "examples": [
            {
                "title": "Top 20 largest files across all drives",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "sort": "-size",
                    "limit": 20,
                    "projection": ["name", "size", "modified", "path", "drive"]
                },
                "explanation": "pattern='*' matches everything. filter=files skips dirs. \
                    sort='-size' puts largest first. projection adds 'drive' column for \
                    multi-drive visibility."
            },
            {
                "title": "Files over 1 GB",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "min_size": 1_073_741_824,
                    "sort": "-size",
                    "limit": 50
                },
                "explanation": "min_size is in bytes. 1073741824 = 1 GB. \
                    Combine with sort to rank the biggest offenders."
            },
            {
                "title": "Large archives (zip, 7z, rar, etc.) over 100 MB",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "ext": "zip,7z,rar,tar,gz,bz2,xz,zst,tgz,cab",
                    "min_size": 104_857_600,
                    "sort": "-size",
                    "limit": 30
                },
                "explanation": "ext lists specific extensions to match. \
                    Or use ext='archives' as a shortcut for common archive types."
            }
        ]
    })
}

/// Time-based filters — recent changes, old stale files.
fn cookbook_time_filters() -> Value {
    json!({
        "category": "Time Filters",
        "description": "Find files by modification, creation, or access time.",
        "examples": [
            {
                "title": "Files modified in the last 7 days",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "newer": "7d",
                    "sort": "modified",
                    "limit": 100
                },
                "explanation": "newer='7d' means modified within the last 7 days. \
                    Accepts: 90s, 30m, 24h, 7d, 2w, or ISO date '2026-01-15', \
                    or named ranges: 'today', 'yesterday', 'this_week', 'last_30d'."
            },
            {
                "title": "Old archives untouched for 2+ years",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "ext": "archives",
                    "older": "730d",
                    "sort": "-size",
                    "limit": 50
                },
                "explanation": "older='730d' = modified more than 730 days ago. \
                    ext='archives' is a collection alias expanding to zip,rar,7z,tar,gz,etc."
            },
            {
                "title": "Files created today",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "newer_created": "today",
                    "sort": "created",
                    "limit": 50,
                    "projection": ["name", "size", "created", "path"]
                },
                "explanation": "newer_created filters on NTFS creation timestamp. \
                    Use 'today' for convenience. Add 'created' to projection to see it."
            }
        ]
    })
}

/// Type and extension filtering.
fn cookbook_type_and_ext() -> Value {
    json!({
        "category": "Type & Extension Filtering",
        "description": "Search by file type category or extension collection.",
        "examples": [
            {
                "title": "All image files across all drives",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "ext": "pictures",
                    "sort": "-size",
                    "limit": 50
                },
                "explanation": "ext='pictures' expands to jpg,jpeg,png,gif,bmp,tiff,webp,svg,ico,raw,heic. \
                    Other aliases: 'documents', 'videos', 'music', 'archives', 'code'."
            },
            {
                "title": "Source code files modified this week",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "type_filter": "code",
                    "newer": "7d",
                    "sort": "modified",
                    "limit": 100
                },
                "explanation": "type_filter='code' matches rs,py,js,ts,java,c,cpp,h,go,rb,php,swift,kt. \
                    Difference from ext: type_filter uses semantic categories, ext uses raw extensions."
            },
            {
                "title": "Executables sorted by size",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "type_filter": "executable",
                    "sort": "-size",
                    "limit": 30
                },
                "explanation": "type_filter='executable' matches exe,msi,bat,cmd,ps1,com,scr."
            }
        ]
    })
}

/// Path scoping — narrowing to a specific directory tree.
fn cookbook_path_scoping() -> Value {
    json!({
        "category": "Path Scoping",
        "description": "Narrow searches to a specific directory subtree or drive.",
        "examples": [
            {
                "title": "Files inside a user's home directory",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "path_contains": "Users\\rnio",
                    "drives": ["C"],
                    "sort": "-size",
                    "limit": 30
                },
                "explanation": "path_contains='Users\\rnio' only returns files whose resolved \
                    path includes that substring. Use backslash as the separator. \
                    Combine with drives to further scope."
            },
            {
                "title": "Rust files in project directories",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*.rs",
                    "filter": "files",
                    "path_contains": "GitHub",
                    "sort": "modified",
                    "limit": 50
                },
                "explanation": "Combines a glob pattern (*.rs) with path_contains \
                    to restrict to paths containing 'GitHub'."
            },
            {
                "title": "Config files in project trees",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "ext": "toml,yaml,yml,json",
                    "path_contains": "projects",
                    "newer": "30d",
                    "limit": 50
                },
                "explanation": "Multi-extension filter + path scoping + time filter. \
                    Filters are ANDed — all must pass."
            }
        ]
    })
}

/// Subtree analysis — directory-level insights.
fn cookbook_subtree_analysis() -> Value {
    json!({
        "category": "Subtree & Directory Analysis",
        "description": "Analyze directory sizes, child counts, and subtree metrics.",
        "examples": [
            {
                "title": "Top 20 largest directory subtrees",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "dirs",
                    "sort": "-treesize",
                    "limit": 20,
                    "projection": ["name", "treesize", "descendants", "path", "drive"]
                },
                "explanation": "filter=dirs shows only directories. treesize is the sum \
                    of all file sizes in the subtree. Add descendants to see child count."
            },
            {
                "title": "Empty directories (cleanup candidates)",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "dirs",
                    "max_descendants": 0,
                    "limit": 100
                },
                "explanation": "max_descendants=0 finds directories with zero children."
            },
            {
                "title": "Directories with 1000+ children (crowded folders)",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "dirs",
                    "min_descendants": 1000,
                    "sort": "-descendants",
                    "limit": 20,
                    "projection": ["name", "descendants", "treesize", "path"]
                },
                "explanation": "Finds overly crowded directories. Useful for identifying \
                    flat folder structures that slow down file managers."
            }
        ]
    })
}

/// Cleanup workflows — finding waste and reclaimable space.
fn cookbook_cleanup() -> Value {
    json!({
        "category": "Cleanup & Waste Detection",
        "description": "Identify cleanup candidates: temp files, zero-byte files, \
            high-waste allocations, and stale content.",
        "examples": [
            {
                "title": "Old temp files wasting space",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*.tmp",
                    "filter": "files",
                    "older": "30d",
                    "sort": "-size",
                    "limit": 50
                },
                "explanation": "Temp files older than 30 days are usually safe to review \
                    for deletion."
            },
            {
                "title": "Zero-byte files (empty files)",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "max_size": 0,
                    "limit": 100
                },
                "explanation": "max_size=0 finds files with zero logical size."
            },
            {
                "title": "Files with wasteful disk allocation (high bulkiness)",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "min_size": 1_048_576,
                    "min_bulkiness": 500,
                    "sort": "-bulkiness",
                    "limit": 20,
                    "projection": ["name", "size", "allocated", "path"]
                },
                "explanation": "Bulkiness = allocated_size / logical_size × 100. \
                    500 means 5× more disk space than the file's logical size. \
                    min_size filters out tiny files where bulkiness is expected."
            },
            {
                "title": "Paths approaching MAX_PATH limit (260 chars)",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "min_path_length": 240,
                    "sort": "-path_length",
                    "limit": 20
                },
                "explanation": "Windows MAX_PATH is 260 chars. Files near this limit \
                    can cause issues with older tools."
            }
        ]
    })
}

/// Aggregation — get the big picture in a single call.
#[expect(clippy::too_many_lines, reason = "large static cookbook JSON blob")]
fn cookbook_aggregation() -> Value {
    json!({
        "category": "Aggregation (Big Picture)",
        "description": "Use uffs_aggregate to get filesystem-wide analytics in one call \
            instead of many searches.  Supports 12 built-in presets AND custom specs \
            for any combination of counts, stats, terms, histograms, rollups, and \
            duplicate detection.  Always start here before deep-diving with search.",
        "examples": [
            // ── Presets ─────────────────────────────────────────────
            {
                "title": "Full filesystem overview (THE first call)",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "overview" },
                "explanation": "Returns total count, files vs dirs, size stats (sum/min/max/avg), \
                    type facet, drive facet, and monthly modified histogram — all in one call. \
                    This is the best first call to understand a filesystem."
            },
            {
                "title": "Breakdown by file type",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "by_type" },
                "explanation": "Shows each semantic type (document, picture, video, code, \
                    archive, executable, …) with count, total size, waste, and share%."
            },
            {
                "title": "Top extensions by size",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "by_extension" },
                "explanation": "Top 50 extensions ranked by total size with share%. \
                    Shows exactly where disk space goes."
            },
            {
                "title": "Size distribution — how big are files?",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "by_size" },
                "explanation": "Size-bucket histogram: Empty, Tiny (<1 KB), Small (<1 MB), \
                    Medium (<100 MB), Large (<1 GB), Huge (<10 GB), Massive (10 GB+)."
            },
            {
                "title": "Age distribution — how stale is the data?",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "by_age" },
                "explanation": "Age buckets: today, this_week, this_month, this_quarter, \
                    this_year, last_year, older, ancient (>5 yr)."
            },
            {
                "title": "Storage analysis — logical vs allocated, waste",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "storage" },
                "explanation": "Logical vs allocated size per drive, waste per extension. \
                    Shows slack space and over-allocation."
            },
            {
                "title": "Cleanup candidates report",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "cleanup" },
                "explanation": "Identifies zero-byte files, temp files, no-extension files, \
                    and cache directories. Best way to find reclaimable space."
            },
            {
                "title": "Duplicate detection",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "duplicates" },
                "explanation": "Groups files by name+size to find potential duplicates. \
                    Shows reclaimable bytes per group."
            },
            {
                "title": "Scoped preset — overview of D: drive only",
                "tool": "uffs_aggregate",
                "arguments": { "preset": "overview", "drives": ["D"] },
                "explanation": "All presets accept 'drives' and 'pattern' to scope. \
                    Combine: preset='by_type', drives=['C'], pattern='*.rs'."
            },
            // ── Custom specs: all 10 kinds ──────────────────────────
            {
                "title": "Custom: terms — top 30 extensions",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["terms:extension,top=30"]
                },
                "explanation": "POWER SYNTAX: 'kind:field,option=value'. \
                    terms groups by a field and shows count + size per bucket. \
                    Options: top=N (default 50), sample=N (inline sample rows)."
            },
            {
                "title": "Custom: stats — size statistics",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["stats:size"]
                },
                "explanation": "Returns count, sum, min, max, avg for a numeric field. \
                    Works with: size, allocated, modified, created, accessed, \
                    descendants, treesize, tree_allocated, bulkiness, name_length, path_length."
            },
            {
                "title": "Custom: histogram — size in 100 MB buckets",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["hist:size,interval=104857600"]
                },
                "explanation": "Fixed-width numeric buckets. interval is in bytes \
                    (104857600 = 100 MB). Each bucket shows count and total size."
            },
            {
                "title": "Custom: date histogram — monthly creation timeline",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["datehist:created,calendar=month"]
                },
                "explanation": "Calendar-aligned time buckets. calendar options: \
                    day, week, month, quarter, year."
            },
            {
                "title": "Custom: range — size brackets",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["range:size,bins=0..1048576+1048576..1073741824+1073741824.."]
                },
                "explanation": "Custom numeric ranges. Format: bins=A..B+C..D+E.. \
                    (open-ended with trailing ..). This creates: <1 MB, 1 MB–1 GB, >1 GB."
            },
            {
                "title": "Custom: missing — files with no extension",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["missing:extension"]
                },
                "explanation": "Counts records where the field has no value. \
                    Useful for finding extensionless files."
            },
            {
                "title": "Custom: distinct — how many unique extensions?",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["distinct:extension"]
                },
                "explanation": "Returns the count of unique values for a field. \
                    Quick cardinality check."
            },
            {
                "title": "Custom: rollup — top 10 folders at depth 2",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["rollup:path,depth=2,top=10"]
                },
                "explanation": "Directory tree rollup at a given depth. depth=1 gives \
                    top-level folders, depth=2 gives second-level. Each bucket shows \
                    count and total size."
            },
            {
                "title": "Custom: rollup with sub-aggregation",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["rollup:path,depth=1,top=10,sub=terms:type"]
                },
                "explanation": "Sub-aggregation inside each rollup bucket. Shows the \
                    type breakdown WITHIN each top-level folder. Eliminates a follow-up query."
            },
            {
                "title": "Custom: terms with inline samples",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["terms:extension,top=10,sample=3"]
                },
                "explanation": "sample=N includes N representative files per bucket. \
                    Each sample shows name, size, and modified date. Lets you see \
                    what's in each group without a follow-up uffs_search call."
            },
            // ── Multi-spec and scoped ───────────────────────────────
            {
                "title": "Multiple specs in one call",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": [
                        "count",
                        "stats:size",
                        "terms:type,top=10",
                        "terms:extension,top=20"
                    ]
                },
                "explanation": "Stack multiple specs in one call — ALL execute in a single \
                    pass over the data. This is 4 answers in 1 round-trip. \
                    Much faster than 4 separate calls."
            },
            {
                "title": "Scoped custom spec — Rust code stats in GitHub folder",
                "tool": "uffs_aggregate",
                "arguments": {
                    "pattern": "*.rs",
                    "aggregations": [
                        "count",
                        "stats:size",
                        "datehist:modified,calendar=month"
                    ]
                },
                "explanation": "pattern scopes to *.rs files. You can also add drives, \
                    path_contains, newer, older — any search filter works as a scope."
            },
            // ── Pagination ──────────────────────────────────────────
            {
                "title": "Paginated aggregation — first page",
                "tool": "uffs_aggregate",
                "arguments": {
                    "aggregations": ["terms:extension,top=500"],
                    "page_size": 50
                },
                "explanation": "page_size=50 returns the first 50 buckets. The response \
                    includes 'next_cursor' — pass it back to get the next page. \
                    Use this when there are more groups than fit in one response."
            },
            {
                "title": "Paginated aggregation — next page",
                "tool": "uffs_aggregate",
                "arguments": {
                    "cursor": "eyJza..."
                },
                "explanation": "Pass the 'next_cursor' from the previous response as \
                    'cursor'. Repeat until next_cursor is null (last page)."
            }
        ]
    })
}

/// Facet exploration — discover what's in the index before searching.
fn cookbook_facets() -> Value {
    json!({
        "category": "Facet Exploration",
        "description": "Use uffs_facet_values to discover what exists in the index \
            before constructing a targeted search.  Returns top-N values with counts \
            and byte totals.  Supports pagination for walking all values. \
            Available fields: extension, type, drive.",
        "examples": [
            {
                "title": "Top 20 file extensions by count",
                "tool": "uffs_facet_values",
                "arguments": { "field": "extension", "top": 20 },
                "explanation": "Shows the 20 most common extensions with file counts \
                    and total bytes. Use this to understand what's on the drives."
            },
            {
                "title": "What drives are indexed?",
                "tool": "uffs_facet_values",
                "arguments": { "field": "drive" },
                "explanation": "Lists all indexed drives with record counts. \
                    Equivalent to uffs_drives but with size totals."
            },
            {
                "title": "Top file types for a specific pattern",
                "tool": "uffs_facet_values",
                "arguments": { "field": "type", "pattern": "*.log", "top": 10 },
                "explanation": "Scope the facet to a search pattern. Shows the type \
                    distribution of files matching *.log."
            },
            {
                "title": "Filter by prefix — extensions starting with 'doc'",
                "tool": "uffs_facet_values",
                "arguments": { "field": "extension", "prefix": "doc", "top": 10 },
                "explanation": "prefix='doc' filters to extensions starting with 'doc': \
                    doc, docx, docm, etc. Useful when you know the start of the value."
            },
            {
                "title": "Paginated facet — first page",
                "tool": "uffs_facet_values",
                "arguments": { "field": "extension", "top": 500, "page_size": 50 },
                "explanation": "page_size=50 returns the first 50 values. Response \
                    includes 'next_cursor'. Pass it back as 'cursor' to get the next \
                    page. Repeat until next_cursor is null."
            },
            {
                "title": "Paginated facet — next page",
                "tool": "uffs_facet_values",
                "arguments": { "field": "extension", "cursor": "eyJza..." },
                "explanation": "Pass the cursor from the previous response. \
                    All other params (field, top, pattern, prefix) are encoded \
                    in the cursor — only 'cursor' is needed."
            }
        ]
    })
}

/// Advanced patterns — NTFS attributes, hidden files, multi-step workflows.
fn cookbook_advanced() -> Value {
    json!({
        "category": "Advanced & NTFS-Specific",
        "description": "Power queries for hidden files, NTFS attributes, and multi-step \
            agent workflows.",
        "examples": [
            {
                "title": "Hidden files only",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "attr": "hidden",
                    "sort": "-size",
                    "limit": 30
                },
                "explanation": "attr='hidden' requires the NTFS Hidden attribute. \
                    Use '!hidden' to exclude hidden files. \
                    Available: hidden, system, compressed, encrypted, sparse, reparse, \
                    archive, readonly, temporary, offline."
            },
            {
                "title": "NTFS-compressed files by size",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "attr": "compressed",
                    "sort": "-size",
                    "limit": 30,
                    "projection": ["name", "size", "allocated", "path"]
                },
                "explanation": "Shows compressed files with both logical size and \
                    allocated (on-disk) size. The difference shows compression savings."
            },
            {
                "title": "File info lookup by path",
                "tool": "uffs_info",
                "arguments": { "path": "C:\\Windows\\System32\\ntoskrnl.exe" },
                "explanation": "Returns full metadata for a specific file: size, timestamps, \
                    MFT attributes, parent directory. Use when you already know the path."
            },
            {
                "title": "Multi-step workflow: disk usage report",
                "what": "Use prompts/get with name='disk_usage_report' for a guided workflow",
                "steps": [
                    "Step 1: uffs_aggregate preset='overview' → get totals and type breakdown",
                    "Step 2: uffs_aggregate preset='by_type' → see which types use most space",
                    "Step 3: uffs_search sort='-size' limit=20 → find the biggest individual files",
                    "Step 4: uffs_aggregate preset='cleanup' → identify reclaimable space"
                ],
                "explanation": "For complex analysis, chain multiple tool calls. \
                    Start with aggregate for the big picture, then drill down with search. \
                    The 'disk_usage_report' prompt automates this exact workflow."
            }
        ]
    })
}

/// Real-world workflows — the most requested queries from user research.
///
/// Based on cross-tool analysis of Everything, `WizFile`, `SearchMyFiles`,
/// `UltraSearch`, and community discussions.  These are the jobs users
/// actually do most often that agents should recognize and fulfill.
#[expect(clippy::too_many_lines, reason = "large static cookbook JSON blob")]
fn cookbook_real_world_workflows() -> Value {
    json!({
        "category": "Real-World Workflows (Most Requested)",
        "description": "The most common things users actually ask for, based on \
            cross-platform search-tool research.  These map natural-language \
            requests to concrete UFFS queries.  If a user asks something similar, \
            use these patterns.",
        "examples": [
            // ── Developer project search ─────────────────────────
            {
                "title": "Find project manifests / config files",
                "user_would_say": "Find all package.json / Cargo.toml / .env files",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "ext": "json,toml,yaml,yml,ini,env,cfg,conf",
                    "filter": "files",
                    "sort": "-modified",
                    "limit": 50
                },
                "explanation": "Developers constantly search for config/manifest files. \
                    Combine with path_contains to scope to a project tree."
            },
            {
                "title": "Find a specific config file by name",
                "user_would_say": "Where is my Cargo.toml / .env / settings.json?",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "Cargo.toml",
                    "whole_word": true,
                    "filter": "files",
                    "limit": 20
                },
                "explanation": "whole_word=true prevents matching 'Cargo.toml.bak'. \
                    Works for package.json, .gitignore, Dockerfile, Makefile, etc."
            },
            // ── Empty folders ────────────────────────────────────
            {
                "title": "Find empty folders",
                "user_would_say": "Show me empty directories / folders with nothing in them",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "dirs",
                    "max_descendants": 0,
                    "sort": "path",
                    "limit": 100
                },
                "explanation": "max_descendants=0 finds directories with zero children. \
                    Common cleanup task — these are safe to remove."
            },
            // ── Long / problematic paths ─────────────────────────
            {
                "title": "Find files with long paths (MAX_PATH issues)",
                "user_would_say": "Find files that exceed 260 characters / MAX_PATH",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "min_path_length": 260,
                    "sort": "-path_length",
                    "limit": 50,
                    "projection": ["name", "size", "path"]
                },
                "explanation": "Windows MAX_PATH is 260 chars.  Files exceeding this \
                    cause issues with older apps, backup tools, and scripts. \
                    Also try min_name_length for long file names specifically."
            },
            // ── Recently accessed ────────────────────────────────
            {
                "title": "Find recently accessed files",
                "user_would_say": "What files were opened / accessed in the last week?",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "newer_accessed": "7d",
                    "sort": "-accessed",
                    "limit": 50,
                    "projection": ["name", "ext", "size", "accessed", "path"]
                },
                "explanation": "newer_accessed uses the NTFS last-access timestamp. \
                    Also available: older_accessed, newer_created, older_created. \
                    Note: Windows may defer access-time updates by default."
            },
            // ── Admin cleanup workflow ───────────────────────────
            {
                "title": "Storage cleanup: old large files on a specific drive",
                "user_would_say": "Find big old files on C: I can delete to free space",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "filter": "files",
                    "min_size": 104_857_600,
                    "older": "365d",
                    "drives": ["C"],
                    "sort": "-size",
                    "limit": 50
                },
                "explanation": "Combines size (>100 MB), age (>1 year), and drive scope. \
                    This is the #1 storage cleanup pattern.  Follow up with \
                    uffs_aggregate preset='cleanup' for a broader analysis."
            },
            {
                "title": "Find forgotten temp/cache files consuming space",
                "user_would_say": "Clean up temp files / cache / old downloads",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "ext": "tmp,temp,log,bak,old,cache",
                    "filter": "files",
                    "min_size": 1_048_576,
                    "sort": "-size",
                    "limit": 50
                },
                "explanation": "Finds temp/cache/backup files over 1 MB sorted by size. \
                    Add path_contains='AppData' or path_contains='Temp' to scope further."
            },
            // ── Media triage ─────────────────────────────────────
            {
                "title": "Find large photos and videos eating disk space",
                "user_would_say": "Why is my disk full? Show me big photos and videos",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "ext": "pictures,videos",
                    "filter": "files",
                    "min_size": 52_428_800,
                    "sort": "-size",
                    "limit": 50,
                    "projection": ["name", "size", "modified", "path"]
                },
                "explanation": "ext='pictures,videos' expands to ~20 media extensions. \
                    min_size=52428800 is 50 MB.  Shows the biggest media files first. \
                    Follow up with uffs_aggregate preset='media' for the full picture."
            },
            // ── DFIR-lite / security triage ──────────────────────
            {
                "title": "Security triage: recent executables",
                "user_would_say": "What executables were added/modified recently?",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*",
                    "type_filter": "executable",
                    "newer": "7d",
                    "sort": "-modified",
                    "limit": 50,
                    "projection": ["name", "size", "modified", "created", "path"]
                },
                "explanation": "type_filter='executable' covers exe, dll, sys, bat, cmd, \
                    ps1, vbs, msi, and more.  Recent executables on a system \
                    are a key indicator in incident response.  Add attr='hidden' \
                    to find hidden executables specifically."
            },
            // ── Migration preparation ────────────────────────────
            {
                "title": "Migration prep: inventory a drive before moving",
                "user_would_say": "I need to move data off D: — what's on it?",
                "tool": "uffs_aggregate",
                "arguments": {
                    "preset": "overview",
                    "drives": ["D"]
                },
                "follow_up": [
                    "uffs_aggregate preset='by_type' drives=['D'] → type breakdown",
                    "uffs_aggregate preset='by_age' drives=['D'] → what's stale",
                    "uffs_search drives=['D'] sort='-size' limit=20 → biggest files",
                    "uffs_search drives=['D'] filter='dirs' sort='-treesize' limit=20 → biggest folders"
                ],
                "explanation": "Start with overview for the big picture, then drill down. \
                    This 4-step workflow gives a complete migration inventory."
            },
            // ── Find executables / installers ────────────────────
            {
                "title": "Find installers and setup files",
                "user_would_say": "Where are my downloaded installers / MSI / setup files?",
                "tool": "uffs_search",
                "arguments": {
                    "pattern": "*setup*",
                    "ext": "exe,msi",
                    "filter": "files",
                    "sort": "-size",
                    "limit": 30
                },
                "explanation": "Combines name pattern with extension filter. \
                    Old installers are a top source of wasted space. \
                    Also try pattern='*install*' or ext='exe,msi,cab,msix'."
            }
        ]
    })
}
