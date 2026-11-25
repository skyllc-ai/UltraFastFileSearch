# Aggregation

> Get answers, not rows.

Aggregation runs **server-side analytics** over the entire file index and
returns compact summaries instead of individual file listings.  One aggregation
call can answer questions that would otherwise require scanning millions of
rows.

| Without aggregation | With aggregation |
|---------------------|------------------|
| `uffs '*' --ext pdf` → 48 000 rows, count them yourself | `uffs --agg overview --ext pdf` → `total_count: 48 000` |
| `uffs '*' --sort -size --limit 50` → top 50, but what % is that? | `uffs --agg by_type` → full breakdown with percentages |
| Three separate queries for C:, D:, E: | `uffs --agg by_drive` → all drives in one call |

## 1. Quick start

```bash
# Full filesystem overview — the single best first command
uffs --agg overview

# What types of files eat the most space?
uffs --agg by_type

# Top 30 extensions by disk usage
uffs --agg by_extension

# How much space does each drive use?
uffs --agg by_drive

# Size distribution — tiny, small, medium, large, huge
uffs --agg by_size

# When were files last modified?
uffs --agg by_age
```

All `uffs --agg` commands accept every filter from the [filters](filters.md) page.
Aggregation reuses the same search pipeline — it just changes the output.

### Inline aggregation (search + aggregate in one command)

You can run aggregation alongside a search using inline flags instead
of the `uffs --agg` subcommand:

```bash
# Count matching files (suppresses rows)
uffs '*.pdf' --count

# Top 20 extensions for this search
uffs '*.pdf' --facet extension

# Size statistics for matching files
uffs '*.pdf' --stats size

# Size histogram
uffs '*.pdf' --histogram size

# Include rows alongside aggregation
uffs '*.pdf' --count --rows

# Raw aggregation spec (power syntax)
uffs '*.pdf' --agg "terms:extension,top=30"
```

By default, `--count`, `--facet`, `--stats`, and `--histogram` suppress
row output.  Add `--rows` to get both.

## 2. Presets

A preset is a one-word shortcut that expands into a tuned set of analytics.

| Preset | What it computes | Typical use |
|--------|------------------|-------------|
| `overview` | Total count, files vs dirs, size stats (sum/min/max/avg), type facet, drive facet, monthly modified histogram | "Give me the lay of the land" |
| `by_type` | Semantic type breakdown (code, document, picture, video, archive, …) with count, size, waste, share% | "What types of files do I have?" |
| `by_extension` | Top 50 extensions by count & size with share% | "Which extensions use the most space?" |
| `by_drive` | Per-drive totals: count, logical size, allocated size, waste | "How full is each drive?" |
| `by_size` | Size-bucket histogram: Empty, Tiny (<1 KB), Small (<1 MB), Medium (<100 MB), Large (<1 GB), Huge (<10 GB), Massive (10 GB+) | "What's the size distribution?" |
| `by_age` | Age buckets: today, this_week, this_month, this_quarter, this_year, last_year, older, ancient (>5 yr) | "How stale is my data?" |
| `storage` | Logical vs allocated size, waste per drive, waste per extension | "Where's my slack space going?" |
| `activity` | Modified/created/accessed monthly histograms | "When are files being touched?" |
| `top_folders` | Largest top-level folders (path rollup at depth 1) with count & size | "Which root folders are biggest?" |
| `duplicates` | Candidate duplicate groups (same name + size), with reclaimable bytes | "Do I have duplicate files?" |
| `media` | Media-only breakdown: pictures/audio/video by type, extension, size, creation date | "What media do I have?" |
| `cleanup` | Zero-byte files, no-extension files, distinct extension count, total files | "What's worth cleaning up?" |

### Using presets

```bash
# Basic preset
uffs --agg by_type

# Scoped to one drive
uffs --agg by_extension --drives C

# Scoped to a file pattern
uffs --agg by_size "*.rs"

# Scoped with filters
uffs --agg by_type --newer 30d --min-size 1mb

# JSON output for piping
uffs --agg overview --format json

# CSV output for spreadsheets
uffs --agg by_extension --format csv
```

