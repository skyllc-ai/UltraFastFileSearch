# Updating

UFFS updates itself. One command keeps your whole install current:

```bash
uffs --update
```

It checks the latest GitHub release, and if you're behind — or your install is
inconsistent — it downloads, SHA-256-verifies, and atomically swaps every core
binary in place (journaled, with automatic rollback on failure). If you're
already current, it does nothing and touches no services.

> **WinGet installs:** use `uffs --update` here too. UFFS recognises a WinGet
> install (its files live under `…\WinGet\Packages\…`) and hands the upgrade
> to WinGet itself: it stops the daemon, stops the broker if it runs from the
> package (one UAC prompt), runs `winget upgrade SkyLLC.UFFS`, then starts
> them again. When the `uffs.exe` you are running is itself part of the
> package, the WinGet upgrade runs right after `uffs` exits. A per-user
> WinGet install must be updated from a **normal (non-elevated)** terminal:
> WinGet refuses to upgrade a user package from an elevated one, so an
> elevated `uffs --update` skips it and prints a note saying so.
> A bare `winget upgrade SkyLLC.UFFS` can fail with `remove_all: Access is
> denied`, because the running daemon and broker hold the files open; that is
> why `uffs --update` stops them first.

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
| `uffs --update apply` | Without `--version`: install the latest release now, skipping the "already up to date" check. Use it when `uffs --update` says it could not check for updates. |
| `uffs --update recover` | Finish or roll back an interrupted update now (foreground). |
| `uffs --update bins` | Print the core binary stems (one per line) — for scripts/tooling. |
| `uffs --update snapshot` / `acquire` | Inspect the individual phases (freeze state / download + verify into staging). |

Add `-v` / `--verbose` to any of them for the full per-binary + per-process
breakdown.

## Pinning or switching to a specific version

Bare `uffs --update` always moves you to the **latest** published release. If
you run a build that is newer than that release (for example one you built
yourself), it is replaced with the latest release too.
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

- **MCP server:** the updater runs `uffs --mcp stop`, which stops only the
  HTTP gateway, and does **not** start the gateway again afterwards. If you
  use the HTTP gateway, run `uffs --mcp start` after the update. Stdio MCP
  sessions (the ones your AI host launches) keep running: their supervisor
  picks up the new `uffsmcp` on its own (see
  [MCP Server](mcp.md#zero-downtime-upgrades-stdio-supervisor)).
- **Watchdog (Windows residency):** the updater stops a running
  `uffs-watchdog` before anything else, so it cannot restart the daemon
  mid-update, replaces it with the other programs, and starts it again
  last.
- **Windows broker:** if `uffs --update doctor` warns the broker pipe isn't
  serving, install it from an elevated PowerShell with `uffs-broker --install`.
  The broker is a `LocalSystem` service, so stopping and replacing **it**
  needs elevation. What a **non-elevated** `uffs --update` does depends on
  where the running broker lives:
  - **In the folder being updated:** the update cannot stop it, so it lists
    that folder as needing Administrator and asks you to continue without it
    (that folder is not updated) or abort. Run `uffs --update` from an
    **elevated** terminal to update it.
  - **In another folder:** everything else is updated, the running broker
    keeps serving (its wire protocol is back-compatible), and the update
    prints a reminder to run it once elevated to bring the broker up to date.
  - **No broker running:** nothing needs elevation and you are not asked.
- **Elevated daemon without a broker:** if the daemon was started from an
  Administrator terminal and no broker is installed, a non-elevated update
  cannot stop it. The update tells you so first and asks whether to continue
  without that folder or abort.
- **Publisher: Unknown:** binaries aren't code-signed yet, so a fresh download
  may show a SmartScreen / UAC warning — see
  [Installation](installation.md#verify-a-download) for verifying a download
  with `CHECKSUMS.txt` and SLSA provenance.

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Uninstalling](uninstall.md): remove UFFS and its data
- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Daemon](daemon.md): the resident index server and how to manage it
- [Troubleshooting](troubleshooting.md): common errors and their fixes
