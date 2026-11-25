# Output Formats

UFFS supports multiple output formats for search results.  CSV is the
default; JSON and table formats are available for scripting and
interactive use.

## 1. Choosing a Format

```bash
uffs '*.txt' --format csv      # Default
uffs '*.txt' --format json     # Newline-delimited JSON (NDJSON)
uffs '*.txt' --format table    # Human-readable aligned table
uffs '*.txt' --format custom   # Custom separator + quoting
```

| Format | Flag | Best for |
|--------|------|----------|
| CSV | `--format csv` | Piping to scripts, importing into Excel/Polars |
| JSON | `--format json` | API consumers, jq processing, structured logging |
| Table | `--format table` | Interactive terminal use, quick inspection |
| Custom | `--format custom` | Custom separators, TSV, advanced scripting |

## 2. CSV Output (Default)

```bash
uffs '*.rs' --limit 3
```

```
Name,Size,Modified,Path
main.rs,1234,2026-04-01 09:15:32,C:\Projects\uffs\src\main.rs
lib.rs,567,2026-03-28 14:22:01,C:\Projects\uffs\src\lib.rs
args.rs,890,2026-04-10 08:00:00,C:\Projects\uffs\src\args.rs
```

### Customising CSV

| Flag | Default | Description |
|------|---------|-------------|
| `--sep <CHAR>` | `,` | Column separator |
| `--quotes <CHAR>` | `"` | Quote character for strings |
| `--header <BOOL>` | `true` | Include header row (`--header false` to suppress) |
| `--pos <STR>` | `1` | Representation for true/active boolean attrs |
| `--neg <STR>` | `0` | Representation for false/inactive boolean attrs |

```bash
# Tab-separated, no header
uffs '*.log' --sep $'\t' --header false

# Semicolon separator, single quotes
uffs '*.csv' --sep ';' --quotes "'"

# Boolean attributes as yes/no
uffs '*' --attr hidden --pos yes --neg no --columns Name,Hidden,Path
```

## 3. JSON Output (NDJSON)

Each result is a JSON object on its own line (Newline-Delimited JSON).

```bash
uffs '*.rs' --format json --limit 2
```

```json
{"Name":"main.rs","Size":1234,"Modified":"2026-04-01 09:15:32","Path":"C:\\Projects\\uffs\\src\\main.rs"}
{"Name":"lib.rs","Size":567,"Modified":"2026-03-28 14:22:01","Path":"C:\\Projects\\uffs\\src\\lib.rs"}
```

### Usage with jq

```bash
# Extract just the paths
uffs '*.rs' --format json | jq -r '.Path'

# Sum total size
uffs '*.rs' --format json --files-only | jq -s 'map(.Size) | add'

# Filter in jq (when UFFS filters aren't enough)
uffs '*.log' --format json | jq 'select(.Size > 1000000)'
```

## 4. Table Output

Columns are aligned for terminal readability.

```bash
uffs '*.rs' --format table --limit 3
```

## 5. Column Selection

By default, UFFS outputs all columns.  Use `--columns` to select a
subset:

```bash
# Only name, size, and path
uffs '*.pdf' --columns Name,Size,Path

# All columns (default)
uffs '*.pdf' --columns all
```

### NTFS Boolean Attribute Columns

These columns output `1`/`0` (or the value set by `--pos`/`--neg`):

`hidden`, `system`, `archive`, `readonly`, `compressed`, `encrypted`,
`sparse`, `reparse`, `offline`, `notindexed`, `temporary`, `virtual`,
`pinned`, `unpinned`, `integrity`, `noscrub`, `directory_flag`,
`device`, `normal`.

```bash
# Show just name, size, and which attributes are set
uffs '*' --columns Name,Size,Hidden,System,Compressed,Encrypted --limit 10
```

## 6. Output to File

Use `--out` to write results to a file instead of stdout:

```bash
# Write CSV to file
uffs '*.dll' --out dll_inventory.csv

# Write JSON to file
uffs '*.log' --format json --out logs.json

# Pipe to other tools (works with stdout, not --out)
uffs '*.rs' --format json | jq '.Path' | sort
uffs '*.dll' --columns Path --header false | wc -l
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

```bash
# List all matching paths (no header, path only)
uffs '*.bak' --columns Path --header false | xargs rm -v

# On Windows (PowerShell)
uffs '*.tmp' --columns Path --header false | ForEach-Object { Remove-Item $_ }
```

### JSON processing pipeline

```bash
# Find top 10 extensions by total size
uffs '*' --format json --files-only | \
  jq -r '.Ext' | sort | uniq -c | sort -rn | head -10
```

> For server-side aggregation (faster than client-side jq pipelines),
> see [Aggregation](aggregation.md).

## See also

- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Sorting](sorting.md): sort columns and multi-tier sorts
- [Getting Started](getting-started.md): your first search and everyday recipes
