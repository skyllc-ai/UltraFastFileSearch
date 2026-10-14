# Daemon

The UFFS daemon is a long-running background process that holds MFT
indices in memory and serves search queries over a local IPC socket.
Searches that would normally take 60+ seconds to load data complete in
**~200 ms** end-to-end because the daemon keeps everything hot.

> **See also:** [Getting Started](getting-started.md) ·
> [CLI Overview](cli-overview.md) ·
> [Cache & Data Sources](cache-and-data.md) ·
> [Advanced Diagnostics](advanced-diagnostics.md)

---

## 1  Architecture

```
┌─────────┐                          ┌─────────────┐
│ uffs CLI ├──── JSON-RPC over ──────┤ uffs-daemon  │
│ uffs-tui │     local IPC socket    │  (in-memory  │
│ uffs --mcp │                         │   MFT index) │
└─────────┘                          └─────────────┘
```

The daemon loads MFT data once at startup, then serves any number of
search queries without re-reading disk.  Multiple CLI, TUI, and MCP
clients share the same daemon instance.

| Transport | Platform |
|-----------|----------|
| Unix domain socket | macOS / Linux |
| Named pipe | Windows |

---

## 2  Quick Start

### macOS / Linux (Offline MFT Files)

On non-Windows platforms, the daemon works with MFT capture files (`.iocp`,
`.bin`, `.mft`) exported from Windows NTFS volumes.

```bash
# Start the daemon with a data directory
uffs --daemon start --data-dir ~/uffs_data

# Or with individual MFT files
uffs --daemon start --mft-file /path/to/C_mft.iocp --mft-file /path/to/D_mft.iocp

# Search (daemon is already running — instant results)
uffs "*.rs" --data-dir ~/uffs_data

# Auto-start: if no daemon is running, search starts one automatically
uffs "*.dll" --data-dir ~/uffs_data
```

The `--data-dir` flag points to a directory with `drive_c/`, `drive_d/`, etc.
subdirectories, each containing an MFT capture file.

### Windows (Live NTFS Drives)

On Windows, the daemon auto-discovers all NTFS drives and reads their MFT
directly.  No `--data-dir` or `--mft-file` needed.

```powershell
# Start the daemon (auto-discovers C:, D:, E:, ...)
uffs --daemon start

# Search — daemon auto-starts if not running
uffs "*.exe"

# Force specific drives only
uffs --daemon start --drive C --drive D
```

> **Note:** Live MFT access needs elevation. Install the Access Broker **once**
> (`uffs-broker --install`, from an elevated terminal) and the daemon — its
> start/stop/restart and non-elevated updates — runs with **no UAC**; otherwise
> start it from an Administrator terminal.

---

## 3  Auto-Start

You rarely need to start the daemon manually.  When you run `uffs` (or
any client), the auto-start mechanism handles everything:

1. CLI checks if a daemon is already running (reads PID file, probes
   socket).
2. If no daemon is found, the CLI **spawns one in the background**,
   passing along `--data-dir`, `--mft-file`, and drive flags from
   the current command.
3. The CLI waits for the daemon to become "Ready" (MFT loaded, index
   built).
4. The CLI sends the search query over IPC.

This means your first `uffs *.txt --data-dir ~/uffs_data` on a clean
machine does everything: spawn daemon, load MFT, build index, search,
return results.  The next search is instant.

---

## 4  Idle Retirement

The daemon retires automatically after being idle for **2 hours**
(7200 seconds).  No cleanup needed — the PID file and socket are
removed on exit.

| Setting | Flag | Default |
|---------|------|---------|
| Idle timeout | `--idle-timeout <SECS>` | `7200` (2 hours) |
| Disable retirement | `--no-retire` | Off |

These flags are passed by the auto-start mechanism.  You can also set
them on `uffs --daemon start`:

```bash
# Never retire (run indefinitely)
uffs --daemon start --data-dir ~/uffs_data --idle-timeout 0

# Retire after 30 minutes
uffs --daemon start --data-dir ~/uffs_data --idle-timeout 1800
```

### Permanent residency (start at login, never retire)

If UFFS is part of your every-day workflow, make the daemon
**resident** instead of tuning timeouts:

```bash
# Windows — auto-discovers NTFS drives, zero UAC (uses the Access Broker):
uffs --daemon resident on

# macOS / Linux — provide MFT data:
uffs --daemon resident on --data-dir ~/uffs_data

# Inspect / undo:
uffs --daemon resident status
uffs --daemon resident off
```

`resident status` reports all three moving parts — the login item, the
auto-spawn marker, and (on Windows) whether the watchdog is
supervising.  `resident off` removes all three.  It deliberately leaves
a **running** daemon running, and says so; stop it explicitly if that
is what you want.

`resident on` registers a per-user login item (Windows: `HKCU` Run
key; macOS: launchd LaunchAgent; Linux: systemd user unit) that
starts `uffsd --no-retire` at login, and starts the daemon
immediately when none is running.  The daemon then never removes
itself from the process list — searches are always instant — while
the memory-tiering ladder still parks unused drives down to a few MB,
so residency costs almost nothing.  On macOS and Linux a crashed
resident daemon is relaunched automatically (a clean
`uffs --daemon stop` is honored and does not relaunch).

Residency also survives crashes and stops on **every** platform
through the auto-spawn marker: `resident on` records the resident
configuration next to the daemon's PID file, and any daemon started
implicitly (the next search, an MCP tool call) inherits it — most
importantly `--no-retire`.  Flags you pass explicitly always win over
the marker.  `resident off` removes the marker along with the login
item.

### The watchdog — surviving crashes and installers

The login item delivers residency at boot, and the auto-spawn marker
revives a dead daemon on the next search.  Neither notices a service
that vanishes **mid-session while nobody is searching**.  On macOS and
Linux launchd and systemd close that gap; on Windows the `Run` key
fires once at login and never again.  So `resident on` also arms
**`uffs-watchdog`**, a small user-level supervisor.

| | |
|---|---|
| Supervises | `uffsd` (daemon) and `uffsmcp` (MCP HTTP gateway) |
| Does **not** supervise | the Access Broker — see below |
| Privileges | none; it runs as you, like everything else residency touches |
| Poll interval | 5 s (`UFFS_WATCHDOG_POLL_SECS=<secs>` to change) |
| Crash budget | 3 respawns per 60 s, then it gives up and says so |
| Log | `watchdog.log`, beside the PID file |

It is a **separate binary** on purpose.  A supervisor cannot supervise
its own death, so it has to outlive the teardowns that kill what it
watches — `just use-local` force-kills `uffsd` and `uffsmcp` by image
name, and a different image name survives that.  It is also not a
`uffs` subcommand, because a long-running `uffs.exe` would lock the
most frequently replaced binary in the tree.

**A deliberate stop always wins.**  `uffs --daemon stop`, `--daemon
kill`, and `uffs --mcp stop` record *stop intent* next to the PID file
(`daemon.stopped` / `mcp.stopped`); the watchdog honours it until you
explicitly start that service again, which clears the marker.  This is
launchd's `KeepAlive.SuccessfulExit = false` contract — without it the
supervisor fights you every time you stop something on purpose.

**It never introduces a service you never ran.**  Each service is
supervised only after it has been seen running at least once, so a
machine that has never started the MCP gateway does not acquire one
because a watchdog is present.

**The Access Broker is deliberately excluded.**  It is a `LocalSystem`
service registered `start= auto`, so the Service Control Manager
already restarts it at boot — and a non-elevated process cannot
`StartService` it anyway.  Supervising it from here would require
elevation and break the zero-UAC property residency exists to protect.
The right mechanism there is SCM failure actions, configured once at
`uffs-broker --install` time.

#### Reading the watchdog log

