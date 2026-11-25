# Getting Started

This page walks you from zero to productive in five minutes.  By the
end you will have searched your filesystem, filtered results, and
understood what the output means.

## 1. Your First Search

### Windows (live NTFS drives)

```bash
uffs "*.txt"
```

That is it.  UFFS reads the MFT from every NTFS drive, starts a
background daemon, and returns every `.txt` file on the machine.

### macOS / Linux (offline MFT captures)

```bash
uffs "*.txt" --data-dir ~/uffs_data
```

Point `--data-dir` at a directory containing MFT captures organised
in `drive_c/`, `drive_d/`, etc.  See [Cache & Data Sources](cache-and-data.md)
for how to set this up.

> **First search is slow (on the test machine: ~7 s with a warm cache,
> or ~66 s cold).** The daemon is loading the MFT into memory.  Every
> search after that completed in ~200 ms end-to-end on that machine
> (~150 ms daemon-side for 25.9M records).

## 2. Understanding the Output

Each row is one file or directory.  CSV is the format for scripts and
files (it is used when the output is piped or redirected, and with
`--out`); `--format table` gives an aligned table for reading in a
terminal.  Name the columns you want with `--columns`:

```bash
uffs "*.txt" --columns name,size,modified,path
```

```
"Name","Size","Last Written","Path"

"notes.txt",1234,2026-04-01 09:15:32,"C:\Users\me\Documents\notes.txt"
"readme.txt",567,2026-03-28 14:22:01,"C:\Projects\readme.txt"
```

| Column | What it shows |
|--------|--------------|
| **Name** | Filename with extension |
| **Size** | Logical file size in bytes |
| **Last Written** (`modified`) | Last modification timestamp, in local time |
| **Path** | Full path including drive letter |

You can change which columns appear, switch to JSON or table format,
and redirect output to a file.  See [Output Formats](output-formats.md).

## 3. Narrowing Results

Bare searches return a lot of results.  Add filters to focus:

```bash
# Only files (no directory entries)
uffs "*.txt" --files-only

# Modified in the last 7 days
uffs "*.txt" --newer 7d

# Larger than 1 MB
uffs "*.txt" --min-size 1MB

# Combine filters — they stack
uffs "*.txt" --files-only --newer 7d --min-size 1MB

# Sort by size, largest first
uffs "*.txt" --sort -size

# Limit to top 20
uffs "*.txt" --sort -size --limit 20
```

Filters are applied server-side in the daemon — adding more filters
makes searches *faster*, not slower, because less data is returned.

> **Full filter reference:** [Filters](filters.md) — 40+ filters across
> size, date, extension, type, attributes, path, and tree metrics.

## 4. The Daemon

You may have noticed: the first search took several seconds, but the
second one was instant.  That is the daemon.

```
┌─────────┐        IPC socket        ┌─────────────┐
│ uffs CLI ├──────────────────────────┤ uffsd        │
│          │   JSON-RPC over Unix     │  (in-memory  │
└─────────┘   domain socket (Mac)     │   MFT index) │
              named pipe (Windows)    └─────────────┘
```

- The daemon **starts automatically** on your first search.
- It loads the MFT once and holds it in memory.
- Every subsequent search completed in ~200 ms end-to-end on the test
  machine.
- Multiple CLI and TUI sessions share the same daemon.
- It retires automatically after being idle.

Check its status:

```bash
uffs --daemon status
```

## 5. Recipes

These recipes are organised by the workflows people use file search
tools for most often.

### Quick Find: "I know roughly what it is called"

The #1 reason people use file search.

```bash
uffs invoice                                    # Names containing "invoice"
uffs '*.pdf'                                    # All PDFs across every drive
uffs 'dir:node_modules'                         # Directories whose name contains node_modules
uffs --contains report                          # Names containing "report"
uffs '*.docx' --drive C --in-path '*\users\*'   # Word docs on C: under a Users folder
uffs --begins-with IMG --ext pictures           # Photos starting with IMG
```

