# MCP Server

> Give every AI agent instant access to your filesystem.

The UFFS MCP server bridges AI agents to the UFFS daemon using the
[Model Context Protocol](https://modelcontextprotocol.io).  Any MCP-compatible
host — Claude Desktop, Cursor, Windsurf, Augment, Codeium, or your own
tooling — can search, aggregate, and inspect files through UFFS with zero
setup beyond a single config line.

![Claude using the UFFS MCP server to find the largest video files and summarize storage by file type](../../assets/demo/uffs-mcp-claude.gif)

> *Claude querying real NTFS data through the UFFS MCP server — deterministic search underneath, MCP on top. Recorded against the real endpoint with unedited timings (see the [demo kit](../../scripts/dev/demo/README.md)).*

## How It Works

```
┌──────────────┐                    ┌──────────────┐              ┌─────────────┐
│ AI Agent     │   MCP (stdio or    │ UFFS MCP     │  IPC socket  │ uffsd        │
│ (Claude,     ├───HTTP)───────────▶│ Server       ├─────────────▶│ (in-memory   │
│  Cursor, …)  │   JSON-RPC 2.0    │ (bridge)     │              │  MFT index)  │
└──────────────┘                    └──────────────┘              └─────────────┘
```

The MCP server is a **thin bridge** — it translates MCP tool calls into
daemon queries and returns results.  It does not hold index data itself.
The daemon must be running (it auto-starts when needed).

## 1. Two transports

| | HTTP (recommended) | stdio |
|---|---|---|
| **Sessions** | Multi-session — many agents share one server | Single-session per AI host |
| **Lifecycle** | You manage: `uffs --mcp start` / `stop` | AI host manages: spawns and kills |
| **Persistence** | Stays running between agent restarts | Dies when agent disconnects |
| **Setup** | One `start`, then point hosts at URL | Config per host with `command` + `args` |
| **Access** | No authentication; listens on `127.0.0.1` (this machine only) unless you pass `--bind` | No network at all (private pipe) |
| **Best for** | Several AI hosts on this machine sharing one server | Quick setup, single-agent use |

## 2. Quick start

### HTTP mode

```bash
# Start the MCP HTTP server (auto-starts daemon too)
# Windows — auto-discovers NTFS drives:
uffs --mcp start

# macOS / Linux — provide MFT data:
uffs --mcp start --data-dir ~/uffs_data

# Custom port:
uffs --mcp start --port 9090

# Check status:
uffs --mcp status

# Performance stats:
uffs --mcp stats

# Stop:
uffs --mcp stop
```

`start` also takes `--bind <address>` (default `127.0.0.1`), `--no-cache`
(build the daemon's index without its cache) and `--mft-file <path>` (search
an offline MFT file). If a healthy gateway already answers on the port,
`start` reuses it instead of starting a second one; an unhealthy process on
the port is killed and replaced. The gateway has no authentication, so keep
it on `127.0.0.1` unless you trust every machine that can reach the address
you bind to.

### stdio mode

```bash
# Typically not run manually — configured in the AI host's MCP settings.
# The host spawns this process and communicates via stdin/stdout.
uffs --mcp run --data-dir ~/uffs_data
```

### Zero-downtime upgrades (stdio supervisor)

Every stdio session is fronted by a tiny **self-upgrading supervisor**:
the process the AI host spawns owns the stdio pipes for the whole
session and serves through a hidden worker child it can replace.  When
the `uffsmcp` binary on disk is replaced (an installer or `just
use-local` rename-swap), the supervisor notices, waits for a quiet
moment (no request in flight), spawns a fresh worker from the new
binary, replays the MCP handshake into it, and reroutes traffic — then
notifies the client with `notifications/tools/list_changed` so a
changed toolset is re-listed.  The host's MCP connection never drops:
**a new binary is picked up without restarting any AI-host session**.
The same path transparently restarts a crashed worker (rate-limited).

No configuration is needed.  Tuning knobs (rarely needed):

| Variable | Effect | Default |
|---|---|---|
| `UFFS_MCP_WORKER_EXE` | Program spawned as the worker | the supervisor's own executable |
| `UFFS_MCP_WATCH_PATH` | File whose identity change triggers a swap | the worker executable |
| `UFFS_MCP_POLL_MS` | How often the binary is checked (ms) | `5000` |

## 3. Host configuration

### Claude Desktop / Claude Code

Add to `settings.json` or `claude_desktop_config.json`:

**HTTP mode:**
```json
{
  "mcpServers": {
    "uffs": {
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

**stdio mode:**
```json
{
  "mcpServers": {
    "uffs": {
      "command": "uffs",
      "args": ["--mcp", "run", "--data-dir", "/path/to/uffs_data"]
    }
  }
}
```

Windows (no `--data-dir` needed):
```json
{
  "mcpServers": {
    "uffs": {
      "command": "uffs",
      "args": ["--mcp", "run"]
    }
  }
}
```

### Cursor

Add to `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "uffs": {
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

### Windsurf

Add to `mcp_config.json`:

```json
{
  "mcpServers": {
    "uffs": {
      "serverUrl": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

### Augment Code

HTTP mode is auto-detected when the server is running.  Stdio mode:

```json
{
  "mcpServers": {
    "uffs": {
      "command": "uffs",
      "args": ["--mcp", "run", "--data-dir", "/path/to/uffs_data"]
    }
  }
}
```

### Standalone binary (alternative)

The `uffsmcp` binary (the program `uffs --mcp` runs) can be used directly
instead of `uffs --mcp run`:

```json
{
  "mcpServers": {
    "uffs": {
      "command": "uffsmcp",
      "args": ["run"]
    }
  }
}
```

Bare `uffsmcp` (no `run`) also serves stdio, but exits after 2 hours without
MCP messages; `uffsmcp run` has no idle timeout unless you set one
(see §11).

## 4. Tools

The MCP server exposes six read-only tools.  All are annotated as
`readOnlyHint: true` — they never modify the filesystem.

| Tool | Required params | Description |
|------|----------------|-------------|
| `uffs_search` | `pattern` | Search files and directories with 40+ filter parameters |
| `uffs_aggregate` | — | Server-side analytics with 12 built-in presets |
| `uffs_facet_values` | `field` | Discover distinct values (extension, type, drive) with counts |
| `uffs_info` | `path` | Full metadata for a single file or directory |
| `uffs_drives` | — | List all indexed drives with record counts |
| `uffs_status` | — | Daemon health, uptime, memory, loading progress |

### Cold-index behaviour: the warming contract

Drives tier down when idle ([Parked after 30 min, Cold after
24 h](daemon.md#memory-tiers-and-why-you-never-see-hot)), and the
re-warm on next touch can take 30–120 s on HDD-heavy systems.  A query
tool never blocks for that.  When a query would touch a drive that is
not `Warm`/`Hot`, the tool:

1. **starts the re-warm immediately** in a detached request that
   survives whatever the host does to the tool call, and
2. **returns an error right away**: *"⏳ Index warming — drive(s) D, S
   were parked/cold and are being re-warmed now … Poll uffs_status until
   its `index_ready` field is true … then retry this exact query
   unchanged."* (The `status` field is not the signal: it reads "Ready"
   for the daemon process even while every drive is parked.)

So an agent's loop is: query → warming error → poll `uffs_status`
(never gated) until `index_ready` is true → retry → millisecond answer.  Repeat triggers are free —
the daemon's single-flight per-drive dedup joins the in-flight load
instead of starting another.  Queries scoped to warm drives are never
gated by some *other* drive being cold.

This mirrors the startup gate (`⏳ Daemon is starting up — X/Y drives
loaded`), which covers initial load the same way.

### `uffs_search`

The primary query tool.  Accepts a `pattern` and any combination of filters
(all ANDed).  See [Search Modes](search-modes.md) for pattern syntax and
[Filters](filters.md) for the full parameter list.

Key parameters:

| Parameter | Example | Purpose |
|-----------|---------|---------|
| `pattern` | `"*.pdf"`, `"invoice"`, `">report_[0-9]+"` | Glob, substring, or regex (prefix `>`) |
| `filter` | `"files"`, `"dirs"`, `"all"` | File/directory filter |
| `ext` | `"pdf"`, `"pictures"`, `"archives"` | Extension or collection alias |
| `type_filter` | `"code"`, `"document"`, `"picture"` | Semantic type category |
| `min_size` / `max_size` | `1073741824` | Size in bytes (1 073 741 824 = 1 GB) |
| `newer` / `older` | `"7d"`, `"today"`, `"2026-01-01"` | Modification time filter |
| `path_contains` | `"Users\\rnio"` | Scope to a subtree |
| `drives` | `["C", "D"]` | Scope to specific drives |
| `sort` | `"-size"`, `"modified"`, `"-treesize"` | Sort field (prefix `-` for descending) |
| `limit` | `50` | Max results (default 50, hard cap 100; larger values are clamped with a warning) |
| `projection` | `["name", "size", "path"]` | Columns to return |
| `whole_word` | `true` | Word-boundary matching |
| `attr` | `"hidden"`, `"compressed"` | NTFS attribute filter |

### `uffs_aggregate`

Server-side analytics.  Use `preset` for one-call answers or `aggregations`
for custom specs.  See [Aggregation](aggregation.md) for full documentation.

```json
{ "preset": "overview" }
{ "preset": "by_type", "drives": ["C"] }
{ "aggregations": ["terms:extension,top=30", "stats:size"] }
```

Available presets: `overview`, `by_type`, `by_extension`, `by_drive`,
`by_size`, `by_age`, `storage`, `activity`, `top_folders`, `duplicates`,
`media`, `cleanup`.

### `uffs_facet_values`

Discover what exists before searching.  Returns top-N values with counts
and byte totals.

```json
{ "field": "extension", "top": 20 }
{ "field": "type", "pattern": "*.log" }
{ "field": "drive" }
```

### `uffs_info`

Full metadata for a single path:

```json
{ "path": "C:\\Windows\\System32\\ntoskrnl.exe" }
```

Returns: size, allocated size, created/modified/accessed timestamps, NTFS
flags, MFT record number, parent directory path.

## 5. Resources

Resources are **read-only data** that agents can access even when tool
invocation is unavailable.  This is critical — some MCP clients expose
`resources/read` but not `tools/call`.

| URI | Content | Agent use |
|-----|---------|-----------|
| `uffs://cookbook` | Curated example queries with ready-to-use arguments | **Start here** — learn query patterns fast |
| `uffs://schema/search` | JSON Schema for `uffs_search` parameters | Validate/construct search calls |
| `uffs://schema/aggregate` | JSON Schema for `uffs_aggregate` parameters | Validate/construct aggregate calls |
| `uffs://schema/fields` | Complete field catalog (types, capabilities) | Discover filterable/sortable fields |
| `uffs://presets/aggregate` | Aggregate preset names with descriptions | Choose the right preset |
| `uffs://drives` | Live drive listing with record counts | Check what's indexed |
| `uffs://status` | Daemon health, loading progress | Check readiness |

### Resource templates

| URI template | Example | Content |
|---|---|---|
| `uffs://info/{path}` | `uffs://info/C%3A%5CWindows` | File/directory metadata |

The `{path}` segment is percent-encoded (`:` → `%3A`, `\` → `%5C`).

## 6. Prompts

Prompts are **guided multi-step workflows** that an agent can request
and then execute step-by-step.

| Prompt | Parameters | Workflow |
|--------|-----------|----------|
| `find_large_files` | `limit` (default 50) | Search for largest files |
| `find_by_extension` | `extension`, `limit` (default 100) | Find all files with a given extension |
| `recent_changes` | `days` (default 1) | Files modified in the last N days |
| `find_duplicates_by_name` | `filename` | Find files with the same name across drives |
| `disk_usage_report` | `drive` (optional) | Multi-step: `overview` → `by_type` → `by_extension` → `storage` presets |
| `cleanup_report` | `min_size_mb` (default 100) | Multi-step: `cleanup` preset → largest files over `min_size_mb` → `duplicates` preset |
| `duplicate_investigation` | `extension` (optional) | Aggregate duplicates → search candidates |

Agents request a prompt via `prompts/get`, receive structured instructions,
then execute the steps using the tools above.

## 7. Server management

### Commands

| Command | Description |
|---------|-------------|
| `uffs --mcp start` | Start the HTTP gateway as a background process (reuses a healthy one already on the port) |
| `uffs --mcp status` | Show the gateway's PID, transport, uptime and `/health` result, and whether the daemon is reachable and its state |
| `uffs --mcp stats` | Show the daemon's performance counters (uptime, records, queries served, query times) |
| `uffs --mcp stop` | Stop the HTTP gateway (signals its PID) and record that it was stopped on purpose, so the watchdog leaves it down |
| `uffs --mcp kill` | Hard kill + PID file cleanup; `--port <n>` also clears an orphaned process on that port (default 8080) |
| `uffs --mcp restart` | Kill the running server; your AI host respawns it, or run `uffs --mcp start` |
| `uffs --mcp reload` | Pick up a new binary: only parts older than the installed binary are touched. A stale daemon is stopped (the next request starts the new one), a stale HTTP gateway is restarted, stale stdio sessions are told to reload |

`stop` and `kill` act on the HTTP gateway. Stdio servers are owned by their
AI host and stop when the host ends the session. Stopping the gateway does
not stop the daemon.

### Status

```
$ uffs --mcp status
uffs-mcp v0.6.47

MCP server:    running (PID 89234)
  Transport:   http:127.0.0.1:8080
  Uptime:       4h 23m 10s
  Health:      ✓ (http://127.0.0.1:8080/health)

Daemon:        reachable (PID 51200)
  Status:      Ready
```

### Stats

```
$ uffs --mcp stats
MCP server PID: 89234

═══ Performance Stats ═══
Backend uptime:     4h 22m 51s
Startup duration:    7 s  412 ms
Total records:     25,912,334
Queries served:    847
Avg query time:      1 ms 240 μs
Total query time:    1 s   50 ms
Queries/second:    0.05
```

The MCP server's own counters (tool calls, errors, sessions) are in the
gateway's `/status` endpoint (§8).

## 8. HTTP endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/mcp` | MCP Streamable HTTP (JSON-RPC 2.0) |
| `GET` | `/mcp` | SSE event stream for server notifications |
| `DELETE` | `/mcp` | Close session |
| `GET` | `/health` | Liveness probe — always `200 OK` with body `ok` |
| `GET` | `/status` | Server status, uptime and MCP counters as JSON |

None of the endpoints require authentication. The gateway listens on
`127.0.0.1` by default, so only programs on this machine can reach it.

### Health check

```bash
$ curl http://127.0.0.1:8080/health
ok

$ curl http://127.0.0.1:8080/status
{
  "status": "running",
  "uptime_secs": 3421,
  "auth_enabled": false,
  "version": "0.6.47",
  "mcp_stats": {
    "tool_calls": 847,
    "tool_errors": 3,
    "avg_tool_latency_us": 2400,
    "resource_reads": 23,
    "prompt_gets": 8,
    "active_sessions": 2,
    "total_sessions": 12,
    "tools": {
      "search": 612, "drives": 6, "status": 4,
      "info": 28, "aggregate": 145, "facet_values": 52
    }
  }
}
```

## 9. Agent instructions

When an agent connects, the MCP server sends **agent instructions** as part
of the `initialize` response.  These instructions teach the agent:

- All 6 tools with one-line descriptions
- A **query strategy** (aggregate → facet → search) that minimizes round-trips
- Key parameters cheat sheet for `uffs_search`
- Resource listing (including the cookbook)
- Prompt listing

This means an agent can start using UFFS effectively without reading any
external documentation — the instructions are embedded in the protocol.

## 10. Cookbook

The `uffs://cookbook` resource is the **single most useful resource** for
agents learning to use UFFS.  It contains dozens of ready-to-use examples,
organized into categories such as:

| Category | Examples |
|----------|---------|
| Quick Find | Glob, substring, whole-word, regex, drive-scoped |
| Size Triage | Top-N largest, >1 GB, large archives |
| Time Filters | Last 7d, >2yr old, created today |
| Type & Extension | Pictures, code, executables |
| Path Scoping | User home dir, project dirs, config files |
| Subtree Analysis | Largest dirs, empty dirs, crowded dirs |
| Cleanup | Temp files, zero-byte, bulkiness, long paths |
| Aggregation | All presets, scoped, custom specs |
| Facets | Extensions, drives, scoped facets |
| Advanced & NTFS-Specific | Hidden files, NTFS compression |
| Real-World Workflows | Multi-step investigations |

Each example includes:
- **tool** — which tool to call
- **arguments** — a complete JSON object, copy-pasteable into `tools/call`
- **explanation** — why each parameter was chosen

Plus a list of power-user tips for combining parameters effectively.

## 11. Idle timeout and auto-exit

### stdio mode

`uffs --mcp run` (and `uffsmcp run`) has **no idle timeout** by default: it
runs until the AI host ends the session.  To make it exit after a quiet
period, set `--idle-timeout <seconds>`:

```json
{
  "command": "uffs",
  "args": ["--mcp", "run", "--idle-timeout", "3600"]
}
```

`--idle-timeout 0` (the default) means no timeout.  Bare `uffsmcp` with no
subcommand uses a 2-hour idle timeout.

The timeout uses a **sliding window** — every MCP request resets the
deadline.  A busy agent will never trigger it.

### HTTP mode

The HTTP gateway runs indefinitely.  Use `uffs --mcp stop` to shut it down.

## 12. Logging and diagnostics

**stdout is the protocol channel** — all diagnostic output goes to stderr
or a log file.

```bash
# Default: INFO to stderr
uffs --mcp run

# Verbose: auto-creates log file
UFFS_LOG=debug uffs --mcp run

# Explicit log file
UFFS_LOG_FILE=/tmp/mcp.log uffs --mcp run

# Both
UFFS_LOG=trace UFFS_LOG_FILE=/tmp/mcp-diag.log uffs --mcp run
```

Default log file location:
- macOS: `~/Library/Logs/uffs/uffs_mcp.log`
- Linux: `$XDG_STATE_HOME/uffs/logs/uffs_mcp.log` (by default
  `~/.local/state/uffs/logs/uffs_mcp.log`)
- Windows: `%LOCALAPPDATA%\uffs\logs\uffs_mcp.log`

Set `UFFS_LOG_DIR` to use another folder.  The HTTP gateway started by
`uffs --mcp start` logs to `mcp-gateway.log` in the same folder.

## 13. Relationship to the daemon

The MCP server is **not** the daemon.  They are separate processes:

| | Daemon (`uffsd`) | MCP Server (`uffs --mcp`) |
|---|---|---|
| **Role** | Holds MFT index in memory, executes queries | Bridges MCP protocol to daemon |
| **Data** | Yes — full file index | No — stateless bridge |
| **Started by** | Auto-started by first client | `uffs --mcp start` or AI host |
| **Stopped by** | `uffs --daemon stop` or idle timeout | `uffs --mcp stop` or AI host disconnect |
| **Multiple?** | One daemon per user | Many MCP servers (one per AI host session) |

When the MCP server connects, it auto-starts the daemon if needed.
Stopping the MCP server does **not** stop the daemon — other clients
(CLI, TUI, other MCP sessions) may still be using it.

## See also

- [Daemon](daemon.md): the resident index server and how to manage it
- [Aggregation](aggregation.md): summaries instead of rows
- [Search Modes](search-modes.md): glob, literal and regex patterns
- [Filters](filters.md): narrow results by size, date, type, attributes and more
- [Troubleshooting](troubleshooting.md): common errors and their fixes
