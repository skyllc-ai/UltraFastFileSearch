# Sorting

UFFS supports single- and multi-tier sorting of search results.  Sort
specifications are applied after all filters, so you are always sorting
the final result set.

## 1. Basic Usage

```bash
# Sort by file size (largest first — size defaults to descending)
uffs '*.pdf' --sort size

# Sort by name (A → Z — name defaults to ascending)
uffs '*.txt' --sort name

# Sort by modification date (newest first — date defaults to descending)
uffs '*.log' --sort modified
```

Without `--sort`, results are returned in the natural order produced by
the search engine (typically by MFT record number).

## 2. Sort Columns

> **New to UFFS?** See [Concepts](concepts.md) for what each metric
> (Size on Disk, Tree Size, Bulkiness, etc.) actually measures.

### Core Fields

| Column Name | Aliases | Default | Description |
|-------------|---------|---------|-------------|
| `name` | — | asc | Filename (case-folded) |
| `size` | — | desc | Logical file size in bytes |
| `sizeondisk` | `allocated` | desc | Allocated size on disk ([why it differs](concepts.md#1-size-vs-size-on-disk)) |
| `modified` | `date`, `written` | desc | Last modification timestamp |
| `created` | — | desc | Creation timestamp |
| `accessed` | — | desc | Last access timestamp |
| `path` | — | asc | Full path (case-folded) |
| `path_only` | `pathonly` | asc | Directory portion of path |
| `drive` | `drv` | asc | Drive letter |
| `ext` | `extension` | asc | File extension (case-folded) |

### Derived Fields

| Column Name | Aliases | Default | Description |
|-------------|---------|---------|-------------|
| `type` | `folder` | asc | Semantic file category (`code`, `picture`, …) |
| `descendants` | — | desc | Direct child count (directories) |
| `tree_size` | `treesize` | desc | Sum of logical sizes in subtree ([concept](concepts.md#2-tree-size-and-tree-allocated)) |
| `tree_allocated` | `treeallocated` | desc | Sum of allocated sizes in subtree |
| `bulkiness` | — | desc | Waste ratio % ([concept](concepts.md#3-bulkiness-waste-ratio)) |
| `name_length` | `namelength`, `namelen` | desc | Character count of filename |
| `path_length` | `pathlength`, `pathlen` | desc | Character count of full path |

### Boolean Attribute Fields

All 19 NTFS attribute flags are sortable.  They group `true` before
`false` (descending) or `false` before `true` (ascending), with a
name tiebreaker within each group.

| Column Name | Aliases | Default | What it groups |
|-------------|---------|---------|---------------|
| `hidden` | `h` | desc | Hidden files first |
| `system` | `s` | desc | System files first |
| `archive` | `a` | desc | Modified-since-backup first |
| `read_only` | `readonly`, `r` | desc | Read-only files first |
| `compressed` | — | desc | NTFS-compressed first |
| `encrypted` | — | desc | EFS-encrypted first |
| `sparse` | — | desc | Sparse files first |
| `reparse` | — | desc | Symlinks/junctions first |
| `offline` | `o` | desc | Offline/tiered first |
| `not_indexed` | `notindexed` | desc | Not-indexed first |
| `temporary` | `temp` | desc | Temp files first |
| `directory_flag` | `directory`, `dir`, `directoryflag` | desc | Directories first |
| `virtual` | — | desc | Virtual files first |
| `pinned` | — | desc | Pinned files first |
| `unpinned` | — | desc | Unpinned files first |
| `integrity` | — | desc | Integrity-stream first |
| `no_scrub` | `noscrub` | desc | No-scrub first |
| `recall_on_open` | `recallonopen` | desc | Recall-on-open first |
| `recall_on_data_access` | `recallondataaccess` | desc | Recall-on-data-access first |

### Smart Defaults

Every column has a **sensible default direction** so you rarely need to
specify `:asc` or `:desc` explicitly:

- **Numeric / date / boolean columns** default to **descending** — you
  usually want the biggest, newest, or flagged items first.
- **Text columns** (name, path, drive, ext, type, path_only) default to
  **ascending** — alphabetical order is the natural expectation.

## 3. Explicit Direction

You can override the default direction with a `:asc` or `:desc` suffix:

```bash
# Size ascending (smallest first)
uffs '*.dll' --sort size:asc

# Name descending (Z → A)
uffs '*.txt' --sort name:desc

# Modified ascending (oldest first)
uffs '*.log' --sort modified:asc
```

### `--sort-desc` Flag

The `--sort-desc` flag reverses the direction of the **primary** sort
column (useful when you want the opposite of the smart default):

```bash
# Normally size defaults to descending — this flips it to ascending
uffs '*.iso' --sort size --sort-desc
```

> When using `:asc` / `:desc` suffixes, the `--sort-desc` flag is
> ignored for columns that already have an explicit direction.

## 4. Multi-Tier Sorting

Separate multiple sort columns with commas.  The first column is the
primary sort; subsequent columns break ties.

```bash
# Primary: size descending, tiebreaker: name ascending
uffs '*.dll' --sort size,name

# Primary: modified descending, then size descending, then name ascending
uffs '*.log' --sort modified,size,name

# Explicit directions on each tier
uffs '*.pdf' --sort modified:desc,size:asc,name
```

### Automatic Name Tiebreaker

If `name` does not appear anywhere in your sort specification, UFFS
automatically appends a **name-ascending** tiebreaker.  This ensures
deterministic, reproducible ordering even when the primary column has
many identical values (e.g. thousands of files with the same size).

```bash
# You type:
uffs '*.exe' --sort size

# UFFS internally sorts by:  size:desc, name:asc
```

## 5. Deterministic String Sorting

String-based columns (`name`, `path`, `ext`) use a **two-phase
comparison** that is both intuitive and fully deterministic:

1. **Case-folded primary** — uppercase and lowercase letters are treated
   identically, so `readme.txt` groups next to `README.txt` (matching
   Windows Explorer behaviour).
2. **Exact tiebreaker** — when two names differ only in casing, a
   Unicode codepoint comparison decides the order, giving a stable,
   reproducible sequence (works correctly for ASCII, accented Latin,
   CJK, and any other script).

This means you always get the same output for the same data, unlike tools
that treat case variants as "equal" and leave their order to chance.

```
Example ordering for three files that differ only in casing:

  README.txt      ← uppercase first  (R < r in byte order)
  Readme.txt
  readme.txt      ← lowercase last
```

The case-folding uses a Schwartzian transform (pre-computed sort keys) to
avoid per-comparison allocation overhead, so even millions of rows sort
efficiently.

> Sorting is always case-folded, independent of `--case` or
> `--smart-case`.  Those flags control which files **match** — not how
> results are **ordered**.

## 6. Examples and Recipes

### Top 20 Largest Files

```bash
uffs '*' --files-only --sort size --limit 20
```

### Most Recently Modified Logs

```bash
uffs '*.log' --sort modified --limit 50
```

### Directories Ranked by Child Count

```bash
uffs '*' --dirs-only --sort descendants --limit 30
```

### Files Sorted by Extension, Then Name

```bash
uffs '*' --files-only --sort ext,name --limit 100
```

### Oldest Files on C: Drive

```bash
uffs '*' --drive C --files-only --sort created:asc --limit 20
```

### Multi-Drive Sort by Path

```bash
uffs '*.pdf' --drives C,D --sort drive,path
```

### Largest Directory Subtrees

```bash
uffs '*' --dirs-only --sort treesize --limit 20
```

### Most Wasteful Files (Highest Bulkiness)

```bash
uffs '*' --files-only --min-size 1MB --sort bulkiness --limit 20
```

### Hidden Files First, Then Alphabetical

```bash
uffs '*' --sort hidden:desc,name
```

### Compressed Files Grouped, Largest First

```bash
uffs '*' --files-only --sort compressed:desc,size --limit 50
```

### Files With Longest Paths (MAX_PATH Detection)

```bash
uffs '*' --files-only --sort path_length --limit 20
```

### Directories First, Then Files Alphabetically

```bash
uffs '*' --sort directory_flag:desc,name
```

## 7. Quick Reference

```text
SORT SYNTAX
  --sort <COLUMN>                    Single column (smart default direction)
  --sort <COLUMN>:asc               Explicit ascending
  --sort <COLUMN>:desc              Explicit descending
  --sort <COL1>,<COL2>,<COL3>       Multi-tier (comma-separated)
  --sort-desc                        Flip primary column direction

CORE COLUMNS
  name            asc     Filename (case-folded)
  size            desc    Logical file size
  sizeondisk      desc    Allocated size on disk
  modified        desc    Last modification time
  created         desc    Creation time
  accessed        desc    Last access time
  path            asc     Full path (case-folded)
  path_only       asc     Directory portion of path
  drive           asc     Drive letter
  ext             asc     File extension (case-folded)

DERIVED COLUMNS
  type (folder)   asc     Semantic file category (code, picture, …)
  descendants     desc    Direct child count (directories)
  treesize        desc    Subtree logical size total
  treeallocated   desc    Subtree allocated size total
  bulkiness       desc    Waste ratio (100% = perfect)
  namelength      desc    Filename character count
  pathlength      desc    Full path character count

BOOLEAN ATTRIBUTE COLUMNS (19)
  hidden          desc    compressed      desc    directory_flag (directory, dir)  desc
  system          desc    encrypted       desc    virtual         desc
  archive         desc    sparse          desc    pinned          desc
  read_only       desc    reparse         desc    unpinned        desc
  offline         desc    temporary       desc    integrity       desc
  not_indexed     desc    no_scrub        desc    recall_on_open  desc
  recall_on_data_access  desc

NOTES
  • Unknown column names are silently ignored.
  • A name-ascending tiebreaker is appended automatically if name
    is not already in the sort spec.
  • String columns use case-folded comparison (Windows-style).
  • Boolean columns sort true/false groups, then by name within each.
  • See concepts.md for what each metric means.
```

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Search Modes](search-modes.md): glob, literal and regex patterns
- [Output Formats](output-formats.md): CSV, JSON and table output, and choosing columns
