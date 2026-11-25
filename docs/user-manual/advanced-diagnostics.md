# Advanced Diagnostics

UFFS includes built-in profiling, benchmarking, and diagnostic flags for
performance analysis and debugging.  These are aimed at developers,
system administrators, and CI pipelines.

## 1. Profiling

### `--profile`

Show a timing breakdown after every search:

```bash
uffs '*.dll' --profile
```

The breakdown goes to stderr (the numbers below are an example):

```
=== PROFILE: Client → Daemon ===
  Connect:              1 ms
  Await ready:          0 ms
  Search (IPC):       180 ms  (daemon: 151 ms)
  Output (stdout):     12 ms  (1048576 bytes formatted)
    scan=120 ms  sort=10 ms  path_resolve=18 ms  write=3 ms
  Total matches:    41230
  Transport:       shmem_blob (mmap + write_all, binary)
```

- **Connect / Await ready** — reaching the daemon and waiting until it
  has loaded its index
- **Search (IPC)** — the search round trip, with the daemon's own time
- **Output** — formatting the results
- **Index warm-up** — only when parked or cold drives had to be brought
  back first
- **scan / sort / path_resolve / write** — the daemon's own phases
- **Total matches** (or **Rows returned** / **Output lines**, depending
  on how the results were sent) and **Transport**

### `--benchmark`

Time the whole pipeline, including output formatting; the rows go to a
sink, not the screen (add `-v` to see them):

```bash
uffs '*.dll' --benchmark
uffs '*.dll' --benchmark -v     # same, but the rows go to stdout
```

### `--no-output`

Match only: the daemon builds no rows, so this times "how many files
match" and nothing else.  It is set automatically when stdout is a null
device (`NUL`, `/dev/null`).

```bash
uffs '*.dll' --no-output --profile
```

Useful for comparing filter strategies and sort performance.

## 2. MFT Read Modes

`--no-bitmap` and `--full` belong to the separate `uffs-mft` tool, not
to `uffs`.  (`uffs` rejects both as unknown flags.)

| `uffs-mft` flag | Description |
|------|-------------|
| `uffs-mft info --no-bitmap` | Disable the MFT bitmap optimisation — reads ALL MFT records, including free/deleted. Default reads only in-use records. |
| `uffs-mft read --full` | Merge extension records — resolves files with many hard links or Alternate Data Streams. Adds ~15–25% read time. |

## 3. Verbose Output

```bash
uffs '*.dll' --benchmark -v
```

For a search, `-v` / `--verbose` matters only together with
`--benchmark`: it sends the rows to stdout instead of the sink.  It
prints no extra diagnostics.  For more detail about a daemon, use
`uffs --daemon status -v`; for what the daemon did, read its log (see
[Daemon §6](daemon.md#6-logging)).

## 4. Environment Variables

| Variable | Read by | Description |
|----------|---------|-------------|
| `UFFS_LOG` | daemon | Log level or filter (e.g. `debug`, `uffs_mft=debug`); replaces `--log-level` |
| `RUST_LOG` | daemon | Used when `UFFS_LOG` is not set |
| `UFFS_LOG_DIR` | daemon, `uffs --daemon start` | Log directory (see [Daemon §6](daemon.md#6-logging)) |
| `UFFS_LOG_FILE` | search auto-start | Passed to an auto-started daemon as `--log-file` |
| `UFFS_CLIENT_TIMEOUT_SECS` | `uffs` | How long a client waits for the daemon; default 600, `0` = no limit |
| `UFFS_CLIENT_SKIP_HEALTH_CHECK` | `uffs` | `1` / `true` / `yes` skips the health check on connect |
| `UFFS_NO_AUTOSTART` | `uffs` | Any value other than empty or `0`: never start a daemon automatically |
| `UFFS_ELEVATE` | `uffs` | `1` / `true` / `yes` / `on`: allow a UAC prompt when starting the daemon (Windows) |
| `UFFS_HOT_TO_WARM_IDLE_SECS` | daemon | Idle time before `Hot` → `Warm`; default 600 |
| `UFFS_WARM_TO_PARKED_IDLE_SECS` | daemon | Idle time before `Warm` → `Parked`; default 1800 |
| `UFFS_PARKED_TO_COLD_IDLE_SECS` | daemon | Idle time before `Parked` → `Cold`; default 86400 |
| `UFFS_SEARCH_TIMEOUT_SECS` | daemon | Time limit for one search inside the daemon; default 30; a search that runs out returns an error |
| `UFFS_WATCHDOG_POLL_SECS` | watchdog | Base poll interval of `uffs-watchdog`; default 5 |

Variables read by the daemon take effect when it starts.

### Tracing Examples

The `uffs` program itself prints no tracing output.  The daemon reads
`UFFS_LOG` (or `RUST_LOG`) when it starts and writes to its log, so
restart it with the variable set:

```bash
# Debug output from the MFT reader
UFFS_LOG=uffs_mft=debug uffs --daemon restart

# Several filters
UFFS_LOG=uffs_mft=info,uffs_core=debug uffs --daemon restart
```

A debug or trace level writes the log file even without `--log-file`
(see [Daemon §6](daemon.md#6-logging)).  `uffs --daemon restart` keeps
only the capture files; to keep other start flags, use
`uffs --daemon stop` and then `uffs --daemon start` with your flags.

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

UFFS timestamps are stored as NTFS 100ns ticks (UTC).  By default they
are shown with the current UTC offset of the system time zone.

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

# Single drive shorthand (one letter)
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
