# Output Formats

UFFS can write search results as CSV, as JSON (one object per line), or
as an aligned table.  Without `--format`, a search shown in a terminal is
an aligned **table**, and output that is piped, redirected or written
with `--out` is **CSV**: pretty for eyes, structured for scripts.

## 1. Choosing a Format

```bash
uffs '*.txt' --format csv      # Comma-separated values, for scripts and files
uffs '*.txt' --format json     # Newline-delimited JSON (NDJSON)
uffs '*.txt' --format table    # Human-readable aligned table
uffs '*.txt' --format custom   # CSV followed by a per-drive summary
uffs '*.txt' -f json           # -f is short for --format
```

| Format | Flag | Best for |
|--------|------|----------|
| CSV | `--format csv` | Piping to scripts, importing into Excel/Polars |
| JSON | `--format json` | API consumers, jq processing, structured logging |
| Table | `--format table` | Interactive terminal use, quick inspection |
| Custom | `--format custom` | CSV plus the legacy drive-summary footer of the original tool |

## 2. CSV Output

Name the columns you want with `--columns` (see §5):

```bash
uffs '*.rs' --columns name,size,modified,path --limit 3
```

```
"Name","Size","Last Written","Path"

"main.rs",1234,2026-04-01 09:15:32,"C:\Projects\uffs\src\main.rs"
"lib.rs",567,2026-03-28 14:22:01,"C:\Projects\uffs\src\lib.rs"
"args.rs",890,2026-04-10 08:00:00,"C:\Projects\uffs\src\args.rs"
```

What to expect:

- The header row holds each column's display name in quotes (the
  `modified` column is headed "Last Written"), and a **blank line**
  follows it.
- Text columns (`name`, `path`, `path_only`, `type`, `extension`,
  `name_hex`) are quoted; numbers, dates and 0/1 flags are not.
- Sizes are in bytes.  Dates are `YYYY-MM-DD HH:MM:SS` in your local time
  zone (see `--tz-offset` below).

### Customising CSV

| Flag | Default | Description |
|------|---------|-------------|
| `--sep <CHAR>` | `,` | Column separator |
| `--quotes <CHAR>` | `"` | Quote character for text columns |
| `--header <BOOL>` | `true` | Include the header row: `true`/`false`, `yes`/`no` or `1`/`0` (lowercase) |
| `--pos <STR>` | `1` | Representation for true/active boolean attrs |
| `--neg <STR>` | `0` | Representation for false/inactive boolean attrs |
| `--tz-offset <HOURS>` | local offset | Write dates at this offset from UTC, e.g. `--tz-offset 0` for UTC |

The daemon's CSV writer also understands these names for `--sep` (in any
case): `TAB`, `SPACE`, `NEWLINE`, `RETURN`, `DOUBLE` (`"`), `SINGLE` (`'`)
and `NULL`.  The command line's own CSV writer, which is used in some
cases, takes the value literally, so passing the character itself
(`$'\t'` in bash, `` "`t" `` in PowerShell) works everywhere.

```bash
# Tab-separated, no header
uffs '*.log' --columns name,size,path --sep $'\t' --header false

# Semicolon separator, single quotes
uffs '*.csv' --columns name,path --sep ';' --quotes "'"

# Boolean attributes as yes/no
uffs '*' --attr hidden --pos yes --neg no --columns name,hidden,path
```

## 3. JSON Output (NDJSON)

Each result is a JSON object on its own line (Newline-Delimited JSON).
JSON always carries the same fixed set of keys; `--columns` does not
apply to it.

```bash
uffs '*.rs' --format json --limit 1
```

```json
{"accessed":134195349320000000,"allocated":4096,"created":134190000000000000,"descendants":0,"drive":"C","file_reference":281474976712345,"flags":32,"is_directory":false,"malformed":false,"malformed_path":false,"modified":134195349320000000,"name":"main.rs","path":"C:\\Projects\\uffs\\src\\main.rs","size":1234,"tree_allocated":4096,"treesize":1234}
```

