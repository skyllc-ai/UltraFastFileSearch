# Filters

UFFS provides a rich set of filters that narrow search results **after**
pattern matching.  Filters are applied server-side inside the daemon — only
matching rows are returned to the CLI, so adding filters never makes a
search slower; it makes it faster by reducing output.

## 1. Scope Filters

Scope filters control whether files, directories, or both appear in
results.

| Flag | Effect |
|------|--------|
| `--files-only` | Show only files (exclude directories) |
| `--dirs-only` | Show only directories (exclude files) |
| `--hide-system` | Hide the NTFS metafiles (`$MFT`, `$Bitmap`, `$LogFile`, …); other names that start with `$` stay |
| `--hide-ads` | Hide NTFS Alternate Data Stream entries |

```bash
# All PDF files — no directory entries
uffs '*.pdf' --files-only

# All directories whose name contains "backup"
uffs backup --dirs-only

# Everything, but suppress $MFT, $Bitmap, $LogFile, etc.
uffs '*' --hide-system
```

> `--files-only` and `--dirs-only` are mutually exclusive in practice —
> combining them would return zero results.

## 2. Size Filters

Size filters work on the file's **logical size**.  Values accept
human-readable suffixes or plain byte counts.

| Flag | Meaning |
|------|---------|
| `--min-size <SIZE>` | Only files ≥ this size |
| `--max-size <SIZE>` | Only files ≤ this size |
| `--exact-size <SIZE>` | Exactly this size (shorthand for `--min-size N --max-size N`) |

### Size Suffixes

| Suffix | Multiplier | Example |
|--------|-----------|---------|
| *(none)* | 1 (bytes) | `1024` → 1 024 bytes |
| `B` | 1 | `512B` → 512 bytes |
| `KB` | 1 024 | `100KB` → 102 400 bytes |
| `MB` | 1 048 576 | `10MB` → 10 485 760 bytes |
| `GB` | 1 073 741 824 | `1GB` → 1 073 741 824 bytes |
| `TB` | 1 099 511 627 776 | `2TB` → 2 199 023 255 552 bytes |

Suffixes are **case-insensitive** (`kb`, `KB`, `Kb` all work).

### Examples

```bash
# Large files: at least 100 MB
uffs '*' --files-only --min-size 100MB

# Tiny files: at most 1 KB
uffs '*' --files-only --max-size 1KB

# Size range: 1 MB to 10 MB
uffs '*.pdf' --min-size 1MB --max-size 10MB

# Combine with sort to find the biggest PDFs
uffs '*.pdf' --min-size 1MB --sort size --sort-desc --limit 20

# Raw bytes still work
uffs '*.log' --min-size 4096
```

### Best Practice

- Size filters are most useful with `--files-only` — directories have
  a size of 0 in the MFT.
- Combine `--min-size` with `--sort -size` to build a "top largest files"
  workflow.

## 3. Date / Time Filters

UFFS can filter on three NTFS timestamps: **modified**, **created**, and
**accessed**.  Each timestamp has a **newer** (after) and **older** (before)
bound.

| Flag | Timestamp | Direction |
|------|-----------|-----------|
| `--newer <SPEC>` | Modified | Files modified **within** / **after** |
| `--older <SPEC>` | Modified | Files modified **before** |
| `--newer-created <SPEC>` | Created | Files created **within** / **after** |
| `--older-created <SPEC>` | Created | Files created **before** |
| `--newer-accessed <SPEC>` | Accessed | Files accessed **within** / **after** |
| `--older-accessed <SPEC>` | Accessed | Files accessed **before** |
| `--between <START,END>` | Modified | Time range shorthand (equivalent to `--newer START --older END`) |

### Time Spec Formats

You can specify time bounds using **relative durations** or **absolute
dates**:

#### Duration Suffixes

| Suffix | Meaning | Example | Interpretation |
|--------|---------|---------|----------------|
| `s` | Seconds | `90s` | Last 90 seconds |
| `m` | Minutes | `30m` | Last 30 minutes |
| `h` | Hours | `24h` | Last 24 hours |
| `d` | Days | `7d` | Last 7 days |
| `w` | Weeks | `2w` | Last 2 weeks (14 days) |

