# Troubleshooting

Common issues and solutions when using UFFS.

## 1. "Connection refused" or "Daemon not running"

**Cause:** The daemon is not running, or a stale PID file exists from
a crashed daemon.

**Fix:**

```bash
# Let auto-start handle it — just run a search
uffs '*.txt'

# Or start manually
uffs --daemon start --data-dir ~/uffs_data

# If stale files are blocking startup
uffs --daemon kill
uffs '*.txt'
```

On Windows, if the running daemon was started from an Administrator
terminal and no Access Broker is installed, `uffs --daemon stop`, `kill`
and `restart` refuse to run from a normal terminal ("Daemon management
commands require an elevated (Administrator) shell"). Run them from an
Administrator terminal, or install the broker once (`uffs-broker --install`).

## 2. "Permission denied" (Windows)

**Symptom:** a search fails with
`daemon needs admin privileges to read NTFS Master File Tables …`.

**Cause:** Reading the NTFS MFT requires Administrator privileges, and no
daemon is running yet to do it for you.

**Fix**, best first:

1. **Install the Access Broker once** from an elevated terminal. After that,
   every search and daemon start/stop runs from a normal terminal with no
   UAC prompt:

   ```powershell
   uffs-broker --install
   ```

2. Or run your terminal as Administrator (right-click → **"Run as
   administrator"**).
3. Or let UFFS ask for elevation with a UAC prompt when it starts the
   daemon: `uffs --daemon start --elevate`, or set `UFFS_ELEVATE=1` so every
   automatic start does the same.

Without elevation, UFFS can still search offline MFT captures via
`--mft-file` or `--data-dir`.

## 3. First Search Is Slow

**Not a bug.** The first search loads the MFT into memory and builds
the in-memory index (~7 s from cache, or ~66 s cold for a large system).
Every subsequent search completes in ~200 ms end-to-end.

A later search can also wait: a drive left idle for 30 minutes or more is
parked to save memory, and the next search that needs it pages it back in,
which can take a while. While it waits, `uffs` prints nothing; the result
appears when the drive is ready. The wait is capped by
`UFFS_CLIENT_TIMEOUT_SECS` (default 600 seconds = 10 minutes; `0` = no
limit). When the cap is reached, `uffs` fails with an error naming what had
not finished. See [Daemon](daemon.md#memory-tiers-and-why-you-never-see-hot).

If the first search is *always* slow (even when the daemon is running),
the daemon may be restarting each time:

```bash
# Check if daemon is running
uffs --daemon status

# Long view — build, elevation / broker mode, live-update loops,
# memory tiers, paths, per-drive detail and performance counters
uffs --daemon status -v
```

## 4. No Results Returned

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Zero results for any pattern | Wrong data source | Check `--data-dir` path; verify MFT files exist |
| Missing files from specific drives | Drive not loaded | Check `uffs --daemon status` for loaded drives |
| Missing `$` files | `--hide-system` given | Leave out `--hide-system` (it is off by default and hides the `$` NTFS metadata files when given) |
| Missing directories | `--files-only` active | Remove `--files-only` or use `--dirs-only` |

## 5. Stale Results

On Windows, live drives keep up with changes on their own: the daemon
follows each drive's USN change journal, so new, renamed and deleted files
show up without a restart.

**Offline MFT captures** (macOS, Linux, or `--data-dir` / `--mft-file` on
Windows) are snapshots: they show the drive as it was when the capture was
taken. To see newer files:

1. Take a fresh capture on the Windows machine (see
   [Cache & Data Sources](cache-and-data.md)).
2. Restart the daemon on the new data, bypassing the old cache:

```bash
uffs --daemon stop
uffs --daemon start --no-cache --data-dir ~/uffs_data
```

## 6. Build Failures

### "error: requires nightly compiler"

UFFS requires the Rust nightly pinned in `rust-toolchain.toml`. You do not
pick it yourself: in the repository folder, `rustup` installs and uses the
pinned toolchain automatically.

```bash
# In the repository folder: install the pinned toolchain
rustup toolchain install
# Or just build — rustup reads rust-toolchain.toml automatically
cargo build --release
```

### Polars compilation takes forever

Polars is a large dependency.  The first build compiles it from
source (~4 minutes).  Subsequent builds are incremental (~25 seconds).

The `uffs-polars` facade crate exists specifically to cache Polars
compilation.  Do not import `polars` directly in other crates.

## 7. macOS / Linux: "No MFT data found"

macOS and Linux cannot read NTFS drives directly.  You need offline
MFT captures.

```bash
# Check your data directory
ls ~/uffs_data/drive_c/

# Make sure it contains .bin, .mft, or .iocp files
# See: cache-and-data.md for setup instructions
```

> **Full guide:** [Cache & Data Sources](cache-and-data.md)

## 8. High Memory Usage

A drive's index is fully in memory only while the drive is in use. A drive
left idle for 30 minutes is parked, and after 24 hours it goes cold, which
releases more memory; the next search that needs it brings it back. See
[Daemon](daemon.md#memory-tiers-and-why-you-never-see-hot). With every drive
in use, a machine with 7 NTFS drives and 25 million files uses roughly
4–6 GB of RAM.

**Reduce memory:**

- Start the daemon with only the drives you need (Windows):
  `uffs --daemon start --drive C --drive D`.
  (`--drives C,D` on a search only limits that search; it does not change
  what the daemon loads.)
- Release drives you are done with now: `uffs --daemon hibernate D E`.
- The daemon retires after 2 hours idle (releases memory), unless it is
  [resident](daemon.md#permanent-residency-start-at-login-never-retire).

## 9. The Daemon Restarts After I Stop It

If you made UFFS [resident](daemon.md#permanent-residency-start-at-login-never-retire),
a supervisor is keeping it alive on purpose: on Windows the UFFS watchdog,
on macOS launchd and on Linux systemd.  A **deliberate** stop is
still honoured — `uffs --daemon stop` and `--daemon kill` record stop
intent, and the watchdog leaves the service down until you start it
again.  So a daemon that comes back means something *else* started it.
Check, in order:

```powershell
# 1. (Windows) What decision did the watchdog take, and on what evidence?
Get-Content "$env:LOCALAPPDATA\uffs\watchdog.log"

# 2. Is the MCP gateway up?  It starts a daemon when a tool call needs one.
uffs --mcp status

# 3. Is residency armed at all?  Shows login item, marker, and watchdog.
uffs --daemon resident status
```

A log line reading `HonourStopIntent` means the watchdog did **not**
touch it — look instead at the MCP gateway, or at whatever ran a
search, since any search auto-starts a daemon.

To stop the services and have them stay stopped, turn residency off
first — that removes the login item and the auto-spawn marker and
disarms the watchdog, then stop the daemon (`resident off` leaves a
running daemon alone on purpose):

```bash
uffs --daemon resident off
uffs --daemon stop
```

## 10. After an Update

### `winget upgrade` fails with `remove_all: Access is denied`

The running daemon and broker hold the program files open, so WinGet cannot
replace them. Use `uffs --update` instead: it stops them, runs the WinGet
upgrade for you, and starts them again. See [Updating](updating.md).

### The MCP HTTP gateway is down after `uffs --update`

The update stops the HTTP gateway and does not start it again. Start it:

```bash
uffs --mcp start
```

Stdio MCP sessions started by your AI host are not affected.

## 11. Getting Help

```bash
# Show all available flags
uffs --help

# Show subcommand help
uffs --daemon --help
uffs --agg --help

# Verbose mode for diagnostic output
uffs '*.txt' -v
```

If you are still stuck, include the output of these commands when
reporting an issue:

```bash
uffs --version
uffs --daemon status -v
uffs --status --json
```

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Daemon](daemon.md): the resident index server and how to manage it
- [Advanced Diagnostics](advanced-diagnostics.md): profiling, logging and environment variables