| Key | Meaning |
|-----|---------|
| `drive` | Drive letter, e.g. `"C"` |
| `path`, `name` | Full path and file name |
| `size`, `allocated` | Logical size and size on disk, in bytes |
| `is_directory` | `true` for directories |
| `modified`, `created`, `accessed` | Raw NTFS timestamps (integers, see below) |
| `flags` | Raw NTFS attribute bits (e.g. `32` = archive) |
| `descendants`, `treesize`, `tree_allocated` | Tree metrics ([Concepts](concepts.md)) |
| `malformed`, `malformed_path` | Ill-formed name flags ([Filters §14a](filters.md#14a-malformed-name-filters-forensic)) |
| `name_hex` | Present only for an ill-formed name: its true bytes as hex |
| `file_reference` | NTFS file reference number (`0` when not available) |

The timestamps are Windows FILETIME values: 100-nanosecond intervals
since 1 January 1601 UTC.  In jq, `(.modified / 10000000 - 11644473600) |
todate` turns one into a readable UTC date.

### Usage with jq

```bash
# Extract just the paths
uffs '*.rs' --format json | jq -r '.path'

# Sum total size
uffs '*.rs' --format json --files-only | jq -s 'map(.size) | add'

# Filter in jq (when UFFS filters aren't enough)
uffs '*.log' --format json | jq 'select(.size > 1000000)'
```

## 4. Table Output

Columns are aligned for terminal readability.

```bash
uffs '*.rs' --format table --limit 3
```

The table always shows the same four columns: **Name**, **Size** (in
readable units such as `1.20 MB`), **Modified** (local time) and **Path**.
It ignores `--columns`, `--sep`, `--quotes` and `--header`; use CSV or
JSON when you need other columns.

## 5. Column Selection

Use `--columns` to choose the columns of CSV output, as a comma-separated
list (names are not case-sensitive):

```bash
# Only name, size, and path
uffs '*.pdf' --columns name,size,path

# Every standard column
uffs '*.pdf' --columns all
```

Without `--columns`, CSV carries every standard column (the `all` set
below), wherever it goes.  In scripts, name the columns you need so a
future column does not shift yours.

**`all`** is: `path`, `name`, `path_only`, `size`, `size_on_disk`,
`created`, `modified`, `accessed`, `descendants`, the attribute flags
below (except `temporary` and `virtual`), `attributes`, `treesize`,
`tree_allocated`, `bulkiness`, `type`, `extension`, `name_length`,
`path_length`.

**Opt-in columns**, only when you name them: `drive`, `attribute_value`,
`temporary`, `virtual`, `malformed`, `malformed_path`, `name_hex`.

**`parity`** (or the `--parity-compat` flag) gives the 25-column layout of
the original C++ tool, including its conventions for directory rows.

### NTFS Boolean Attribute Columns

These columns output `1`/`0` (or the value set by `--pos`/`--neg`):

`hidden`, `system`, `archive`, `read_only` (or `readonly`), `compressed`,
`encrypted`, `sparse`, `reparse`, `offline`, `not_indexed` (or
`notindexed`), `temporary`, `virtual`, `pinned`, `unpinned`, `integrity`,
`no_scrub` (or `noscrub`), `directory_flag`, `recall_on_open`,
`recall_on_data_access`.

```bash
# Show just name, size, and which attributes are set
uffs '*' --columns name,size,hidden,system,compressed,encrypted --limit 10
```

## 6. Output to File

Use `--out` to write results to a file instead of stdout.  The daemon
writes the file itself, and always as **CSV**, whatever `--format` says:

```bash
# Write CSV to file
uffs '*.dll' --out dll_inventory.csv

# JSON to a file: redirect stdout instead of using --out
uffs '*.log' --format json > logs.json

# Pipe to other tools (works with stdout, not --out)
uffs '*.rs' --format json | jq -r '.path' | sort
uffs '*.dll' --columns path --header false | wc -l
```

## 7. Scripting Patterns

### Count files by extension

```bash
uffs '*' --count --facet extension
```

### Export full inventory to CSV

```bash
uffs '*' --files-only --columns all --out full_inventory.csv
```

### Feed paths to another tool

In CSV, paths are quoted.  For bare paths, one per line, take them from
JSON:

```bash
# List all matching paths, one per line
uffs '*.bak' --files-only --format json | jq -r '.path'

# On Windows (PowerShell): delete matching files
uffs '*.tmp' --files-only --format json |
  ForEach-Object { ($_ | ConvertFrom-Json).path } |
  Remove-Item
```

### Top extensions

```bash
# Server-side: the top 50 extensions by count and size
uffs '*' --files-only --agg by_extension

# Client-side with jq: count files by extension
uffs '*' --format json --files-only | \
  jq -r '.name | split(".") | last' | sort | uniq -c | sort -rn | head -10
```

> For server-side aggregation (faster than client-side jq pipelines),
> see [Aggregation](aggregation.md).

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Sorting](sorting.md): sort columns and multi-tier sorts
- [Getting Started](getting-started.md): your first search and everyday recipes