#### ISO Date

Use `YYYY-MM-DD` format for absolute dates.  A date means **midnight UTC**
at the start of that day (not midnight in your local time zone):

```bash
--newer 2026-01-15       # Modified on or after 15 January 2026, 00:00 UTC
--older 2025-06-01       # Modified before 1 June 2025, 00:00 UTC
```

#### Named Time Specs

Days start at **midnight UTC**, and weeks start on Monday.  A named range
has a start and an end: `--newer NAME` uses its start, `--older NAME` uses
its end.  So `--newer last_week` alone means "since the start of last
week"; give the same name to both to get exactly that range:
`--newer last_week --older last_week`.

| Name | Start | End |
|------|-------|-----|
| `today` | Midnight today | Midnight today |
| `yesterday` | Midnight yesterday | Midnight today |
| `this_week` | Monday of this week | (same) |
| `last_week` | Monday of last week | Monday of this week |
| `next_week` | Monday of next week | The Monday after |
| `this_month` | 1st of this month | (same) |
| `last_month` | 1st of last month | 1st of this month |
| `next_month` | 1st of next month | 1st of the month after |
| `this_year`, `ytd` | 1 January this year | (same) |
| `last_year` | 1 January last year | 1 January this year (the previous calendar year) |
| `tomorrow`, `next_day` | Midnight tomorrow | Midnight the day after |
| `last_7d` | 7 days ago (from now) | (same) |
| `last_30d` | 30 days ago | (same) |
| `last_90d` | 90 days ago | (same) |
| `last_365d` | 365 days ago | (same) |

Every name with an underscore also works without it (`lastweek`,
`thismonth`, `last30d`, `nextday`, …), and names are not case-sensitive.
For "the last 365 days" use `last_365d` (or `365d`), not `last_year`.

#### `--between` Shorthand

```bash
# Equivalent to --newer 2026-01-01 --older 2026-03-31
uffs '*.pdf' --between 2026-01-01,2026-03-31

# Equivalent to --newer 30d --older 7d
uffs '*.log' --between 30d,7d
```

### Examples

```bash
# Files modified in the last 7 days
uffs '*.log' --newer 7d

# Files NOT modified in over a year
uffs '*.doc' --older 365d

# Files created in the last month
uffs '*' --newer-created 30d --files-only

# Files modified between two dates (combine newer + older)
uffs '*' --newer 2026-01-01 --older 2026-03-31

# Recently accessed executables
uffs '*.exe' --newer-accessed 1d

# Old archives untouched for 2+ years
uffs '*.zip' --older 730d --files-only
```

### Best Practice

- Duration specs (`7d`, `24h`) are the most common and intuitive for
  everyday use.
- Combine `--newer` and `--older` to define a time window.
- The `--newer-created` filter is useful for finding newly downloaded or
  installed files that may have old modification dates.
- All times are resolved relative to **now** at query execution time.

## 4. NTFS Attribute Filters

The `--attr` flag filters by NTFS file-system attributes.  You can
**require** attributes (they must be set) or **exclude** them (they must
not be set) by prefixing with `!`.

### Syntax

```bash
--attr hidden              # Must have Hidden attribute
--attr !hidden             # Must NOT have Hidden attribute
--attr hidden,system       # Must have BOTH Hidden AND System
--attr !system,!hidden     # Must have NEITHER System NOR Hidden
--attr compressed,!hidden  # Must be Compressed AND NOT Hidden
```

### Available Attributes