### Filter by Type / Date / Size

"I know what kind of file it is."

```bash
uffs '*' --ext pictures --min-size 1MB          # Images over 1 MB
uffs '*' --ext executables --sort -size         # Executables ranked by size
uffs '*' --type code --newer 7d                 # Source code modified this week
uffs '*' --type video --min-size 100MB          # Large videos
uffs '*.pdf' --newer 7d --files-only            # Recent PDFs
uffs '*' --newer-accessed 24h --files-only      # Files opened in the last 24 hours
uffs '*' --month jan,feb --ext documents        # Docs from January & February
uffs '*' --type config --in-path '*projects*'   # Config files in project trees
```

### Triage and Cleanup

Storage management — the hidden killer feature.

```bash
# ── Storage hogs ──────────────────────────────────────────
uffs '*' --files-only --sort -size --limit 20   # Top 20 largest files
uffs '*' --files-only --min-size 1GB            # Files over 1 GB
uffs '*' --dirs-only --sort -treesize --limit 20 # Biggest directory subtrees
uffs '*' --files-only --min-bulkiness 500 --sort -bulkiness # Wasteful allocations

# ── Stale content ─────────────────────────────────────────
uffs '*' --ext archives --older 730d            # Archives over 2 years old
uffs '*' --files-only --older 365d --min-size 100MB # Old large files
uffs '*.tmp' --older 30d --sort -size           # Old temp files

# ── Empty / broken structures ─────────────────────────────
uffs '*' --dirs-only --max-descendants 1        # Empty directories (count 1 = itself)
uffs '*' --files-only --max-size 0              # Zero-byte files

# ── Path & name problems ─────────────────────────────────
uffs '*' --files-only --min-path-length 250 --sort -pathlength # MAX_PATH risk
uffs '*' --files-only --min-name-length 100 --sort -namelength # Absurdly long names
```

### Power Search: Hidden / System / Attribute Files

```bash
uffs '*' --attr hidden --files-only             # Hidden files
uffs '*' --attr hidden,encrypted --files-only   # Hidden + encrypted
uffs '*' --attr compressed --sort -size         # NTFS-compressed files by size
uffs '*' --attr system --sort -sizeondisk       # System files by disk usage
uffs '*' --sort hidden:desc,name                # Group hidden files first
```

### Developer / Admin Workflows

```bash
# ── Regex power ───────────────────────────────────────────
uffs '>.*\.log$' --newer 24h                    # Logs from last 24h
uffs '>[0-9]{4}-[0-9]{2}-[0-9]{2}' --ext csv   # Date-stamped CSVs

# ── Project / folder scoping ─────────────────────────────
uffs '*.rs' --in-path '*projects*'              # Rust files in project dirs
uffs package.json --in-path '*node_modules*'    # package.json in node_modules

# ── Multi-drive & output ─────────────────────────────────
uffs '*.exe' --drives C,D,E --sort -size --limit 10
uffs '*.rs' --format json --limit 5             # NDJSON output
uffs '*.dll' --columns name,size,path           # Selective columns
uffs '*.txt' --out results.csv                  # Write to file
```

## Where to Go Next

| You want to… | Read |
|--------------|------|
| Learn every pattern type (glob, regex, literal) | [Search Modes](search-modes.md) |
| See every filter in detail | [Filters](filters.md) |
| Sort by any column | [Sorting](sorting.md) |
| Choose columns, JSON or a table | [Output Formats](output-formats.md) |
| Get a summary of a whole drive instead of a file list | [Aggregation](aggregation.md) |
| Understand why sizes differ from Explorer | [Concepts](concepts.md#1-size-vs-size-on-disk) |
| Manage the daemon | [Daemon](daemon.md) |
| Search offline captures on macOS or Linux | [Cache & Data Sources](cache-and-data.md) |
| Connect AI agents | [MCP Server](mcp.md) |
| See measured performance | [Performance](performance.md) |
| Fix "the daemon will not start" | [Troubleshooting](troubleshooting.md) |
