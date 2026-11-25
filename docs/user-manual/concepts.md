# Concepts

UFFS exposes several file metrics that go beyond the simple "name and
size" you see in Windows Explorer.  This page explains what each metric
means, why it exists, and when to use it.

## 1. Size vs Size on Disk

Every file has two sizes:

| Metric | What it measures | CLI flag |
|--------|------------------|----------|
| **Size** (logical) | The number of meaningful bytes in the file | `--min-size`, `--max-size` |
| **Size on Disk** (allocated) | The actual bytes consumed on the physical drive | `--min-size-on-disk`, `--max-size-on-disk` |

### Why are they different?

NTFS allocates space in fixed-size **clusters** (typically 4 KB).  A 100-byte
file still occupies one full 4 KB cluster on disk — so its Size is 100
but its Size on Disk is 4 096.

```
┌─────────────────────────────────────────────────┐
│  File: notes.txt                                │
│  Size (logical):      100 bytes                 │
│  Size on Disk:      4 096 bytes (1 cluster)     │
│  Wasted space:      3 996 bytes                 │
└─────────────────────────────────────────────────┘
```

Three common situations where Size ≠ Size on Disk:

| Situation | Size | Size on Disk | Ratio |
|-----------|------|-------------|-------|
| **Small file** (cluster padding) | 100 B | 4 096 B | 40× |
| **NTFS-compressed file** | 10 MB | 3 MB | 0.3× |
| **Sparse file** (VM disk, database) | 100 GB | 2 GB | 0.02× |

- Small files waste space due to cluster alignment.
- Compressed files use *less* disk than their logical size.
- Sparse files reserve huge logical ranges but only allocate clusters
  that actually contain data.

### When to use each

- **"How much data is in this file?"** → Size (logical).
- **"How much disk space does this file consume?"** → Size on Disk.
- **"Find files wasting disk space"** → compare them (see Bulkiness below).

## 2. Tree Size and Tree Allocated

These are **directory-only** metrics.  They aggregate the sizes of every
file in the directory's entire subtree (all descendants, recursively).

| Metric | What it sums | CLI flag |
|--------|-------------|----------|
| **Tree Size** | Logical sizes of all files below this directory | `--min-treesize`, `--max-treesize` |
| **Tree Allocated** | Allocated sizes of all files below this directory | `--min-tree-allocated`, `--max-tree-allocated` |

```
Projects/                     ← Tree Size = 15 MB (sum of all below)
├── src/
│   ├── main.rs       1 MB
│   └── lib.rs        2 MB
├── target/
│   └── debug/
│       └── app.exe  10 MB
└── README.md         2 MB
```

In this example, the `Projects/` directory has:
- **Size** = 0 (directories themselves have zero logical size in NTFS)
- **Tree Size** = 15 MB (1 + 2 + 10 + 2)
- **Descendants** = 8: the 7 entries below it (3 subdirectories and 4
  files) plus 1 for `Projects/` itself

### When to use

- **"Which folders use the most disk space?"** →
  `--dirs-only --sort -treesize`
- **"Find folders over 10 GB"** →
  `--dirs-only --min-treesize 10GB`
- **"Where is disk space actually allocated?"** →
  `--dirs-only --sort -tree_allocated`

## 3. Bulkiness (Waste Ratio)

Bulkiness measures how much disk space a file or directory uses
**relative to its logical size**.  It is expressed as a percentage:

```
Bulkiness = (Size on Disk ÷ Size) × 100
```

| Bulkiness | Meaning | Example |
|-----------|---------|---------|
| **100%** | Perfectly packed — no wasted space | Large file filling exact clusters |
| **200%** | Using 2× the expected space | File using double its logical size |
| **500%** | Using 5× the expected space | Small file in a large cluster |
| **50%** | Compressed to half its logical size | NTFS-compressed file |

```
┌──────────────────────────────────────────────────┐
│  Perfectly packed (bulkiness = 100%):            │
│  ████████████████████████████████  ← all used    │
│                                                  │
│  Wasteful (bulkiness = 4000%):                   │
│  ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░  ← mostly empty │
│                                                  │
│  Compressed (bulkiness = 30%):                   │
│  ████████████  ← compressed smaller than logical │
└──────────────────────────────────────────────────┘
```

For **directories**, bulkiness uses the tree metrics:
`Tree Allocated ÷ Tree Size × 100`.

### When to use

- **"Find files wasting the most space"** →
  `--min-bulkiness 500 --files-only --sort -bulkiness`
- **"Find well-compressed files"** →
  `--max-bulkiness 50 --attr compressed`