## 3. Custom aggregation specs

When presets aren't enough, build your own with `--agg`:

### 3.1 Power syntax

```
KIND:FIELD,option=value,option=value
```

The `--agg` flag is repeatable — stack multiple specs in one command.

### 3.2 Available kinds

| Kind | Syntax | What it does |
|------|--------|--------------|
| **count** | `count` | Total matching record count |
| **stats** | `stats:FIELD` | Sum, min, max, avg for a numeric/timestamp field |
| **terms** | `terms:FIELD,top=N` | Top-N values by count, with size metrics per bucket |
| **histogram** | `hist:FIELD,interval=N` | Fixed-width numeric buckets |
| **date histogram** | `datehist:FIELD,calendar=INTERVAL` | Calendar-aligned time buckets |
| **range** | `range:FIELD,bins=A..B+C..D` | Custom numeric ranges |
| **missing** | `missing:FIELD` | Count records where the field has no value |
| **distinct** | `distinct:FIELD` | Count unique values |
| **rollup** | `rollup:path,depth=N,top=N` | Directory tree rollup at a given depth |
| **duplicates** | `duplicates:KEY+KEY,top=N` | Duplicate candidate detection |

### 3.3 Examples

```bash
# Top 20 extensions (default is 50)
uffs "*" --agg "terms:extension,top=20"

# Size statistics for all files
uffs "*" --agg "stats:size"

# Modified date statistics
uffs "*" --agg "stats:modified"

# Size histogram with 10 MB buckets
uffs "*" --agg "hist:size,interval=10485760"

# Monthly creation timeline
uffs "*" --agg "datehist:created,calendar=month"

# Custom size ranges
uffs "*" --agg "range:size,bins=0..1048576+1048576..1073741824+1073741824..∞"

# Count files with no extension
uffs "*" --agg "missing:extension"

# How many unique extensions exist?
uffs "*" --agg "distinct:extension"

# Top 10 folders at depth 2
uffs "*" --agg "rollup:path,depth=2,top=10"

# Multiple specs in one command
uffs "*" --agg "count" --agg "stats:size" --agg "terms:type,top=10"
```

## 4. Groupable and aggregatable fields

Not every field supports every operation.  Here's what works:

### 4.1 Fields you can aggregate (stats: sum, min, max, avg)

| Field | What it measures |
|-------|------------------|
| `size` | Logical file size in bytes |
| `size_on_disk` | Allocated size on disk (includes slack) |
| `created` | NTFS creation timestamp |
| `modified` | NTFS last-write timestamp |
| `accessed` | NTFS last-access timestamp |
| `descendants` | Child count (directories only) |
| `tree_size` | Recursive subtree size (directories only) |
| `tree_allocated` | Recursive subtree allocated size |
| `bulkiness` | allocated ÷ logical × 100 |
| `name_length` | Character count of file name |
| `path_length` | Character count of full path |

### 4.2 Fields you can group by (terms, rollup)

| Field | Cardinality | Example values |
|-------|-------------|----------------|
| `drive` | Fixed (≤26) | `C`, `D`, `E` |
| `type` | Low (~24) | `code`, `picture`, `video`, `document`, `archive` |
| `extension` | Medium (~2 000) | `rs`, `pdf`, `dll`, `jpg` |
| `name` | Unbounded | Full file names — use with `duplicates` |
| `path_only` | Unbounded | Directory-only portion of path |
| `directory` | Fixed (2) | `true` / `false` |
| `hidden` | Fixed (2) | `true` / `false` |
| `system` | Fixed (2) | `true` / `false` |
| `compressed` | Fixed (2) | `true` / `false` |
| `encrypted` | Fixed (2) | `true` / `false` |
| `read_only` | Fixed (2) | `true` / `false` |
| `archive` | Fixed (2) | `true` / `false` |
| `sparse` | Fixed (2) | `true` / `false` |
| `reparse` | Fixed (2) | `true` / `false` |
| `temporary` | Fixed (2) | `true` / `false` |
| `offline` | Fixed (2) | `true` / `false` |

