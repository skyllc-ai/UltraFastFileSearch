# CLI Overview

`uffs` is the command-line interface for Ultra Fast File Search.  It reads
NTFS Master File Tables directly and searches millions of files in
milliseconds.

**Search is the default action** — just type `uffs <pattern>`.  No
command required; management lives under `--<command>` (§6).

![UFFS CLI: real searches, filters, and aggregations across millions of indexed files with measured latency](../../assets/demo/uffs-cli.gif)

> *Real commands, real result counts, real measured latency on a hot daemon. Recorded against the real CLI (see the [demo kit](../../scripts/dev/demo/README.md)).*

## 1. Search (Default Action)

When the first token is not a `--command`, `uffs` performs a search.  The
first positional argument is the pattern.

```
uffs <PATTERN> [OPTIONS]
```

### Pattern Syntax

| Syntax | Mode | Detail |
|--------|------|--------|
| `*.ext` | Glob | Wildcards `*` and `?`, and `\|` for OR (`*.rs\|*.toml`) → [Search Modes §2](search-modes.md#2-glob-patterns-default) |
| `word` | Literal | Case-insensitive substring match on the file name → [Search Modes §3](search-modes.md#3-literal-search) |
| `>regex` | Regex | `>` prefix activates regex (matched against the file name) → [Search Modes §4](search-modes.md#4-regex-search) |
| `src\*.rs` | Path-aware | Contains `\` or `/`: walks directory names → [Search Modes §5](search-modes.md#5-path-aware-patterns) |
| `C:` or `C:\` | Drive | Everything on that drive → [Search Modes §13](search-modes.md#13-drive-scoping) |

The pattern is matched against the **file name**, not the full path,
unless it contains a `\` or `/`.  A pattern cannot start with `-` (it
would be read as a flag); for names that begin with a dash, use
`--begins-with`, e.g. `uffs --begins-with --draft`.

### Scope Prefixes (Everything-compatible)

| Prefix | Effect | Detail |
|--------|--------|--------|
| `path:` | Match names as usual, and also return **everything under** each matching directory | [Search Modes §10](search-modes.md#10-scope-prefixes) |
| `dir:` | Only search **directories** | Same as `--dirs-only` |
| `file:` | Only search **files** | Same as `--files-only` |

### Pattern Sugar

| Flag | Effect | Detail |
|------|--------|--------|
| `--begins-with <PREFIX>` | Sugar for `PREFIX*` | [Search Modes §11](search-modes.md#11-pattern-sugar-flags) |
| `--ends-with <SUFFIX>` | Sugar for `*SUFFIX` | Same |
| `--contains <NEEDLE>` | Sugar for `*NEEDLE*` | Same |
| `--not-contains <NEEDLE>` | Exclude names containing NEEDLE | Like `--exclude '*NEEDLE*'`; use one or the other, not both |

If you give more than one way to build the pattern, the first one wins in
this order: the pattern itself, `--begins-with`, `--ends-with`,
`--contains`.

### Search Modifiers

| Flag | Effect | Detail |
|------|--------|--------|
| `--case` | Case-sensitive matching | [Search Modes §6](search-modes.md#6-case-sensitivity) |
| `--smart-case` | Auto case-sensitive if pattern has uppercase | [Search Modes §6](search-modes.md#6-case-sensitivity) |
| `--word` | Accepted, but currently has no effect | [Search Modes §7](search-modes.md#7-whole-word-matching---word) |
| `--name-only` | Names are already matched by default; this flag makes a pattern with `\` or `/` an error | [Search Modes §8](search-modes.md#8-name-only-matching---name-only) |
| `--in-path <GLOB>` | Keep only entries whose directory path matches | [Search Modes §12](search-modes.md#12-path-directory-filter) |
| `--not-in-path <GLOBS>` | Drop entries whose directory path matches any of these (comma-separated) | [Filters §8](filters.md#8-path-filter) |

### Drive Selection

| Flag | Example | Effect |
|------|---------|--------|
| `-d, --drive <X>` | `--drive C` | Search single drive |
| `--drives <X,Y>` | `--drives C,D,E` | Search multiple drives concurrently |
| Pattern prefix | `c:/*.dll` | Infer drive from pattern |

### Data Sources

| Flag | Description |
|------|-------------|
| `--mft-file <PATH>` | Use offline raw MFT file(s) instead of live volume |
| `--data-dir <DIR>` | Auto-discover MFT files in `drive_*` subdirectories |
| `--no-cache` | Bypass cache; re-read MFT fresh |

## 2. Filters

All filters are detailed in the [Filters guide](filters.md).  Summary:

| Flag | Category | Quick Description |
|------|----------|-------------------|
| `--files-only` | Scope | Files only |
| `--dirs-only` | Scope | Directories only |
| `--hide-system` | Scope | Hide the NTFS metafiles (`$MFT`, `$Bitmap`, `$LogFile`, …) |
| `--hide-ads` | Scope | Hide Alternate Data Stream entries |
| `--min-size <SIZE>` | Size | Minimum logical size (e.g. `100MB`, `1GB`) |
| `--max-size <SIZE>` | Size | Maximum logical size |
| `--exact-size <SIZE>` | Size | Exactly this size (shorthand for min=max) |
| `--min-size-on-disk <SIZE>` | Size | Minimum allocated size ([concept](concepts.md#1-size-vs-size-on-disk)) |
| `--max-size-on-disk <SIZE>` | Size | Maximum allocated size |
| `--exact-size-on-disk <SIZE>` | Size | Exactly this allocated size |
| `--newer <SPEC>` | Date | Modified within / after |
| `--older <SPEC>` | Date | Modified before |
| `--newer-created <SPEC>` | Date | Created within / after |
| `--older-created <SPEC>` | Date | Created before |
| `--newer-accessed <SPEC>` | Date | Accessed within / after |
| `--older-accessed <SPEC>` | Date | Accessed before |
| `--between <START,END>` | Date | Time range shorthand (e.g. `2026-01-01,2026-03-31`) |
| `--month <SPEC>` | Date | Month-of-year filter (jan, Q1, etc.) |
| `--attr <LIST>` | Attribute | Require/exclude NTFS attributes |
| `--type <CATEGORY>` | Type | Semantic type: code, picture, video, … |
| `--ext <LIST>` | Extension | Filter by extension or collection |
| `--min-descendants <N>` | Tree | Min entries in the whole subtree, +1 for the directory itself (an empty directory has 1) |
| `--max-descendants <N>` | Tree | Max entries in the whole subtree (+1 for the directory) |
| `--exact-descendants <N>` | Tree | Exactly N (same count) |
| `--min-treesize <SIZE>` | Tree | Min subtree logical size ([concept](concepts.md#2-tree-size-and-tree-allocated)) |
| `--max-treesize <SIZE>` | Tree | Max subtree logical size |
| `--min-tree-allocated <SIZE>` | Tree | Min subtree allocated size |
| `--max-tree-allocated <SIZE>` | Tree | Max subtree allocated size |
| `--min-bulkiness <N>` | Derived | Min waste ratio ([concept](concepts.md#3-bulkiness-waste-ratio)) |
| `--max-bulkiness <N>` | Derived | Max waste ratio |
| `--min-name-length <N>` | Derived | Min filename character count |
| `--max-name-length <N>` | Derived | Max filename character count |
| `--min-path-length <N>` | Derived | Min full-path character count |
| `--max-path-length <N>` | Derived | Max full-path character count |
| `--in-path <GLOB>` | Path | Directory path must match glob |
| `--not-in-path <GLOBS>` | Path | Directory path must match none of these globs (comma-separated) |
| `--exclude <GLOB>` | Exclude | Exclude matching filenames |
| `--malformed` | Forensic | Only ill-formed-UTF-16 names ([guide](filters.md#14a-malformed-name-filters-forensic)) |
| `--well-formed` | Forensic | Only valid names (inverse of `--malformed`) |
| `--malformed-path` | Forensic | Match when any path segment is ill-formed |
| `--normalize-malformed` | Forensic | Display: render corrupt code units as `<BAD:HHHH>` not `�` |
| `--diff <BASELINE>` | Forensic | Search the **deleted** set vs a baseline MFT capture; composes with every filter above ([Delete Visibility](../architecture/engine/12-forensics-diagnostics.md#delete-visibility-uffs-cli)) |
| `-n, --limit <N>` | Limit | Max results (0 = unlimited) |

## 3. Sorting

All 36+ sortable columns are detailed in the [Sorting guide](sorting.md).
Summary:

| Flag | Example | Effect |
|------|---------|--------|
| `--sort <SPEC>` | `--sort -size` | Sort by column: ascending, or descending with a leading `-` or `:desc` |
| `--sort <SPEC>` | `--sort size:asc,name` | Multi-tier with explicit direction |
| `--sort-desc` | `--sort size --sort-desc` | Make the primary sort column descending |

**Popular sort columns:** `size`, `modified`, `created`, `name`, `ext`,
`path`, `treesize`, `bulkiness`, `pathlength`, `namelength`,
`descendants`, `type`, `hidden`, `compressed`, `directory_flag`.
See [Sorting §2](sorting.md) for the full list.

## 4. Output Control

All output options are detailed in the [Output Formats guide](output-formats.md).
Summary:

| Flag | Default | Description |
|------|---------|-------------|
| `-f, --format <FMT>` | see below | Output format: `csv`, `json`, `table`, `custom` |
| `--columns <LIST>` | see below | Columns to output (comma-separated, `all`, or `parity`) |
| `--out <DEST>` | `console` | Output destination: `console` or a filename (always written as CSV) |
| `--sep <CHAR>` | `,` | Column separator (CSV mode) |
| `--quotes <CHAR>` | `"` | Quote character for string fields |
| `--header <BOOL>` | `true` | Include header row (`--header false` to suppress) |
| `--pos <STR>` | `1` | Representation for true/active boolean attributes |
| `--neg <STR>` | `0` | Representation for false/inactive boolean attributes |

CSV is the format for scripts and files: it is used when the output is
piped or redirected, and always with `--out`.  `--format table` gives an
aligned table for reading in a terminal.  Name the columns you want with
`--columns`, for example `--columns name,size,modified,path`; the default
set depends on how the output is produced (see
[Output Formats §5](output-formats.md#5-column-selection)).

## 5. Inline Aggregation Flags

These flags run server-side analytics alongside (or instead of) search
results.  For the `uffs --agg` command, see §6 below.

> `--stats` and `--agg` are dual-use: as the **first token**
> (`uffs --stats`, `uffs --agg <preset>`) they are commands; **after a
> pattern** (`uffs '*.log' --stats size`) they are inline modifiers on a
> search.  The first token decides — see [CLI Grammar](../architecture/cli-grammar.md).

| Flag | Effect |
|------|--------|
| `--count` | Show total matching count (suppresses rows) |
| `--facet <FIELD[:TOP]>` | Top-N breakdown by field (e.g. `--facet extension`) |
| `--stats <FIELD>` | Scalar statistics for a numeric field |
| `--histogram <FIELD[:INTERVAL]>` | Histogram buckets |
| `--agg <SPEC>` | Raw aggregation spec (power syntax) |
| `--rows` | Include matching rows alongside aggregates |
| `--agg-cursor <CURSOR>` | Continue from a previous page |
| `--agg-page-size <N>` | Max buckets per page |

> **Full aggregation guide:** [Aggregation](aggregation.md)

## 6. Commands

> Building/loading an index is **daemon-managed** — there is no standalone
> `uffs index` command. Point the daemon at a live drive or a raw MFT with
> `uffs --daemon start --data-dir <dir>` / `--mft-file <file>` (see
> [Daemon](daemon.md)). For low-level NTFS volume/record inspection, use the
> separate `uffs-mft` tool (`uffs-mft --help`).

### `uffs --search`

An explicit search, the same as the bare default: `uffs --search '*.pdf'`
is `uffs '*.pdf'`.

### `uffs --stats`

Show file statistics: connects to the daemon and runs the `overview`
aggregate preset.  `--data-dir` and `--mft-file` are passed on to the
daemon.

```bash
uffs --stats                    # Live overview of every loaded drive
```

### `uffs --agg` (alias: `uffs --aggregate`)

Run server-side analytics on the filesystem index.  Returns aggregate
results only — no file rows.

```bash
uffs --agg overview         # Full filesystem overview
uffs --agg by_extension     # Top 50 extensions
uffs --agg by_type          # Breakdown by file type
uffs --agg by_drive         # Per-drive totals
uffs --agg by_size          # Size distribution
uffs --agg by_age           # Age distribution
uffs --agg count            # Simple total count
```

> **Full guide:** [Aggregation](aggregation.md)

### `uffs --snapshot`: capture a baseline MFT

Save the drive's current MFT to a file so a later `uffs --diff` can report
what was deleted since. Reads the live NTFS MFT: **Windows + Administrator**.

```bash
uffs --snapshot --drive C --out C_baseline.bin   # zstd-compressed (default)
uffs --snapshot --drive C --out C_base.bin --no-compress
```

| Option | Effect |
|--------|--------|
| `-d, --drive <D>` | Drive to capture (required) |
| `-o, --out <FILE>` | Output file (required) |
| `--no-compress` | Store uncompressed |
| `--compression-level <N>` | zstd level 1-22 (default 3) |
| `--raw` | Headerless raw dump for other MFT tools; implies `--no-compress`, and `uffs --diff` cannot load it |

### `uffs --deleted`: forensic tombstone read

Surface recently-deleted files straight from not-in-use MFT records — **no
baseline needed** — reconstructing each path from the surviving parent chain.
Best-effort: only deletes whose MFT slot hasn't been recycled are visible, and
the timestamp is the file's own last-write, not the deletion time.

```bash
uffs --deleted --drive C                 # live volume (Windows, elevated)
uffs --deleted --mft-file C_old.bin      # or an offline capture
uffs --deleted --drive C --limit 50 --json
```

> Deleting *with filters* since a baseline? Use the `--diff` search flag (§2)
> instead — it runs the full filter/sort/output pipeline over the deleted set.
> **Full guide:** [Delete Visibility](../architecture/engine/12-forensics-diagnostics.md#delete-visibility-uffs-cli)

### `uffs --daemon`

Manage the UFFS background daemon.  The daemon starts automatically on
first search — these commands give explicit control.

```bash
uffs --daemon start --data-dir ~/uffs_data   # Start with specific data
uffs --daemon status                          # Check status (short)
uffs --daemon status -v                       # Long view + performance counters
uffs --daemon status --json                   # Machine-readable (status+drives+stats)
uffs --daemon stop                            # Graceful shutdown
uffs --daemon kill                            # Force kill + cleanup
uffs --daemon restart                         # Stop then restart
```

More actions:

| Action | Effect |
|--------|--------|
| `load` | Load more MFT files or drives into the running daemon |
| `hibernate [DRIVE...]` | Free a drive's memory; its encrypted cache stays on disk |
| `preload <DRIVE...>` | Bring drives back into memory and keep them there for a while |
| `forget <DRIVE...>` | Remove drives from the daemon and delete their caches |
| `resident on\|off\|status` | Keep UFFS running from login, without idle shutdown |
| `status_drives` | Per-drive memory tier and statistics |
| `changed-since <DRIVE>` | Files changed since a USN-journal cursor |

`uffs --daemon --help` lists the options of each action.

> **Full guide:** [Daemon](daemon.md)

### `uffs --mcp`

Manage the MCP server for AI agent integration.

```bash
uffs --mcp run                      # Run on stdin/stdout (for AI hosts)
uffs --mcp start                    # Start HTTP server on :8080 (background)
uffs --mcp start --port 9090        # Custom port
uffs --mcp status                   # Is the MCP server process running?
uffs --mcp stats                    # MCP and daemon performance stats
uffs --mcp stop                     # Graceful shutdown
uffs --mcp reload                   # Reload all MCP sessions after binary update
```

> **Full guide:** [MCP Server](mcp.md)

### `uffs --status`

Show combined system status — daemon + Access Broker + MCP health in one
view.

```bash
uffs --status                       # Short: one glyph-led line per component
uffs --status -v                    # Long: expand every section
uffs --status --json                # Machine-readable superset of all sections
uffs --status --brief               # Short JSON: only the up/down flags a supervisor needs
```

### `uffs --update`

Update UFFS in place.  Without an action it updates to the latest
release; actions include `check`, `doctor`, `repair`, `apply` and
`recover`.  `--upgrade` is accepted as another name for it.  See
[Updating](updating.md).

```bash
uffs --update                       # Update to the latest release
uffs --update doctor                # Self-update health check
```

### `uffs --uninstall`

Remove UFFS from this machine.  See [Uninstall](uninstall.md).

### `uffs --help` / `uffs --version`

Print the help text, or the version.

## 7. Advanced / Diagnostic Flags

These flags are for power users, profiling, and parity testing:

| Flag | Description |
|------|-------------|
| `-v, --verbose` | Enable verbose output (global) |
| `--profile` | Show detailed timing breakdown |
| `--benchmark` | Time the whole pipeline including output formatting; the rows are rendered into a sink instead of the screen (add `-v` to also print them). The profile block reports the output pass as its own line |
| `--no-output` | Match only: the daemon builds no rows, so this times "how many files match" and nothing else. Auto-set when stdout is a null device |
| `--no-cache` | Bypass cache; re-read MFT fresh |
| `--tz-offset <HOURS>` | Show CSV timestamps at this offset from UTC instead of the local one |
| `--parity-compat` | C++ parity-compatible output (25 baseline columns) |

> **Full diagnostics guide:** [Advanced Diagnostics](advanced-diagnostics.md)

## 8. Examples

For a comprehensive recipe gallery organized by workflow (quick find,
cleanup, developer/admin, attribute search, output piping), see
**[Getting Started §5 — Recipes](getting-started.md#5-recipes)**.

Quick taste:

```bash
uffs '*.pdf' --newer 7d --sort -size --limit 20
uffs '*' --dirs-only --max-descendants 1     # Empty directories
uffs '>.*\.log$' --newer 24h
```

## See also

- [Getting Started](getting-started.md): your first search and everyday recipes
- [Search Modes](search-modes.md): glob, literal and regex patterns
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Sorting](sorting.md): sort columns and multi-tier sorts
- [Output Formats](output-formats.md): CSV, JSON and table output, and choosing columns