- **"Which folders have the worst packing efficiency?"** →
  `--dirs-only --min-bulkiness 200 --sort -bulkiness`

## 4. Descendants

Descendants counts **every entry in a directory's whole subtree**, at
every depth, plus 1 for the directory itself.  Files, subdirectories and
the files' named data streams (see [§10](#10-alternate-data-streams-ads))
all count, so a file with one extra stream counts 2.

| Descendants | Meaning |
|-------------|---------|
| **0** | A file (files have no descendants) |
| **1** | Empty directory (only the directory itself) |
| **2–10** | Small directory tree |
| **1 000+** | Large directory tree |

### When to use

- **"Find empty directories for cleanup"** →
  `--dirs-only --max-descendants 1`
- **"Find bloated directories"** →
  `--dirs-only --min-descendants 1000 --sort -descendants`

## 5. File Type (Semantic Category)

UFFS maps file extensions to **24 human-readable categories** so you can
filter and sort by *what a file is*, not just its extension.

| Category | What it covers |
|----------|---------------|
| `code` | Source code (`.rs`, `.py`, `.js`, `.java`, `.c`, `.go`, …) |
| `document` | Office docs, PDFs, text (`.pdf`, `.docx`, `.txt`, `.md`, …) |
| `picture` | Images (`.jpg`, `.png`, `.gif`, `.svg`, `.heic`, …) |
| `video` | Video files (`.mp4`, `.mkv`, `.mov`, `.avi`, …) |
| `audio` | Sound files (`.mp3`, `.flac`, `.wav`, `.aac`, …) |
| `archive` | Compressed archives (`.zip`, `.rar`, `.7z`, `.tar`, `.iso`, …) |
| `executable` | Runnable programs (`.exe`, `.msi`, `.bat`, `.cmd`, …) |
| `database` | Data stores (`.db`, `.sqlite`, `.sql`, `.mdf`, …) |
| `config` | Configuration (`.json`, `.yaml`, `.toml`, `.xml`, `.ini`, …) |
| `disk` | Disk images (`.vmdk`, `.vhd`, `.img`, `.dmg`, …; `.iso` counts as `archive`) |
| `system` | OS internals (`.sys`, `.dll`, `.drv`, `.ocx`, …) |
| `web` | Web files (`.html`, `.css`, `.jsx`, `.vue`, `.wasm`, …) |
| `script` | Shell scripts (`.sh`, `.bash`, `.lua`, `.pl`, …) |
| `directory` | NTFS directory entries (no extension needed) |
| `file` | Files with no extension |
| `other` | Extensions not mapped to any category |

The full list of all 24 categories and their extensions is in
[Filters §9](filters.md).

### When to use

- **"Find all source code"** → `--type code`
- **"Find all images"** → `--type picture`
- **"Sort by file type, then by size"** → `--sort type,size`

## 6. NTFS Attributes (File Flags)

Every file and directory on an NTFS volume carries a set of **boolean
flags** that describe its properties.  These are set by Windows and
applications — they are metadata *about* the file, not the file's content.

| Attribute | What it means |
|-----------|--------------|
| **Hidden** | File is hidden from default Explorer views |
| **System** | File is part of the operating system |
| **Read-only** | File cannot be modified without removing the flag |
| **Archive** | File has been modified since the last backup |
| **Compressed** | File content is NTFS-compressed (transparent to apps) |
| **Encrypted** | File content is EFS-encrypted (only accessible by the owning user) |
| **Sparse** | File has large unallocated gaps (common for VM disks) |
| **Reparse** | File is a symbolic link, junction, or mount point |
| **Temporary** | File is expected to be short-lived |
| **Offline** | File content has been moved to slow/archival storage |
| **Directory** | Entry is a directory, not a file |

> **Hidden + System** together mark OS-internal files that Windows hides
> even with "Show hidden files" enabled.  Use `--attr hidden,system` to
> find them, or `--attr !hidden,!system` to exclude them.

### When to use

- **"Find hidden files"** → `--attr hidden`
- **"Exclude system files"** → `--attr !system`
- **"Find NTFS-compressed files"** → `--attr compressed`
- **"Find symlinks and junctions"** → `--attr reparse`
- **"Sort by hidden status"** → `--sort hidden:desc` (hidden first)

## 7. Timestamps

NTFS stores three timestamps per file, all recorded to 100-nanosecond
precision:

| Timestamp | What triggers an update |
|-----------|----------------------|
| **Modified** | File content was written (most commonly used) |
| **Created** | File was first created on this volume |
| **Accessed** | File was read or opened (often disabled for performance) |

