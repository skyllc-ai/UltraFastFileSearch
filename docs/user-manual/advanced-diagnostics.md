# Advanced Diagnostics

UFFS includes built-in profiling, benchmarking, and diagnostic flags for
performance analysis and debugging.  These are aimed at developers,
system administrators, and CI pipelines.

## 1. Profiling

### `--profile`

Show a detailed timing breakdown after every search:

```bash
uffs '*.dll' --profile
```

Output includes:

- **MFT read time** — how long it took to parse the MFT (if cold start)
- **Index build time** — name interning, path resolution
- **Query time** — filter evaluation, sorting
- **Output time** — serialisation, formatting
- **Total wall time** — end-to-end

### `--benchmark`

Run the full search pipeline but suppress output.  Measures raw
engine performance without I/O overhead:

```bash
uffs '*.dll' --benchmark
```

Useful for comparing filter strategies, sort performance, or
MFT read modes.

## 2. MFT Read Modes

| Flag | Description |
|------|-------------|
| `--no-bitmap` | Disable the MFT bitmap optimisation — reads ALL MFT records, including free/deleted. Default reads only allocated records. |
| `--full` | Enable extension record merging — resolves files with many hard links or Alternate Data Streams. Adds ~15–25% read time. |

### Query Mode Override

The `--query-mode` flag forces a specific query execution path:

| Mode | Description |
|------|-------------|
| `auto` | (Default) Daemon mode when daemon is running; falls back to direct |
| `index` | Use the in-memory compact index path |
| `dataframe` | Use the Polars DataFrame lazy query path |

```bash
uffs '*.dll' --query-mode dataframe --profile
```

## 3. Verbose Output

```bash
uffs '*.dll' -v                    # Short form
uffs '*.dll' --verbose             # Long form
```

Verbose mode prints diagnostic information to stderr:

- Data sources being loaded
- Drive detection results
- Cache hit/miss status
- IPC connection details
- Record counts per drive

## 4. Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Standard Rust `tracing` filter (e.g. `RUST_LOG=uffs_core=debug`) |
| `UFFS_LOG_DIR` | Directory for daemon log files |
| `UFFS_DATA_DIR` | Default data directory (overridden by `--data-dir`) |

### Tracing Examples

```bash
# See all debug-level output from the MFT reader
RUST_LOG=uffs_mft=debug uffs '*.dll'

# Trace-level for path resolution (very verbose)
RUST_LOG=uffs_core::path_resolver=trace uffs '*.dll'

# Multiple crate filters
RUST_LOG=uffs_mft=info,uffs_core=debug uffs '*.dll'
```

## 5. Parity Testing

The `--parity-compat` flag outputs results in a format compatible
with the original C++ implementation.  Used for correctness testing
between the two implementations.

```bash
uffs '*.dll' --parity-compat --columns all
```

This flag:

- Forces the 25-column baseline schema
- Uses Windows timestamp format
- Aligns boolean attribute output with the C++ convention

## 6. Timezone Control

UFFS timestamps are stored as NTFS 100ns ticks (UTC).  The display
timezone is detected from the system locale.

```bash
# Override timezone offset (hours from UTC)
uffs '*.dll' --tz-offset -5       # US Eastern
uffs '*.dll' --tz-offset 0        # UTC
uffs '*.dll' --tz-offset 9        # Japan
```

## 7. Drive Selection

```bash
# Search specific drives only
uffs '*.dll' --drives C,D

# Single drive shorthand
uffs '*.dll' -d C

# Exclude drives (search all except these)
# Not directly supported — use --drives with the ones you want
```

Drive selection applies to both search and aggregation commands.

## See also

- [Performance](performance.md): measured numbers and how to benchmark your machine
- [Daemon](daemon.md): the resident index server and how to manage it
- [Concepts](concepts.md): what each size, count and attribute means
- [CLI Overview](cli-overview.md): every flag and command at a glance