| Name | Aliases | Hex Bit | Description |
|------|---------|---------|-------------|
| `readonly` | `read-only`, `r` | `0x0001` | Read-only file |
| `hidden` | `h` | `0x0002` | Hidden file |
| `system` | `s` | `0x0004` | System file |
| `directory` | `dir`, `d` | `0x0010` | Directory entry |
| `archive` | `a` | `0x0020` | Archive (modified since backup) |
| `device` | — | `0x0040` | Device (reserved by Windows) |
| `normal` | — | `0x0080` | Normal (no other attribute set) |
| `temporary` | `temp`, `t` | `0x0100` | Temporary file |
| `sparse` | — | `0x0200` | Sparse file |
| `reparse` | — | `0x0400` | Reparse point (symlink, junction) |
| `compressed` | `c` | `0x0800` | NTFS-compressed |
| `offline` | `o` | `0x1000` | Offline / tiered storage |
| `notindexed` | `notcontent`, `n` | `0x2000` | Not indexed by content indexer |
| `encrypted` | `e` | `0x4000` | EFS-encrypted |
| `integrity` | `i` | `0x8000` | Integrity stream (ReFS) |
| `virtual` | `v` | `0x10000` | Virtual file |
| `noscrub` | `no_scrub_data`, `x` | `0x20000` | No scrub data |
| `pinned` | `p` | `0x80000` | Pinned to local storage |
| `unpinned` | `u` | `0x100000` | Not pinned |

Names are not case-sensitive.

### Presets

| Preset | Also Accepted | Same as |
|--------|---------------|---------|
| `system-files` | `system_files`, `sysfiles` | `hidden,system` |
| `user-files` | `user_files`, `userfiles` | `!hidden,!system` |
| `compressed-encrypted` | `compressed_encrypted`, `compenc` | `compressed,encrypted` |

Presets mix with other names: `--attr user-files,readonly`.

### Examples

```bash
# Find all hidden files
uffs '*' --attr hidden --files-only

# Find encrypted files
uffs '*' --attr encrypted --files-only

# Find compressed but not hidden files
uffs '*' --attr compressed,!hidden --files-only

# Find reparse points (symlinks, junctions)
uffs '*' --attr reparse

# Find sparse files (often VM disks, database files)
uffs '*' --attr sparse --files-only
```

### Best Practice

- Use short aliases for quick filtering: `--attr h` instead of
  `--attr hidden`.
- Combine `--attr !hidden,!system` with `--hide-system` for the cleanest
  "user files only" view.
- The `archive` attribute is useful for backup workflows — it marks files
  modified since the last backup.

## 5. Descendant Filters

Descendant filters operate on directories.  A directory's descendant
count is the number of entries in its **whole subtree** (files and
directories at every depth, with each alternate data stream counted as
an entry), **plus 1 for the directory itself**.  An empty directory
therefore has 1; a file has 0.

| Flag | Meaning |
|------|---------|
| `--min-descendants <N>` | Descendant count of at least N |
| `--max-descendants <N>` | Descendant count of at most N |
| `--exact-descendants <N>` | Exactly N (shorthand for `--min-descendants N --max-descendants N`) |

### Examples

```bash
# Find empty directories (the count is 1: the directory itself)
uffs '*' --dirs-only --max-descendants 1

# Find large directory trees with 1000+ entries
uffs '*' --dirs-only --min-descendants 1000

# Empty directories, sorted by path
uffs '*' --dirs-only --exact-descendants 1 --sort path
```

### Best Practice

- Empty-directory detection (`--dirs-only --max-descendants 1`) is one of
  the most popular cleanup workflows.  Keep `--dirs-only`: files have a
  count of 0 and would match too.
- Combine with `--sort -descendants` to rank directories by the size of their subtree.

## 6. Extension Filters

The `--ext` flag filters files by extension.  It accepts individual
extensions and **collection aliases** that expand to predefined groups.

### Syntax

```bash
--ext rs                   # Single extension
--ext jpg,png,gif          # Multiple extensions
--ext documents            # Collection alias (expands to many extensions)
--ext documents,mp4,heic   # Mix collections and individual extensions
```

Extensions are case-insensitive.  A leading dot is stripped automatically
(`.txt` and `txt` are equivalent).

### Collection Aliases