### 4.3 Fields you can bucket (histogram, date histogram)

All aggregatable fields (§4.1) support bucketing — that is, grouping into
fixed-width or calendar-aligned ranges.

## 5. Samples and drill-down

Every terms/rollup/duplicate bucket can optionally include **sample rows**
— a few representative files from that bucket — so you can see what's
actually in each group without a follow-up search.

```bash
# Top 10 extensions with 3 sample files each
uffs "*" --agg "terms:extension,top=10,sample=3"
```

Output:
```
Key                Count   Total Size   Count%    Size%
─────────────── ──────── ────────── ──────── ────────
dll               185,000   412.0 GB    15.3%    22.1%
  → ntdll.dll (2.1 MB) modified:2025-12-01
  → msvcrt.dll (1.8 MB) modified:2025-11-15
  → kernel32.dll (1.2 MB) modified:2025-12-01
exe                42,000   298.0 GB     8.6%    14.6%
  → Teams.exe (289 MB) modified:2026-03-20
  → chrome.exe (245 MB) modified:2026-04-01
  → code.exe (132 MB) modified:2026-03-28
```

Each bucket also carries a **drill-down predicate** — the exact filter
needed to re-query just that bucket's contents:

```json
{ "drilldown": [{ "field": "extension", "op": "eq", "value": "dll" }] }
```

An MCP agent can read this and construct a targeted `uffs_search` call
automatically.

## 6. Pagination

High-cardinality aggregations (extensions, folder rollups) may produce more
buckets than the default top-N.  Use pagination to walk through all of them:

```bash
# First page
uffs --agg by_extension --agg-page-size 20

# Next page (use the cursor from the previous response)
uffs --agg by_extension --agg-page-size 20 --agg-cursor "eyJza..."
```

The response includes:
- `next_cursor` — opaque token for the next page (null if this is the last)
- `other_count` — how many records fell into buckets beyond the current page
- `values_complete` — `true` if all values fit in the current page

## 7. Duplicate detection

The `duplicates` preset finds files that share the same **name and size**
— probable duplicates that waste disk space.

```bash
# Basic duplicate scan
uffs --agg duplicates

# With verification (reads first 4 KB of each candidate)
uffs "*" --agg "duplicates:size+name,verify=first_bytes,top=50"

# Full SHA-256 verification (slow but certain)
uffs "*" --agg "duplicates:size+name,verify=sha256,top=20"
```

Output:
```
Duplicate Candidates — 1,847 groups, 12,340 files, 48.2 GB reclaimable

Name                    Copies  File Size  Reclaimable  Verified
─────────────────────── ────── ────────── ─────────── ────────
boost_1_90_0.zip            3    278 MB       556 MB       ✓
node_modules.tar.gz         5    145 MB       580 MB       ✓
```

### Verification modes

| Mode | Speed | Certainty | I/O |
|------|-------|-----------|-----|
| `none` (default) | Instant | Name+size match only | Zero |
| `first_bytes` | Fast | Reads first 4 KB per candidate | Light |
| `sha256` | Slow | Full cryptographic hash comparison | Heavy |

A **verification budget** caps I/O: 256 MB and 10 000 file reads by default.
Groups beyond the budget are kept but marked unverified.

## 8. Output formats

```bash
# Human-readable table (default)
uffs --agg by_extension

# JSON — structured, complete, pipe-friendly
uffs --agg by_extension --format json

# CSV — for spreadsheets and data tools
uffs --agg by_extension --format csv

# TSV — tab-separated variant
uffs --agg by_extension --format tsv
```

### JSON structure

Every aggregation response contains:

```json
{
  "label": "by_extension",
  "kind": "terms",
  "field": "extension",
  "buckets": [
    {
      "key": "dll",
      "count": 185000,
      "total_bytes": 442000000000,
      "total_allocated": 445000000000,
      "avg_size": 2389189.2,
      "share_count": 15.3,
      "share_bytes": 22.1,
      "sample_rows": [...],
      "drilldown": [...]
    }
  ],
  "other_count": 42000,
  "total_groups": 1847,
  "values_complete": false,
  "next_cursor": "eyJza..."
}
```

