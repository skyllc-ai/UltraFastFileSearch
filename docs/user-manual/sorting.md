# Sorting

UFFS supports single- and multi-tier sorting of search results.  Sort
specifications are applied after all filters, so you are always sorting
the final result set.

## 1. Basic Usage

```bash
# Sort by file size, largest first (a leading - means descending)
uffs '*.pdf' --sort -size

# Sort by name, A → Z
uffs '*.txt' --sort name

# Sort by modification date, newest first
uffs '*.log' --sort -modified
```

The first sort column is **ascending** unless you ask for descending
(§3). Without `--sort`, results come sorted by modification time, newest
first.

## 2. Sort Columns

> **New to UFFS?** See [Concepts](concepts.md) for what each metric
> (Size on Disk, Tree Size, Bulkiness, etc.) actually measures.

### Core Fields

| Column Name | Aliases | Description |
|-------------|---------|-------------|
| `name` | — | Filename (case-folded) |
| `size` | — | Logical file size in bytes |
| `size_on_disk` | `sizeondisk`, `allocated` | Allocated size on disk ([why it differs](concepts.md#1-size-vs-size-on-disk)) |
| `modified` | `date`, `written` | Last modification timestamp |
| `created` | — | Creation timestamp |
| `accessed` | — | Last access timestamp |
| `path` | — | Full path (case-folded) |
| `path_only` | `pathonly` | Directory portion of path |
| `drive` | `drv` | Drive letter |
| `ext` | `extension` | File extension (case-folded) |

### Derived Fields

| Column Name | Aliases | Description |
|-------------|---------|-------------|
| `type` | `folder` | Semantic file category (`code`, `picture`, …) |
| `descendants` | `decendents` | Entries in the whole subtree (directories) |
| `tree_size` | `treesize` | Sum of logical sizes in subtree ([concept](concepts.md#2-tree-size-and-tree-allocated)) |
| `tree_allocated` | `treeallocated` | Sum of allocated sizes in subtree |
| `bulkiness` | — | Waste ratio % ([concept](concepts.md#3-bulkiness-waste-ratio)) |
| `name_length` | `namelength`, `namelen`, `name_len` | Character count of filename |
| `path_length` | `pathlength`, `pathlen`, `path_len` | Character count of full path |

### Boolean Attribute Fields

All 19 NTFS attribute flags are sortable.  They group `true` before
`false` (descending) or `false` before `true` (ascending), with a name
tiebreaker within each group. That tiebreaker follows the direction: with
`hidden:desc` the names inside each group run Z → A.

| Column Name | Aliases | What it groups |
|-------------|---------|---------------|
| `hidden` | `h` | Hidden files first |
| `system` | `s` | System files first |
| `archive` | `a` | Modified-since-backup first |
| `read_only` | `readonly`, `r` | Read-only files first |
| `compressed` | — | NTFS-compressed first |
| `encrypted` | — | EFS-encrypted first |
| `sparse` | — | Sparse files first |
| `reparse` | — | Symlinks/junctions first |
| `offline` | `o` | Offline/tiered first |
| `not_indexed` | `notindexed` | Not-indexed first |
| `temporary` | `temp` | Temp files first |
| `directory_flag` | `directory`, `dir`, `directoryflag` | Directories first |
| `virtual` | — | Virtual files first |
| `pinned` | — | Pinned files first |
| `unpinned` | — | Unpinned files first |
| `integrity` | — | Integrity-stream first |
| `no_scrub` | `noscrub` | No-scrub first |
| `recall_on_open` | `recallonopen` | Recall-on-open first |
| `recall_on_data_access` | `recallondataaccess` | Recall-on-data-access first |

Two more columns sort the forensic name checks: `malformed` and
`malformed_path` (see [Filters §14a](filters.md#14a-malformed-name-filters-forensic)).

### Default Directions

- The **first** column is ascending unless you mark it descending.
- A **later** column (a tiebreaker) without a direction is descending for
  `size`, `size_on_disk`, `created`, `modified`, `accessed`,
  `descendants`, `tree_size` and `tree_allocated`, and ascending for every
  other column.

## 3. Explicit Direction

Mark a column descending with a leading `-` (`-size`) or a `:desc` suffix
(`size:desc`), and ascending with `:asc`. `:descending` and `:ascending` work
too.

```bash
# Size ascending (smallest first)
uffs '*.dll' --sort size:asc

# Name descending (Z → A)
uffs '*.txt' --sort name:desc

# Modified ascending (oldest first)
uffs '*.log' --sort modified:asc
```

### `--sort-desc` Flag

The `--sort-desc` flag makes the **primary** sort column descending:

```bash
# Largest first (the same as --sort -size)
uffs '*.iso' --sort size --sort-desc
```

> When using `:asc` / `:desc` suffixes, the `--sort-desc` flag is
> ignored for columns that already have an explicit direction.

## 4. Multi-Tier Sorting

Separate multiple sort columns with commas.  The first column is the
primary sort; subsequent columns break ties.

```bash
# Primary: size descending, tiebreaker: name ascending
uffs '*.dll' --sort -size,name

# Primary: modified descending, then size descending, then name ascending
uffs '*.log' --sort -modified,size,name

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
uffs '*.exe' --sort -size

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
uffs '*' --files-only --sort -size --limit 20
```

### Most Recently Modified Logs

```bash
uffs '*.log' --sort -modified --limit 50
```

### Directories Ranked by Descendants

```bash
uffs '*' --dirs-only --sort -descendants --limit 30
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
uffs '*' --dirs-only --sort -treesize --limit 20
```

### Most Wasteful Files (Highest Bulkiness)

```bash
uffs '*' --files-only --min-size 1MB --sort -bulkiness --limit 20
```

### Hidden Files First

```bash
uffs '*' --sort hidden:desc
```

Within each group the names run Z → A, because the name tiebreaker of a
boolean column follows its direction.

### Compressed Files Grouped, Largest First

```bash
uffs '*' --files-only --sort compressed:desc,size --limit 50
```

### Files With Longest Paths (MAX_PATH Detection)

```bash
uffs '*' --files-only --sort -path_length --limit 20
```

### Directories First

```bash
uffs '*' --sort directory_flag:desc
```

## 7. Quick Reference

```text
SORT SYNTAX
  --sort <COLUMN>                    Single column, ascending
  --sort -<COLUMN>                   Descending
  --sort <COLUMN>:asc|:desc          Explicit direction
  --sort <COL1>,<COL2>,<COL3>        Multi-tier (comma-separated)
  --sort-desc                        Make the primary column descending

  Later tiers without a direction: descending for size, size_on_disk,
  created, modified, accessed, descendants, tree_size, tree_allocated;
  ascending for everything else.

CORE COLUMNS
  name                    Filename (case-folded)
  size                    Logical file size
  size_on_disk            Allocated size on disk
  modified                Last modification time
  created                 Creation time
  accessed                Last access time
  path                    Full path (case-folded)
  path_only               Directory portion of path
  drive                   Drive letter
  ext                     File extension (case-folded)

DERIVED COLUMNS
  type (folder)           Semantic file category (code, picture, …)
  descendants             Entries in the whole subtree (directories)
  treesize                Subtree logical size total
  treeallocated           Subtree allocated size total
  bulkiness               Waste ratio (100% = perfect)
  namelength              Filename character count
  pathlength              Full path character count

BOOLEAN ATTRIBUTE COLUMNS (19)
  hidden                  compressed              directory_flag (directory, dir)
  system                  encrypted               virtual
  archive                 sparse                  pinned
  read_only               reparse                 unpinned
  offline                 temporary               integrity
  not_indexed             no_scrub                recall_on_open
  recall_on_data_access

NOTES
  • Unknown column names are silently ignored.
  • A name-ascending tiebreaker is appended automatically if name
    is not already in the sort spec.
  • String columns use case-folded comparison (Windows-style).
  • Boolean columns sort true/false groups, then by name within each
    (the name order follows the column's direction).
  • See concepts.md for what each metric means.
```

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Search Modes](search-modes.md): glob, literal and regex patterns
- [Output Formats](output-formats.md): CSV, JSON and table output, and choosing columns