| Alias | Also Accepted | Extensions |
|-------|---------------|------------|
| `pictures` | `images` | jpg, jpeg, png, gif, bmp, tiff, tif, webp, svg, ico, raw, heic |
| `documents` | `docs` | doc, docx, pdf, txt, rtf, odt, xls, xlsx, ppt, pptx, csv, md |
| `videos` | `video` | mp4, avi, mkv, mov, wmv, flv, webm, mpeg, mpg, m4v, 3gp |
| `music` | `audio` | mp3, wav, flac, aac, ogg, wma, m4a, opus, aiff |
| `archives` | `compressed` | zip, rar, 7z, tar, gz, bz2, xz, iso |
| `code` | `source` | rs, py, js, ts, java, c, cpp, h, hpp, go, rb, php, swift, kt |
| `executables` | `exec` | exe, msi, bat, cmd, ps1, com, scr, vbs, wsf, dll, sys |

These collections are not the same lists as the `--type` categories
(§9): for example `--ext executables` includes `dll` and `sys`, while
`--type executable` does not.

### Examples

```bash
# All image files across all drives
uffs '*' --ext pictures

# All source code files
uffs '*' --ext code --files-only

# Documents and spreadsheets modified this week
uffs '*' --ext documents --newer 7d

# Archives larger than 100 MB
uffs '*' --ext archives --min-size 100MB

# Mix: documents plus MP4 and HEIC
uffs '*' --ext documents,mp4,heic
```

### Best Practice

- Collections are the fastest way to search for "all images" or "all
  documents" without remembering every extension.
- Use `--ext jpg,png,gif` instead of a regex like `>.*\.(jpg|png|gif)$`:
  it is simpler to write (UFFS runs that exact regex shape as an extension
  lookup anyway, but any other regex tests every name).

## 7. Exclude Filter

The `--exclude` flag removes entries whose name matches a pattern
**after** the main pattern match.

```bash
# All .txt files, but skip anything starting with "backup"
uffs '*.txt' --exclude 'backup*'

# All files, but exclude temp files
uffs '*' --exclude '*.tmp' --files-only

# Rust files, but skip test files
uffs '*.rs' --exclude '*test*'

# Several patterns: separate them with |
uffs '*' --files-only --exclude '*.tmp|*.bak'
```

The exclude pattern is matched against the **filename only** (not the full
path) and is always case-insensitive.  Like a search pattern, a plain word
without wildcards matches anywhere in the name.  Use `--exclude` or
`--not-contains`, not both: together they currently do not work as
expected.

## 8. Path Filter

The `--in-path` flag filters by the **directory portion** of the resolved
path.  It matches a glob pattern against everything *except* the filename —
useful for limiting results to a specific directory tree without changing
the search pattern.

```bash
# Only .rs files under directories containing "projects"
uffs '*.rs' --in-path '*projects*'

# Only files under Windows\System32
uffs '*.dll' --in-path '*windows\system32*'

# Combine with exclude — .dat files in temp dirs, but not named *backup*
uffs '*.dat' --in-path '*temp*' --exclude '*backup*'

# Everything under C:\Users (the directory path ends with a \)
uffs '*.docx' --drive C --in-path 'c:\users\*'
```

