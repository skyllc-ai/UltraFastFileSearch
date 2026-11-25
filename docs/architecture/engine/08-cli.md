# CLI Reference

## Introduction

The `uffs` CLI is the primary user interface for UFFS. Search is the **default action** — no subcommand needed, matching the UX of ripgrep, fd, and Everything.

---

## Quick Start

```bash
uffs *.txt                    # Find all .txt files on all NTFS drives
uffs c:/pro*                  # Find files starting with "pro" on C:
uffs ">.*\.log$"              # REGEX: find .log files (> prefix)
uffs readme                   # Literal substring match
uffs --ext=rs,toml            # Find by extension
uffs * --files-only --min-size 1048576  # Files > 1MB
```

---

## Search Flags

### Pattern & Scope

| Flag | Description | Example |
|------|-------------|---------|
| `PATTERN` | Search pattern (positional, glob/regex/literal) | `uffs *.rs` |
| `-d, --drive` | Single drive letter | `--drive C` |
| `--drives` | Multiple drives (comma-separated) | `--drives C,D,E` |
| `--ext` | Filter by file extension(s) | `--ext rs,toml` |
| `--exclude` | Exclude files matching pattern | `--exclude backup*` |
| `-i, --index` | Use pre-built index file | `--index C.parquet` |
| `--mft-file` | Use raw MFT file(s) (cross-platform) | `--mft-file C.bin` |

### Filtering

| Flag | Description | Example |
|------|-------------|---------|
| `--files-only` | Show only files (exclude directories) | |
| `--dirs-only` | Show only directories | |
| `--hide-system` | Hide system files (starting with $) | |
| `--min-size` | Minimum file size in bytes | `--min-size 1024` |
| `--max-size` | Maximum file size in bytes | `--max-size 1048576` |
| `--attr` | Filter by NTFS attributes | `--attr hidden,!system` |
| `--newer` | Modified within duration/after date | `--newer 7d` |
| `--older` | Modified before duration/date | `--older 2026-01-01` |
| `--newer-created` | Created within duration/after date | `--newer-created 24h` |
| `--older-created` | Created before duration/date | |
| `--newer-accessed` | Accessed within duration/after date | |
| `--older-accessed` | Accessed before duration/date | |
| `-n, --limit` | Maximum result count (0 = unlimited) | `-n 100` |

### Matching Options

| Flag | Description | Default |
|------|-------------|---------|
| `--case` | Case-sensitive matching | `false` |
| `--smart-case` | Auto case-sensitive if pattern has uppercase | `false` |
| `--word` | Whole word matching (wraps in `\b...\b`) | `false` |

### Output Formatting

| Flag | Description | Default |
|------|-------------|---------|
| `-f, --format` | Output format: csv, table, json | `csv` |
| `--columns` | Columns to output (comma-separated or "all") | `all` |
| `--sep` | Column separator (or TAB, SPACE, etc.) | `,` |
| `--quotes` | Quote character for strings | `"` |
| `--header` | Include header row | `true` |
| `--pos` | Boolean true representation | `1` |
| `--neg` | Boolean false representation | `0` |
| `--out` | Output destination: console or filename | `console` |
| `--sort` | Sort by column(s): size, modified, name, etc. | (none) |
| `--sort-desc` | Reverse sort order | `false` |

### Performance & Debugging

| Flag | Description |
|------|-------------|
| `--profile` | Show detailed timing breakdown |
| `--benchmark` | Skip output, measure only MFT reading |
| `--no-cache` | Bypass cache, read MFT fresh |
| `--tz-offset` | Override timezone for timestamp display |
| `-v, --verbose` | Enable info-level terminal logging |

---

## Commands

| Command | Description |
|---------|-------------|
| `uffs --stats` | Display index statistics |

> **Historical note.** The standalone `uffs index` / `uffs info` /
> `uffs save-raw` / `uffs load-raw` subcommands were removed when the CLI
> became a thin daemon client. Their capabilities live on elsewhere:
> index building/loading is **daemon-managed** (`uffs --daemon start
> --data-dir <dir>` / `--mft-file <file>`), and low-level NTFS volume/record
> inspection + raw-MFT capture moved to the separate `uffs-mft` tool
> (run `uffs-mft --help`).

### `uffs --stats`

```bash
uffs --stats                       # Live overview via the daemon
uffs --stats --data-dir ~/uffs_data
```

`--stats` takes `--data-dir` and `--mft-file` and nothing else.

---

## Pattern Syntax

### Glob Patterns (Default)

| Pattern | Matches |
|---------|---------|
| `*` | Everything |
| `*.txt` | Files ending in .txt |
| `foo*` | Files starting with foo |
| `*bar*` | Files containing bar |
| `foo*bar` | Files starting with foo and ending with bar |
| `*.rs\|*.toml` | OR: .rs or .toml files |
| `c:/Users/*.txt` | Path pattern: .txt files under C:\Users |

### Regex Patterns (> Prefix)

Prefix with `>` to use regex:

```bash
uffs ">.*\.txt$"              # Ends with .txt
uffs ">^test_.*\.rs$"         # Starts with test_, ends with .rs
uffs ">C:\\Users\\.*\.log"    # Path regex
```

### Literal Patterns

Patterns without wildcards are treated as **substring matches** (like Everything):

```bash
uffs readme                   # Finds any file containing "readme"
uffs "hello world"            # Finds files containing "hello world"
```

---

## Attribute Filter Syntax

The `--attr` flag accepts comma-separated attribute requirements. Prefix `!` to require absence:

```bash
uffs * --attr hidden              # Only hidden files
uffs * --attr compressed,!system  # Compressed but not system
uffs * --attr !hidden,!system     # Neither hidden nor system
```

Available attributes: `hidden`, `system`, `archive`, `readonly`, `compressed`, `encrypted`, `sparse`, `reparse`, `offline`, `notindexed`, `temporary`, `virtual`, `pinned`, `unpinned`, `integrity`, `noscrub`, `directory`

---

## Date Filter Syntax

The `--newer` and `--older` flags accept durations or ISO dates:

```bash
# Duration formats
uffs * --newer 7d              # Modified in last 7 days
uffs * --newer 24h             # Modified in last 24 hours
uffs * --newer 30m             # Modified in last 30 minutes

# Date formats
uffs * --newer 2026-01-15              # After Jan 15
uffs * --older 2026-01-15T10:30:00     # Before Jan 15 10:30am
```

---

## Sort Syntax

Multi-tier sorting with comma-separated columns:

```bash
uffs * --sort size                    # Sort by size
uffs * --sort size --sort-desc        # Largest first
uffs * --sort modified,name           # By modified date, then name
uffs * --sort ext,size                # By extension, then size
```

Available sort columns: `size`, `sizeondisk`, `modified`, `created`, `accessed`, `name`, `ext`, `descendants`, `hidden`, `system`, `archive`, `readonly`, `compressed`, `encrypted`, `directory`

---

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `RUST_LOG` | Terminal log level | `error` (or `info` with `-v`) |
| `RUST_LOG_FILE` | File log level | `info` |
| `UFFS_LOG_DIR` | Log file directory | `~/bin/uffs/logs` |
| `UFFS_REBUILD_CHILDREN_ALWAYS` | Force child-list rebuild (validation) | unset |
| `UFFS_SKIP_ORPHANS` | Exclude orphan records from tree | unset |

---

## Global Allocator

The CLI uses `mimalloc` as the global allocator for improved performance with many small allocations:

```rust
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
```

---

*Document Version: 2.0*
*Last Updated: 2026-04-12*
*UFFS Version: 0.4.106*
