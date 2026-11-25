# Updating

UFFS updates itself. One command keeps your whole install current:

```bash
uffs --update
```

It checks the latest GitHub release, and if you're behind — or your install is
inconsistent — it downloads, SHA-256-verifies, and atomically swaps every core
binary in place (journaled, with automatic rollback on failure). If you're
already current, it does nothing and touches no services.

> **WinGet installs:** use `uffs --update` here too — it updates the WinGet
> install in place (the binaries live in a per-user `…\WinGet\Links` location
> UFFS treats as an ordinary unmanaged install). Prefer it over `winget upgrade
> SkyLLC.UFFS`, which can fail with `remove_all: Access is denied` because the
> running daemon and broker hold the binaries open while WinGet tries to replace
> them. `uffs --update` stops those services first, then swaps. If you do want
> `winget upgrade`, stop UFFS first: elevated `uffs --daemon stop` +
> `uffs-broker --stop`.

## What `uffs --update` guarantees

Run it from any starting state and it reconciles to a **complete, latest core
install**:

| Starting state | What `uffs --update` does |
|---|---|
| Already on the latest release | Nothing — reports "up to date", touches no services |
| Behind the latest release | Downloads + swaps every core binary to latest |
| **Mixed versions** (a half-finished prior update) | Realigns every binary to one version |
| **A core binary is missing** (deleted) | Re-acquires and places it back |
| A previous update crashed mid-flight | Auto-heals (finishes or rolls back) on the next run |

The **core binary set** is `uffs` (CLI), `uffsd` (daemon), `uffsmcp` (MCP
server), `uffs-update` (the updater), `uffs-mft` (MFT tools), and — on Windows
only — `uffs-broker` (the elevated-handle service). It's the single set every
flow honours; print it any time with:

```bash
uffs --update bins
```

Updates are **journaled**: every file transition is an atomic rename with a
`.bak` kept until the new image passes a smoke test, so a crash at any point
leaves a recoverable state and never a half-written binary.

## Commands

`uffs --update [<action>] [--options]` — with **no action** it updates
end-to-end (the everyday command). The actions expose the phases:

| Command | What it does |
|---|---|
| `uffs --update` | Update to the latest release if needed (the default). |
| `uffs --update check` | Is an update available? Detect + compare. **Non-mutating.** |
| `uffs --update doctor` | Health-check the install (versions, dirs, journal, backups, services, broker, release reach). If it's out of date / inconsistent it points you to `uffs --update` (asks first on a terminal). |
| `uffs --update repair` | `doctor` + self-heal: resume/roll back an interrupted update, sweep stale backups, restart stopped services — and run the update flow if the install is out of date. |
| `uffs --update apply --version <tag>` | Install a **specific** release (see *Pinning* below). |
| `uffs --update recover` | Finish or roll back an interrupted update now (foreground). |
| `uffs --update bins` | Print the core binary stems (one per line) — for scripts/tooling. |
| `uffs --update snapshot` / `acquire` | Inspect the individual phases (freeze state / download + verify into staging). |

Add `-v` / `--verbose` to any of them for the full per-binary + per-process
breakdown.

## Pinning or switching to a specific version

Bare `uffs --update` always targets the **latest** release and won't downgrade.
To install an exact release — including rolling **back** to an older one — use
the `apply` action with `--version`:

```bash
# Switch to a specific release (downgrade included): acquire + verify + swap
uffs --update apply --version v0.6.5

# Just stage & verify it without swapping (inspect first):
uffs --update acquire --version v0.6.5
```

Notes:

- The target tag must have published release assets (recent releases do).
- After a **downgrade**, `uffs --update check` will report an update is
  available again — that's expected; bare `uffs --update` pulls you back to
  latest.

## Health check and self-heal

```bash
uffs --update doctor        # diagnose
uffs --update doctor -v     # diagnose, full detail
uffs --update repair        # diagnose + fix what can be fixed automatically
```

`doctor` is non-mutating: it reports versions, the update working dirs, any
in-flight journal, stale backups, running services, the Windows broker pipe, and
whether the release feed is reachable. When it finds something the update flow
fixes (out of date, version-skewed, or a missing core binary) it **redirects**
there — printing a hint when piped, asking on an interactive terminal, or (with
`repair` / `--repair`) running the update automatically.

Add `--offline` to skip every network check (and the update redirect).

## Notes and edge cases

- **MCP server:** the stdio MCP server (`uffsmcp`) is launched on demand by your
  LLM client (Claude Desktop / Cursor / Windsurf), so the updater stops it to
  free the file lock during a swap and lets the client respawn it — it does not
  try to restart it.
- **Windows broker:** if `uffs --update doctor` warns the broker pipe isn't
  serving, install it from an elevated PowerShell with `uffs-broker --install`.
  The broker is a `LocalSystem` service, so refreshing **it** needs elevation: a
  **non-elevated** `uffs --update` updates every binary *except* the broker —
  the running broker keeps serving (its wire protocol is back-compatible) and
  the update prints a reminder. Run `uffs --update` once from an **elevated**
  shell to bring the broker up to date too. (Everything else — daemon included —
  updates without elevation once the broker is installed.)
- **Publisher: Unknown:** binaries aren't code-signed yet, so a fresh download
  may show a SmartScreen / UAC warning — see
  [Installation](installation.md) for verifying a download with `CHECKSUMS.txt`
  and SLSA provenance.

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Uninstalling](uninstall.md): remove UFFS and its data
- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Daemon](daemon.md): the resident index server and how to manage it
- [Troubleshooting](troubleshooting.md): common errors and their fixes