The glob is matched **case-insensitively** against the directory path only
(not the filename).  The directory path includes the drive and ends with
`\`, for example `C:\Users\me\`.  A plain word without wildcards matches
anywhere in it, and `|` separates alternatives.  This is a post-filter —
applied after path resolution.

`--not-in-path` does the opposite: it takes a **comma-separated** list of
globs and drops every entry whose directory path matches any of them.

```bash
# Every .log file, except those under AppData or a cache folder
uffs '*.log' --not-in-path '*appdata*,*cache*'
```

## 9. Type Filter

The `--type` flag filters by **semantic file category**.  UFFS maps file
extensions to 24 human-readable categories and lets you filter, sort, and
output by category name.

```bash
uffs '*' --type code          # All source code files
uffs '*' --type picture       # All images
uffs '*' --type executable    # All executables
uffs '*' --type database      # All database files
```

### Available Categories

| Category | Extensions |
|----------|-----------|
| `archive` | zip, rar, 7z, tar, gz, bz2, xz, iso |
| `audio` | mp3, wav, flac, aac, ogg, wma, m4a, opus, aiff |
| `backup` | bak, old, orig, swp, tmp, temp, `~` |
| `cad` | dwg, dxf, step, stp, stl, obj, fbx, blend, 3ds, gltf, glb |
| `cert` | pem, crt, cer, der, pfx, p12, key, csr, jks |
| `code` | rs, py, js, ts, java, c, cpp, h, hpp, go, rb, php, swift, kt |
| `config` | ini, cfg, conf, yaml, yml, toml, json, xml, env, properties, reg, inf, plist |
| `data` | csv, tsv, parquet, avro, arrow, ndjson, jsonl, dat, sav, hdf5 |
| `database` | db, sqlite, sqlite3, mdb, accdb, sql, ldf, mdf, ndf, dbf |
| `disk` | vmdk, vhd, vhdx, vdi, qcow2, img, wim, iso, dmg |
| `document` | doc, docx, pdf, txt, rtf, odt, xls, xlsx, ppt, pptx, csv, md |
| `ebook` | epub, mobi, azw, azw3, djvu, cbr, cbz |
| `executable` | exe, msi, bat, cmd, ps1, com, scr |
| `font` | ttf, otf, woff, woff2, eot, fon |
| `log` | log, out, err, trace, evt, evtx |
| `picture` | jpg, jpeg, png, gif, bmp, tiff, tif, webp, svg, ico, raw, heic |
| `script` | sh, bash, zsh, fish, csh, ksh, awk, sed, lua, pl, pm, tcl |
| `shortcut` | lnk, url, desktop, webloc |
| `system` | sys, dll, drv, ocx, cpl, ax, mui |
| `video` | mp4, avi, mkv, mov, wmv, flv, webm, mpeg, mpg, m4v, 3gp |
| `web` | html, htm, css, scss, less, sass, jsx, tsx, vue, svelte, wasm, xhtml |
| `directory` | (NTFS directory flag) |
| `file` | (no extension) |
| `other` | (unknown extension) |

> **Filtering and display can differ for a few extensions.**  `--type X`
> matches every extension in X's row above, so `--type data` finds `.csv`
> files and `--type disk` finds `.iso` files.  The `type` *column* and the
> `type` sort show a single category per file, picked in this order:
> document, picture, video, audio, archive, code, executable, script, web,
> font, database, config, log, backup, disk, data, cad, shortcut, system,
> cert, ebook.  So a `.csv` file shows as `document` and an `.iso` file as
> `archive`.

> **How it runs.**  For every category with an extension list, `--type`
> becomes an extension filter applied during the scan (combined with
> `--ext` when both are given: only extensions in both lists remain).
> Only `directory`, `file` and `other` are checked afterwards, as a
> post-filter.  Directories use the `directory` category, files without
> an extension use `file`, and extensions not in any category map to
> `other`.

## 10. Bulkiness Filter

Bulkiness measures the **waste ratio** between allocated disk space and
logical file size.  A perfectly packed file has bulkiness = 100 (100%).
A file allocating 5× its logical size has bulkiness = 500.

| Flag | Meaning |
|------|---------|
| `--min-bulkiness <N>` | Only files/dirs with bulkiness ≥ N% |
| `--max-bulkiness <N>` | Only files/dirs with bulkiness ≤ N% |

```bash
# Find files wasting ≥5× their logical size
uffs '*' --min-bulkiness 500 --files-only

# Find perfectly packed files (no waste)
uffs '*' --min-bulkiness 100 --max-bulkiness 100 --files-only