## 9. Combining aggregation with search

By default, `uffs --agg` returns **only** aggregate results and no file rows.
Use `--rows` to get both:

```bash
# Aggregate + rows
uffs "*.rs" --agg "stats:size" --rows

# Count files matching a filter (no rows)
uffs "*.pdf" --count

# Facet a filtered set
uffs "*" --newer 7d --facet type
```

### Shorthand flags

| Flag | Equivalent power syntax | What it does |
|------|------------------------|-------------|
| `--count` | `count` | Total count, no rows |
| `--facet FIELD` | `terms:FIELD,top=20` | Top values of a field |
| `--facet FIELD:N` | `terms:FIELD,top=N` | Top N values |
| `--stats FIELD` | `stats:FIELD` | Sum/min/max/avg for a field |
| `--histogram FIELD` | `hist:FIELD` | Size-bucketed histogram |

## 10. MCP / daemon API

For MCP agents and HTTP clients, aggregation is accessed through the
`uffs_aggregate` tool (or the `aggregations` array on `uffs_search`).

### Preset call

```json
{
  "tool": "uffs_aggregate",
  "arguments": {
    "preset": "overview"
  }
}
```

### Custom specs

```json
{
  "tool": "uffs_aggregate",
  "arguments": {
    "aggregations": ["terms:extension,top=30", "stats:size"]
  }
}
```

### Scoped aggregation

```json
{
  "tool": "uffs_aggregate",
  "arguments": {
    "preset": "by_type",
    "drives": ["C"],
    "pattern": "*.rs"
  }
}
```

See `uffs://cookbook` in the MCP resource list for more examples.

## 11. Recipes

### "How much space do my photos take?"

```bash
uffs --agg overview --ext pictures
```

### "Which drive has the most waste?"

```bash
uffs --agg storage
```

### "Are there files near the MAX_PATH limit?"

```bash
uffs "*" --agg "range:path_length,bins=0..100+100..200+200..260+260..500"
```

### "What's the creation timeline of my music collection?"

```bash
uffs --agg activity --ext music --drives M
```

### "Show me the 10 largest top-level folders, with their type breakdown"

```bash
uffs "*" --agg "rollup:path,depth=1,top=10,sub=terms:type"
```

### "How many unique extensions are on D:?"

```bash
uffs "*" --agg "distinct:extension" --drives D
```

### "Find all Rust code stats in my GitHub folder"

```bash
uffs --agg overview "*.rs" --path-contains GitHub
```

### "Monthly modification activity for the last 2 years"

```bash
uffs "*" --newer 730d --agg "datehist:modified,calendar=month"
```

## 12. Performance notes

Aggregation operates directly on the compact in-memory index — the same
data structure used for search.  Key performance characteristics:

| Scenario | Records | Time | Notes |
|----------|--------:|-----:|-------|
| `overview` (no grouping) | 25M | ~50 ms | Single counter pass |
| `by_extension` (~2 000 groups) | 25M | ~80 ms | Hash map, extension IDs (no strings) |
| `by_type` (24 groups) | 25M | ~60 ms | Fixed-size array, no hashing |
| `duplicates` (by name) | 25M | ~200 ms | Hash map with string keys |

**No path resolution** — aggregation skips the expensive parent-chain walk
used to reconstruct full paths.  It operates on `CompactRecord` fields
(size, extension ID, flags, timestamps) directly.

**Zero string allocation** — for extension aggregation, records are keyed by
`extension_id` (a `u16`).  String names are resolved only for the final
top-N buckets in the output.

**Parallel per drive** — each indexed drive is scanned in its own thread.
Per-drive accumulators are merged at the end (O(G) where G = group count).

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Output Formats](output-formats.md): CSV, JSON and table output, and choosing columns