`resident on` starts the watchdog with its output discarded, so every
decision is also appended to `watchdog.log` in the lifecycle directory
(`%LOCALAPPDATA%\uffs\` on Windows, `~/Library/Application Support/uffs/`
on macOS, `~/.local/share/uffs/` on Linux):

```
daemon down: stop_intent=false (marker …\daemon.stopped) recent_respawns=0 -> Respawn
daemon down: stop_intent=true  (marker …\daemon.stopped) recent_respawns=1 -> HonourStopIntent
```

Each line records the decision **and the inputs that produced it**, so
"why did my daemon come back?" is answerable from the file rather than
by guesswork.  The four decisions are `Respawn` (gone, and you did not
ask for that), `HonourStopIntent` (gone because you stopped it),
`GaveUp` (respawned too often inside the window — something is broken
in a way restarting cannot fix), and `Leave`.

Liveness is read from `uffs --status --json`, which reports every
service under its own key, so one service's state can never be
mistaken for another's.  An unreadable probe means *unknown*, and
unknown is always left alone — a supervisor that restarts things it
cannot see manufactures the outage it exists to prevent.

### Memory tiers — and why you never see `Hot`

Each drive's index sits in one of four tiers, visible in
`uffs --daemon status_drives`:

| Tier | What is in RAM | Reached by |
|------|----------------|------------|
| `Hot` | Body, **pre-faulted** into the working set | `uffs --daemon preload` **only** |
| `Warm` | Body, fully searchable | initial load, and every promotion |
| `Parked` | Bloom filter + path trie; body released | 30 min idle |
| `Cold` | Nothing (encrypted on-disk cache only) | 24 h idle |

**`Hot` is an operator mode, not something the daemon reaches on its
own.**  Nothing promotes a drive to `Hot` because it is busy — there is
exactly one code path that creates a `Hot` shard and it is `preload`.
A freshly loaded drive starts `Warm`, and a query that promotes a
`Parked` or `Cold` drive promotes it back to **`Warm`**, never past it.
So on a daemon where `preload` has never run, every drive reads `warm`
forever and the `Hot → Warm` idle threshold
(`UFFS_HOT_TO_WARM_IDLE_SECS`, default 600 s) never fires — there is
nothing `Hot` to demote.  The effective ladder is
`Warm → Parked → Cold`.

For serving queries the two active tiers are **identical**: dispatch
treats `Warm` and `Hot` as one set, and a `Hot` drive is not searched
faster.  What `preload` actually buys is:

* **Pre-faulting** — it issues a `PrefetchVirtualMemory` hint, pulling
  the mapped pages into the working set up front.  This is the real
  win: it moves first-touch paging off the critical path of your next
  query.  On a large index (say 5 GB across seven drives, several on
  HDDs) that first query can otherwise take tens of seconds while the
  pages fault in — everything after is memory-speed.
* **A pin** — demotion is blocked until the pin expires (30 min by
  default, `--pin-minutes` to change), plus one extra rung of runway
  afterwards.

```bash
# Make the drives you actually search ready, and hold them there.
uffs --daemon preload --drives C,D --pin-minutes 60
```

Note that residency and `Hot` are different promises: `--no-retire`
keeps the **process** alive, while the tiering ladder still parks the
drives underneath it.  A resident daemon left idle overnight still pays
the page-in on the next first query unless it was preloaded.

**A search that needs a parked drive waits for it.**  Bringing a
`Parked` or `Cold` drive back re-reads its index from disk, which can
take minutes on a large HDD volume.  `uffs` waits silently while that
happens: it prints the result and nothing else, so scripts that read its
output are unaffected.  The daemon log records each warm-up and how long
it took.  The wait is capped by the client limit, `UFFS_CLIENT_TIMEOUT_SECS`
(default 600 s = 10 minutes; `0` means no limit).  If the limit runs out,
`uffs` exits with an error that names what had not finished: the
daemon's own index load, the drives still being brought back, or a
daemon that no longer answers.  Set the variable lower if you would
rather get that error sooner, for example `UFFS_CLIENT_TIMEOUT_SECS=120`.

---

## 5  Management Commands

| Command | Description |
|---------|-------------|
| `uffs --daemon start` | Start the daemon (with data sources) |
| `uffs --daemon status` | Show PID, uptime, loaded drives, record counts |
| `uffs --daemon status -v` | Long view: build, elevation / broker mode, live-update, memory, paths, performance counters, and the physical-drive inventory |
| `uffs --daemon status --json` | Machine-readable status + drives + stats |
| `uffs --daemon status_drives` | Per-drive tier + telemetry table (resident bytes, query rate, pins) |
| `uffs --daemon stop` | Graceful shutdown via RPC (records stop intent) |
| `uffs --daemon kill` | Hard kill + remove PID/socket files (records stop intent) |
| `uffs --daemon restart` | Stop → re-start with same data sources |
| `uffs --daemon resident on\|off\|status` | Login autostart + no idle retire; arms the watchdog |
| `uffs --daemon preload` | Promote drive(s) to `Hot` and pin the tier |
| `uffs --daemon hibernate` | Demote drive(s) to `Cold` (frees RAM, cache stays) |
| `uffs --daemon load` | Hot-load additional MFT file(s) into a running daemon |
| `uffs --daemon forget` | Evict drive(s) and delete their on-disk caches |
| `uffs --daemon changed-since <DRIVE>` | USN-journal delta probe: which files changed since a cursor (read-only) |

`stop` and `kill` record *stop intent* so the [watchdog](#the-watchdog--surviving-crashes-and-installers)
does not undo them; the next explicit `start` clears it.

### `uffs --daemon status`

The short view is a one-glance health summary:

```
$ uffs --daemon status
═══ UFFS Daemon ═══
● running  PID 72558
  Version:  0.6.24
  Uptime:   2m 25s
  Drives:   7 loaded · 25,846,853 records
  Queries:  2 (avg 1.19ms, 0.0/s)
```

The health glyph is colour-coded on a terminal (green `●` running, yellow
`◐` loading/refreshing); colour is dropped automatically when the output is
piped or `NO_COLOR` is set.

### `uffs --daemon status -v`  (long view)

`-v` / `--verbose` expands every section, including the performance counters
that used to live under the separate `uffs --daemon stats` command (now folded
in here):

```
$ uffs --daemon status -v
═══ UFFS Daemon ═══
● running  PID 52044
  Version:     0.6.31
  Uptime:      10 m   20 s
  Drives:      7 loaded · 24,897,476 records
  Queries:     0
── Build ──
  Commit:    96f165b96
  Elevated:  yes (direct elevated reads)
── Live update ──
  Journal:   7 journal loop(s) running
── Memory ──
  Index heap:  5021 MB
  Mimalloc:    4316 MB committed
  RSS:         3743 MB
── Paths ──
  Data:     C:\Users\you\AppData\Local\uffs\cache
  Socket:   C:\Users\you\AppData\Local\uffs\daemon.sock
── Performance ──
  Startup duration:  9 s  278 ms
  Total records:     24,897,476
  Queries served:    0
  Queries/second:    0.00
  Agg cache:         0 hits / 0 misses (0.0% hit-rate, 0 entries)
── Drives ──
  ● G:       15,384 records (live)  ·      2 MB  [rec=   1 names=   0 tri=   0 ch=  0 ext=  0]
  ● F:    1,203,779 records (live)  ·    297 MB  [rec= 101 names=  37 tri= 124 ch=  9 ext=  4]
  ● C:    3,289,117 records (live)  ·    757 MB  [rec= 276 names=  95 tri= 327 ch= 25 ext= 12]
── Physical drives ──
  ● C:* NVMe         1.53 TB ·   91% used ·  144.19 GB free  “BOOT 990”   · indexed (  3,289,117 records)
  ● D:  HDD          7.28 TB ·   65% used ·    2.52 TB free  “DATA”       · indexed (  7,253,055 records)
  · E:  HDD        931.51 GB ·  100% used ·    2.29 GB free  “Software”   · not loaded
  ● G:  Removable   14.72 GB ·   84% used ·    2.35 GB free  “NTFS_16_GB” · indexed (     15,384 records)
```

Every column in both drive blocks is padded to a fixed width, so the
sections read as tables even though each row is rendered
independently — sizes right-align on their units, and the
`[rec=… names=…]` breakdown lines up across rows.

**`── Drives ──`** lists what the daemon has **loaded**.  Each is
labelled by its **letter** — live Windows volumes by their real letter,
and offline `.bin`/`.mft` captures by the letter derived from the file,
tagged **`(file)`** so a capture is distinguishable from a live volume
(the source filename itself is not shown).  The trailing
`[rec=… names=… tri=… ch=… ext=…]` is the per-drive memory breakdown —
record, name-arena, trigram, child-map, and extension shard sizes in MB.

**`── Physical drives ──`** is the inventory of what *exists* on the
machine, whether or not UFFS has indexed it — bus type, capacity, usage,
free space, volume label, and either `indexed (N records)` or
`not loaded`.  The `*` marks the boot volume.  This is the section to
check when a search comes back empty: a drive listed here as
`not loaded` is one the daemon never read.

> The **short** view collapses all of this to a single count/records
> line; use `-v` for the per-drive lists or `--json` for the structured
> `{"letter","records","tier"}` array.

### `uffs --daemon status_drives`

The tier table shows what each drive is costing you in RAM right now,
and why it is in the tier it is in:

```
$ uffs --daemon status_drives
DRIVE  TIER    RESIDENT   QPM     LAST QUERY        PIN UNTIL        PROMOTIONS
C      warm       757 MiB 0.00    10m ago           -                         0
D      warm     1.630 GiB 0.00    10m ago           -                         0
G      warm         2 MiB 0.00    10m ago           -                         0
```

| Column | Meaning |
|--------|---------|
| `TIER` | `hot` / `warm` / `parked` / `cold` — see [Memory tiers](#memory-tiers--and-why-you-never-see-hot) |
| `RESIDENT` | Bytes held in RAM for that drive, scaled to `MiB` / `GiB` |
| `QPM` | Queries per minute against that drive (drives the tiering decisions) |
| `LAST QUERY` | How long since it was last searched |
| `PIN UNTIL` | Demotion is blocked until this time — set by `preload --pin-minutes` |
| `PROMOTIONS` | How often this drive has been promoted back up the ladder; a high count on an idle machine means the thresholds are too aggressive for your workload |

> **`uffs --daemon stats` has been folded into `uffs --daemon status -v`.**
> The old command now prints a one-line redirect.


### `uffs --daemon changed-since` — USN-journal delta probe

Read-only probe for the daemon's `changed_since` RPC: ask which files
changed on a drive since a cursor, answered from the NTFS USN journal
instead of any index sweep. Two-step usage mirrors the cursor contract:

```powershell
# 1. Bootstrap — no cursor yet; the daemon prints a fresh one:
$ uffs --daemon changed-since C
Drive C: USN journal delta
  journal_id: 133742...
  next_usn:   4520448
  delta:      unavailable — no cursor given (bootstrap)

# 2. Change some files, then ask for the delta since that cursor:
$ uffs --daemon changed-since C --journal-id 133742... --since-usn 4520448
Drive C: USN journal delta
  changed:    3 file(s)
    frs       281474  ...
```

A cursor that no longer matches the volume's journal (the journal was
recreated, or wrapped past it) comes back as `delta: unavailable` with a
fresh cursor — the daemon never fabricates a delta it cannot prove. A
large catch-up is paged: `--max-records N` bounds one call, and a
`truncated` response is continued from its printed `next_usn`. This is
the operator surface of the journal-delta capability consumed
programmatically over the `changed_since` RPC
(`uffs-client::protocol::response_journal`).

### `uffs --daemon status --json`

For scripts and dashboards, `--json` emits the machine-readable superset
(status + drives + stats) under stable top-level keys:

```
$ uffs --daemon status --json
{
  "running": true,
  "status": { "status": {"state": "ready"}, "pid": 72558, "uptime_secs": 591,
              "git_sha": "a1b2c3d", "elevated": false, "reading_via_broker": true,
              "live_update": {"active_loops": 7}, "paths": { ... } },
  "drives": [ { "letter": "C", "records": 3428455, "tier": "warm" }, ... ],
  "stats":  { "total_queries": 2, "queries_per_second": 0.0, ... }
}
```

For **multi-service** scripting, prefer `uffs --status --json`, which
reports the daemon, the Access Broker, and both MCP transports under
their own top-level keys:

```
$ uffs --status --json
{
  "broker":    { "installed": true, "running": true, "pipe_serving": true, ... },
  "daemon":    { "running": true, "status": { ... }, "drives": [ ... ] },
  "mcp_http":  { "running": true, "pid": 2912, "endpoint": "http://127.0.0.1:8080/mcp" },
  "mcp_stdio": { "sessions": [ ... ] }
}
```

Each service carries its own `running` flag.  Read that flag rather
than scanning the human output for the word "running": the text views
mention *other* services by design — `uffs --mcp status` reports the
daemon too — so a substring scan will attribute one service's state to
another.  The watchdog learned this the hard way, and now reads this
document.

---

## 6  Logging

The daemon runs detached — its stdout is `/dev/null`.  To capture logs,
use `--log-file` and `--log-level`:

```bash
uffs --daemon start --data-dir ~/uffs_data \
    --log-level debug \
    --log-file ~/uffs_daemon.log
```

| Flag | Default | Description |
|------|---------|-------------|
| `--log-level <LEVEL>` | `info` | Tracing level: `error`, `warn`, `info`, `debug`, `trace` |
| `--log-file <PATH>` | *(none)* | Write daemon logs to this file |

The `RUST_LOG` and `UFFS_LOG_DIR` environment variables also control
logging — see [Advanced Diagnostics](advanced-diagnostics.md) for details.

---

## 7  Platform Differences

| Aspect | Windows | macOS / Linux |
|--------|---------|---------------|
| Data source | Live NTFS MFT (auto-detected) | Offline captures (`.iocp`, `.bin`, `.mft`) |
| Privileges | Admin **once** (Access Broker) → then none; else Administrator | None (reads regular files) |
| IPC transport | Named pipe | Unix domain socket |
| Auto-discovery | All NTFS drives | Requires `--data-dir` or `--mft-file` |

### IPC Socket Locations

| Platform | Default path |
|----------|-------------|
| macOS | `~/Library/Application Support/uffs/uffs-daemon.sock` |
| Linux | `$XDG_RUNTIME_DIR/uffs/uffs-daemon.sock` or `/tmp/uffs/uffs-daemon.sock` |
| Windows | `\\.\pipe\uffs-daemon` |

PID files are stored alongside the socket.  `uffs --daemon kill` removes
both if a graceful stop fails.

---

## 8  Performance

### Windows — Live NTFS, 7 Drives, 25.9M Records

Measured on AMD Ryzen 9 3900XT (12c/24t, 64 GB DDR4), 7 NTFS volumes
(NVMe + SATA SSD + SATA HDD), 25,929,744 total records:

| Operation                   | Time       |
|-----------------------------|------------|
| Daemon startup (cold, all drives) | ~66 s |
| Daemon startup (warm cache)      | ~7 s  |
| Search end-to-end (HOT, CLI)     | ~200–380 ms |
| Daemon-side search (HOT)         | ~151 ms |
| Graceful stop               | ~15 ms     |
| Hard kill                   | ~25 ms     |

Cold startup is dominated by raw MFT reading.  Warm cache startup
deserializes `.iocp` files (~7 s for 25.9M records).  Once loaded,
the daemon-side search takes ~151 ms for all 25.9M records; the
~200–380 ms CLI time includes process spawn, IPC round-trip, and
stdout formatting.

> 📖 **Full data:** [Performance](performance.md) — per-drive
> cold/warm/hot tables, profile internals, query pattern comparison.

---

## 9  Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| "Connection refused" on search | Daemon not running | Let auto-start handle it, or `uffs --daemon start` |
| Stale PID file | Previous daemon crashed | `uffs --daemon kill` removes PID + socket |
| First search slow after restart | MFT being loaded | Normal — ~7 s warm cache (or ~66 s cold), sub-second after |
| "Permission denied" (Windows) | No broker + not elevated | Install the Access Broker once (`uffs-broker --install`, elevated) for zero-UAC, or run the terminal as Administrator |
| Multiple daemons running | Rare race condition | `uffs --daemon kill` + `uffs --daemon start` |

> **More troubleshooting:** [Troubleshooting](troubleshooting.md)

---

## 10  Readiness Verification

A comprehensive test script exercises all daemon lifecycle combinations
(10 scenarios, 68 steps):

```bash
# macOS/Linux: with a data directory
rust-script scripts/dev/daemon-readiness.rs ~/uffs_data

# macOS/Linux: with a single MFT file
rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp

# macOS/Linux: with custom search pattern
rust-script scripts/dev/daemon-readiness.rs ~/uffs_data --pattern "*.dll"

# Windows: auto-discovers live NTFS drives (no path needed)
rust-script scripts/dev/daemon-readiness.rs

# Windows: with custom pattern
rust-script scripts/dev/daemon-readiness.rs --pattern "*.exe"
```

Scenarios tested: clean lifecycle, idempotent ops on stopped daemon, double
start, hard kill recovery, graceful stop→start cycle, restart data
preservation, double restart, stats accumulation, kill→status, and search
auto-start.