# Large files with high waste
uffs '*' --min-size 1MB --min-bulkiness 1000
```

> For directories, bulkiness uses the subtree metrics:
> `tree_allocated × 100 / treesize`.

## 11. Size on Disk Filters

Size on Disk (`SizeOnDisk` / allocated size) reflects the actual bytes
consumed on the physical volume, which may differ from the logical size
due to NTFS compression, sparse files, or cluster alignment.

| Flag | Meaning |
|------|---------|
| `--min-size-on-disk <SIZE>` | Only files with allocated size ≥ value |
| `--max-size-on-disk <SIZE>` | Only files with allocated size ≤ value |
| `--exact-size-on-disk <SIZE>` | Shortcut for min = max = value |

```bash
# Compressed files using < 1MB on disk despite larger logical size
uffs '*' --attr compressed --max-size-on-disk 1MB --min-size 10MB

# Files consuming at least 1GB on disk
uffs '*' --min-size-on-disk 1GB --files-only
```

## 12. Tree Size / Tree Allocated Filters

Tree metrics aggregate **subtree totals** for directories.  `TreeSize` is
the sum of logical sizes of all descendants; `TreeAllocated` is the sum of
allocated sizes.

| Flag | Meaning |
|------|---------|
| `--min-treesize <SIZE>` | Directories with subtree size ≥ value |
| `--max-treesize <SIZE>` | Directories with subtree size ≤ value |
| `--min-tree-allocated <SIZE>` | Directories with subtree allocated ≥ value |
| `--max-tree-allocated <SIZE>` | Directories with subtree allocated ≤ value |

```bash
# Directories containing at least 1 GB of files
uffs '*' --dirs-only --min-treesize 1GB --sort treesize --sort-desc

# Directories using less than 100 MB on disk
uffs '*' --dirs-only --max-tree-allocated 100MB
```

## 13. Name and Path Length Filters

| Flag | Meaning |
|------|---------|
| `--min-name-length <N>` | Filenames with at least N characters |
| `--max-name-length <N>` | Filenames with at most N characters |
| `--min-path-length <N>` | Full paths with at least N characters |
| `--max-path-length <N>` | Full paths with at most N characters |

```bash
# Find files with very long names (> 100 chars)
uffs '*' --min-name-length 100 --files-only

# Find paths approaching MAX_PATH (260 chars)
uffs '*' --min-path-length 240

# Short filenames (8.3 candidates)
uffs '*' --max-name-length 12 --files-only
```

> Name length is checked in the hot-path.  Path length is a post-filter
> (requires resolved full path).

## 14. Month-of-Year Filter

The `--month` flag filters by the **month** of the last-modified timestamp,
across all years.  Useful for seasonal analysis.

```bash
# Files modified in January (any year)
uffs '*' --month jan

# Files modified in Q4 (Oct/Nov/Dec)
uffs '*' --month Q4

# Files modified in summer months
uffs '*' --month jun,jul,aug
```

### Accepted Formats

| Format | Example | Expands to |
|--------|---------|------------|
| Full name | `january` | Month 1 |
| Abbreviation | `jan` | Month 1 |
| Quarter | `Q1` | Months 1, 2, 3 |
| Combo | `jan,feb,Q4` | Months 1, 2, 10, 11, 12 |

## 14a. Malformed-Name Filters (forensic)

NTFS stores file names as UTF-16 with **no well-formedness guarantee** — a name
can contain an unpaired surrogate that has no valid UTF-8 form. UFFS retains
such names byte-faithfully (it never silently "heals" them to `U+FFFD`), so it
can find files that other tools — even Windows Explorer and `dir` — mangle or
hide. These filters surface exactly those names.

```bash
# Every file/dir whose own NAME is ill-formed (not valid UTF-8)
uffs '*' --malformed

# The inverse — only well-formed names
uffs '*' --well-formed

