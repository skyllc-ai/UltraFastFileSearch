# Uninstalling

`uffs --uninstall` removes UFFS and **all of its data** from the machine in one
guided, reversible-until-you-confirm flow: it analyzes what is installed, shows
you an itemized plan, asks for confirmation, then removes everything in a safe
order and verifies the result.

```bash
uffs --uninstall            # analyze, show the plan, confirm, then remove
uffs --uninstall --dry-run  # show the analysis + plan and change NOTHING
```

> Nothing is removed without your explicit `y` at the prompt (or `--yes`).
> `--dry-run` is always safe and never needs elevation.

## What it does, step by step

1. **Analyzes** the install. Every UFFS binary is listed **in the order the OS
   resolves them**, so the copy a bare `uffs` actually runs is flagged `ACTIVE`
   and any shadowed / duplicate copies are shown. This explains version skew
   (e.g. a WinGet copy shadowed by a hand-placed one).
2. **Inventories** every non-binary artifact with its size: the data dir, the
   encrypted cache (per-drive indexes + USN cursors), the legacy cache, the
   per-user config, and the Windows broker service.
3. **Deep sweep** (UFFS searching for itself): asks the running daemon for any
   stray `uffs*` files elsewhere on your drives. Strays are **listed for review,
   never auto-removed** (one might be a copy you placed in `Downloads`).
4. **Plan + consent.** Prints an itemized, ordered removal plan with the total
   space reclaimed, then prompts. `--dry-run` stops here.
5. **Removes** in a safe order: stop the daemon / MCP / broker service, delete
   binaries, purge data / cache / config, clean PATH, then **verify** that the
   targeted locations are gone.

## Elevation: do you need `sudo` / Administrator?

UFFS only asks for elevation when a removal genuinely requires it, and it
**refuses up front** (before touching anything) if the run is not elevated:

| Platform | When elevation is needed |
|---|---|
| **macOS / Linux** | Only if a binary lives somewhere your user cannot write (e.g. a root-owned `/usr/local/bin`). A normal user install (`~/bin`, `~/.cargo/bin`, a dev build) needs **no `sudo`** — verified with a real `access(W_OK)` writability check. |
| **Windows** | Removing the `UffsAccessBroker` service or a machine-scope install under `%PROGRAMFILES%` needs an **elevated** shell. A per-user install does not. |

If elevation is required and missing, the command lists exactly which items need
it and exits without changing anything:

```
This uninstall includes items that require Administrator:
  - Stop + delete service UffsAccessBroker
Re-run with elevated privileges (sudo on Linux/macOS, an elevated shell on Windows):
  uffs --uninstall
```

## Channel-aware: WinGet is delegated, never hand-deleted

If UFFS was installed via **WinGet**, that root is handed to
`winget uninstall SkyLLC.UFFS` rather than deleted by hand, so WinGet's own
state stays consistent. Manual (GitHub-release) and dev-build installs are
removed directly.

## Flags

| Flag | Effect |
|------|--------|
| `--dry-run` | Show the analysis + plan and change nothing (always safe). |
| `--yes`, `-y` | Skip the confirmation prompt (for scripted removal). |
| `--keep-config` | Remove binaries + caches but **keep** the settings/config dir. |
| `--no-deep-sweep` | Skip the cross-drive search for stray UFFS files. |
| `--no-path` | Do not touch PATH (a manual hint is printed instead). |
| `--scope <user\|machine\|all>` | Restrict to a single scope (default `all`). |
| `--json` | Emit the full analysis + plan as JSON (for tooling / installers). |
| `--help`, `-h` | Show usage. |

## What gets removed

- **Binaries:** `uffs`, `uffsd`, `uffsmcp`, `uffs-update`, `uffs-mft` (and
  `uffs-broker` on Windows), in every discovered install root, plus any
  `uffs-tui` / `uffs-gui` left from earlier installs.
- **Service (Windows):** the `UffsAccessBroker` LocalSystem service + its
  registry key.
- **Data dir:** `%LOCALAPPDATA%\uffs\` (daemon pid + state, the update working
  dir). macOS: `~/Library/Application Support/uffs`; Linux: the XDG data dir.
- **Cache:** `%LOCALAPPDATA%\uffs\cache\` (per-drive compact indexes + USN
  cursors) and the legacy `%TEMP%\uffs_index_cache\`. macOS:
  `~/Library/Caches/com.uffs`; Linux: `~/.cache/uffs`.
- **Config / settings** (unless `--keep-config`).
- **PATH entries** that point at a removed UFFS root (Windows: the registry,
  with open shells notified; macOS/Linux: a manual hint, since the shell owns
  PATH).

The running `uffs.exe` (and `uffs-update.exe`) cannot delete themselves in place
on Windows, so they are scheduled to delete the moment this process exits.

## Safety

- **Dry-run + explicit consent.** Nothing is removed without `--dry-run`-able
  review and a `y` at the prompt (or `--yes`).
- **Idempotent.** If a run is interrupted, just run `uffs --uninstall` again —
  it finds and removes whatever is left. The next launch tells you a prior run
  was interrupted.
- **Best-effort.** A single item that cannot be removed (a locked file, a
  permission error) is reported; the rest still proceed, and the final
  verification lists anything that survived (and whether a reboot or elevation
  is needed).
- **Conservative PATH + strays.** Only PATH entries pointing exactly at a
  removed UFFS root are touched; stray `uffs*` files found elsewhere are listed
  for you to review, never auto-deleted.

To remove UFFS entirely:

```bash
uffs --uninstall --dry-run   # review first
uffs --uninstall             # then confirm
```

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Updating](updating.md): keep UFFS current with `uffs --update`
- [Troubleshooting](troubleshooting.md): common errors and their fixes