### Common surprises

- **Copied files** keep their original Modified time but get a *new*
  Created time.  A file can be "created" in 2026 but "modified" in 2020 —
  this is normal and means it was copied.
- **Accessed** timestamps are often stale.  Windows disables access-time
  updates by default since Vista (`NtfsDisableLastAccessUpdate`).  Don't
  rely on Accessed for precise auditing.
- **Moved files** (within the same volume) keep all three timestamps
  unchanged — the MFT record is simply re-parented.

### When to use

- **"Find recently changed files"** → `--newer 7d` (Modified)
- **"Find newly downloaded/installed files"** → `--newer-created 7d`
- **"Find files not touched in years"** → `--older 730d`

## 8. Name Length and Path Length

| Metric | What it counts |
|--------|---------------|
| **Name Length** | Characters in the filename only (e.g. `report.pdf` = 10) |
| **Path Length** | Characters in the full path (e.g. `C:\Users\me\report.pdf` = 22) |

### Why it matters

Windows has a historic **MAX_PATH** limit of **260 characters**.  While
modern Windows supports long paths (32 767 chars), many older apps,
scripts, and backup tools break on paths longer than 260.

### When to use

- **"Find paths approaching MAX_PATH"** →
  `--min-path-length 240`
- **"Find files with extremely long names"** →
  `--min-name-length 100`
- **"Find short 8.3-style filenames"** →
  `--max-name-length 12`

## 9. Month-of-Year

The month filter lets you select files by the **calendar month** of their
last modification, regardless of year.  This is useful for seasonal or
periodic analysis.

```bash
# Tax documents modified in any January or April
uffs '*.pdf' --month jan,apr

# Files modified in Q4 (October, November, December) of any year
uffs '*' --month Q4
```

> The month is extracted from the Modified timestamp.  A file modified on
> 2023-01-15 and another modified on 2026-01-03 both match `--month jan`.

## 10. Alternate Data Streams (ADS)

NTFS allows files to have multiple named data streams.  The default
(unnamed) stream is what you see as the file's content.  Named streams
are hidden from Explorer and most tools.

Common uses:

- **Zone.Identifier** — attached by browsers to mark downloaded files
  ("This file came from the internet")
- **Thumbnails** — some applications store preview data in ADS
- **Custom metadata** — applications may store arbitrary data in named
  streams

### In UFFS

UFFS indexes named streams too.  Each one appears in the results as its
own entry, named `file:stream` (for example
`setup.exe:Zone.Identifier`).  Use `--hide-ads` to leave them out.

```bash
# Find downloaded-file markers
uffs '*Zone.Identifier'

# Search without ADS entries
uffs '*.exe' --hide-ads
```

## 11. The MFT: How UFFS Reads Your Files

The Master File Table is a flat database at the start of every NTFS
volume.  Each 1 KB record contains a file's metadata — name, parent
directory, size, timestamps, and attributes.

```
Traditional file search              UFFS
──────────────────────────           ──────────────────────────
Walk directory tree (FindFirstFile)   Read MFT sequentially
  → 1 API call per file               → 1 sequential I/O
  → millions of calls                  → thousands of records/ms
  → random I/O to disk                → linear scan
  → 30-60 seconds                     → 5-8 s NVMe, ~200 ms once hot
```

The MFT contains everything UFFS needs except two things:

1. **Full paths** — the MFT stores only `name` + `parent record number`.
   UFFS reconstructs paths by walking the parent chain.
2. **File contents** — the MFT records metadata, not data bytes.
   UFFS cannot search inside files.

> **Deep dive:** [Cache & Data Sources](cache-and-data.md)

## Quick Comparison Table

| Metric | Applies to | What it answers |
|--------|-----------|----------------|
| Size | Files | "How big is this file's content?" |
| Size on Disk | Files | "How much drive space does it consume?" |
| Tree Size | Directories | "How much content is in this folder tree?" |
| Tree Allocated | Directories | "How much drive space does this folder tree consume?" |
| Bulkiness | Both | "How efficiently is space used? (waste ratio)" |
| Descendants | Directories | "How many entries are in this folder tree?" |
| Type | Both | "What kind of file is this?" |
| Attributes | Both | "What flags does NTFS set on this?" |
| Name Length | Both | "How long is the filename?" |
| Path Length | Both | "How long is the full path?" |

## See also

- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Sorting](sorting.md): sort columns and multi-tier sorts
- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Glossary](glossary.md): the terms the chapters use