# Every entry whose PATH has an ill-formed component (e.g. a clean-named file
# living under a crooked directory)
uffs '*' --malformed-path
```

| Flag | Matches |
|------|---------|
| `--malformed` | the entry's own leaf name is ill-formed UTF-16 |
| `--well-formed` | the entry's own leaf name is valid UTF-8 |
| `--malformed-path` | any component of the resolved path is ill-formed (⊇ `--malformed`) |

`--malformed` runs on the hot path (it keeps the `--limit` fast path);
`--malformed-path` is path-derived and post-filtered, so it scans more — scope
it with other filters when possible.

### Seeing the true bytes

Because the displayed name of an ill-formed file degrades to `U+FFFD`, two
different crooked names can look identical on screen. The opt-in `name_hex`
column emits the **true WTF-8 bytes as hex** so you can tell them apart and
recover the exact name (decode with `xxd -r -p`):

```bash
# Surface the malformed flag + the true bytes as columns (CSV)
uffs '*' --malformed --columns path,malformed,malformed_path,name_hex
```

The `malformed` / `malformed_path` columns render `0`/`1` like the attribute
flags (`--columns ...,malformed`); `name_hex` is non-empty only for ill-formed
names. In CSV, none of these appear unless explicitly requested.  In
`--format json` (which ignores `--columns`), every row carries `malformed`
and `malformed_path`, and an ill-formed row also carries `name_hex`.

### Inline corrupt-name markers (`--normalize-malformed`)

`name_hex` disambiguates corrupt names in a separate column. When you instead
want the marker **inline in the path / name**, so a plain `grep`, a CSV consumer,
or a script can spot and parse corrupt entries by string, pass
`--normalize-malformed`. Each ill-formed UTF-16 code unit then renders as
`<BAD:HHHH>` (the four-hex code unit) in place of the default `�`:

```bash
# Default: one U+FFFD per corrupt code unit (matches Explorer / Everything)
uffs '*' --malformed --columns path
#   G:\UFFS_corrupted_names\evil�.exe

# Normalized: greppable, reversible marker
uffs '*' --malformed --normalize-malformed --columns path
#   G:\UFFS_corrupted_names\evil<BAD:D800>.exe
```

`<` and `>` cannot appear in a real NTFS name, so the marker never collides with
a legitimate file. The hex keeps two different corrupt names distinct (where a
bare `�` would not) and is reversible to the true code unit. The valid parts of
a name, including its extension, are preserved, so `bad_<surrogate>.rs` prints as
`bad_<BAD:DCFF>.rs`. It is **display only**: it changes how corrupt names print,
never which rows match (use `--malformed` to filter).

> Corrupt-named entries also keep their true **position** in the tree. A file
> under a crooked directory resolves at its real path (the directory is no longer
> collapsed out of the path), so it is findable where it actually lives.

## 15. Result Limit

The `--limit` (or `-n`) flag caps the number of results returned.

```bash
# Top 20 largest files
uffs '*' --files-only --sort size --sort-desc --limit 20

# Just check if any .exe exists on D:
uffs '*.exe' --drive D --limit 1
```

A limit of `0` (the default) means unlimited.

## 16. Combining Filters: Recipes

Filters are **ANDed together** — every filter must pass for a row to
appear.  This makes it easy to build precise queries by stacking filters.

### Find the Top 10 Largest PDFs Modified This Month

```bash
uffs '*.pdf' --files-only --newer 30d --sort size --sort-desc --limit 10
```

### Find Empty Directories on C: Drive

```bash
uffs '*' --dirs-only --max-descendants 1 --drive C
```

### Find Hidden Encrypted Files Over 1 MB

```bash
uffs '*' --attr hidden,encrypted --min-size 1MB --files-only
```

### Find Old Archives Untouched for 2+ Years

```bash
uffs '*' --ext archives --older 730d --files-only --sort size --sort-desc
```

### Find Recently Created Source Code

```bash
uffs '*' --ext code --newer-created 7d --files-only
```

### Find Large Directory Trees with Many Entries

```bash
uffs '*' --dirs-only --min-descendants 500 --sort descendants --sort-desc --limit 20
```

### Find Wasteful Files (High Bulkiness)

```bash
uffs '*' --files-only --min-bulkiness 500 --sort bulkiness --sort-desc --limit 20
```

### Find Source Code Modified in January

```bash
uffs '*' --type code --month jan --files-only
```

### Files in a Specific Directory Tree

```bash
uffs '*.log' --in-path '*windows\system32*' --newer 7d
```

### Largest Directory Subtrees

```bash
uffs '*' --dirs-only --min-treesize 10GB --sort treesize --sort-desc --limit 20
```

### Files Deleted Since a Baseline (`--diff`)

`--diff <BASELINE>` searches the files that were **deleted** since a
baseline MFT capture (made earlier with `uffs --snapshot`), instead of the
live files.  Every filter on this page applies to that set.  The drive
must be loaded in the daemon.

```bash
# .txt files deleted from C: since the baseline, modified in the last 30 days
uffs '*.txt' --diff C_baseline.bin --drive C --newer 30d
```

## 17. Quick Reference

```text
SCOPE
  --files-only               Files only (no directories)
  --dirs-only                Directories only (no files)
  --hide-system              Hide the NTFS metafiles ($MFT, $Bitmap, …)
  --hide-ads                 Hide Alternate Data Stream entries

SIZE
  --min-size <SIZE>          Minimum logical file size (e.g. 100MB)
  --max-size <SIZE>          Maximum logical file size
  --exact-size <SIZE>        Exact logical size (min = max)
  --min-size-on-disk <SIZE>  Minimum allocated (on-disk) size
  --max-size-on-disk <SIZE>  Maximum allocated (on-disk) size
  --exact-size-on-disk <SIZE> Exact allocated size (min = max)

DATE / TIME
  --newer <SPEC>             Modified within / after  (7d, 24h, 2026-01-15)
  --older <SPEC>             Modified before
  --newer-created <SPEC>     Created within / after
  --older-created <SPEC>     Created before
  --newer-accessed <SPEC>    Accessed within / after
  --older-accessed <SPEC>    Accessed before
  --between <START,END>      Time range shorthand (--newer START --older END)
  --month <SPEC>             Month-of-year filter (jan, Q4, jun,jul,aug)

ATTRIBUTES
  --attr <LIST>              Require/exclude NTFS attrs (hidden, !system, …)

DESCENDANTS (entries in the whole subtree + 1; empty dir = 1)
  --min-descendants <N>      Minimum descendant count (dirs)
  --max-descendants <N>      Maximum descendant count (dirs)
  --exact-descendants <N>    Exact descendant count (min = max)

EXTENSIONS & TYPE
  --ext <LIST>               Filter by extension or collection alias
  --type <CATEGORY>          Filter by semantic type (code, picture, …)

PATH
  --in-path <GLOB>           Filter by directory path glob
  --not-in-path <GLOBS>      Drop entries whose directory path matches any glob
  --exclude <GLOB>           Exclude files matching filename glob

DELETED FILES
  --diff <BASELINE>          Search files deleted since a baseline capture

TREE METRICS
  --min-treesize <SIZE>      Minimum subtree logical size (dirs)
  --max-treesize <SIZE>      Maximum subtree logical size
  --min-tree-allocated <SIZE> Minimum subtree allocated size (dirs)
  --max-tree-allocated <SIZE> Maximum subtree allocated size

BULKINESS
  --min-bulkiness <N>        Minimum waste ratio (100 = 1×, 500 = 5×)
  --max-bulkiness <N>        Maximum waste ratio

NAME / PATH LENGTH
  --min-name-length <N>      Minimum filename character count
  --max-name-length <N>      Maximum filename character count
  --min-path-length <N>      Minimum full path character count
  --max-path-length <N>      Maximum full path character count

LIMIT
  -n, --limit <N>            Maximum result count (0 = unlimited)

TIME SPEC FORMATS
  90s / 30m / 24h / 7d / 2w       Relative durations
  2026-01-15                       ISO date (YYYY-MM-DD, midnight UTC)
  today / yesterday / tomorrow     Named days (UTC)
  this_week / last_week / next_week  Calendar weeks (Monday start)
  this_month / last_month / next_month  Calendar months
  this_year / ytd / last_year      Calendar years
  last_7d / last_30d / last_90d / last_365d  Named durations
```

## See also

- [Concepts](concepts.md): what each size, count and attribute means
- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Search Modes](search-modes.md): glob, literal and regex patterns
- [Sorting](sorting.md): sort columns and multi-tier sorts
