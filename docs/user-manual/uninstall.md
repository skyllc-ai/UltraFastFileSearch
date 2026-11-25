# Uninstalling

`uffs --uninstall` removes UFFS and its data from the machine in one guided
flow: it analyzes what is installed, shows you a removal plan, asks for
confirmation, then removes everything in a safe order and verifies the result.
Nothing changes until you confirm.

```bash
uffs --uninstall            # analyze, show the plan, confirm, then remove
uffs --uninstall --dry-run  # show the analysis + plan and change NOTHING
uffs --uninstall -v         # the same, with the full tables
```

> Nothing is removed without your explicit `y` at the prompt (or `--yes`).
> `--dry-run` is always safe and never needs elevation.

If you made UFFS [resident](daemon.md#permanent-residency-start-at-login-never-retire)
(start at login, watchdog), turn that off **first**: the uninstall stops a
running watchdog and removes its program, but it does not remove the login
item.

```bash
uffs --daemon resident off
uffs --uninstall
```

## What it does, step by step

1. **Analyzes** the install. Every UFFS binary is found **in the order the OS
   resolves them**, so the copy a bare `uffs` actually runs is flagged `ACTIVE`
   and any shadowed / duplicate copies are shown. This explains version skew
   (e.g. a WinGet copy shadowed by a hand-placed one). By default you see a
   one-line summary; `-v` shows the full table.
2. **Inventories** every non-binary artifact with its size: the data dir, the
   encrypted cache (per-drive indexes + USN cursors), the legacy cache, the
   per-user config, and the Windows broker service.
3. **Deep sweep** (Windows only; UFFS searching for itself): asks the daemon
   for any stray `uffs*` files elsewhere on your drives. Strays are **listed
   for review, never auto-removed** (one might be a copy you placed in
   `Downloads`). A full sweep needs the daemon to cover every drive. In a
   non-elevated terminal with no Access Broker, it asks whether to start the
   daemon with one UAC prompt (`d`) or skip the sweep (`s`); with `--yes` or
   `--dry-run` it skips the sweep and says so.
4. **Plan + consent.** Prints an itemized, ordered removal plan with the total
   space reclaimed, then prompts. `--dry-run` stops here.
5. **Removes** in a safe order: stop the daemon / MCP / broker service, delete
   binaries, purge data / cache / config, clean PATH, then **verify** that the
   targeted locations are gone.

## Elevation: do you need `sudo` / Administrator?

Most items need no elevation. When some do and the terminal is not elevated,
this is the **first** thing the uninstall says, before any analysis:

| Platform | When elevation is needed |
|---|---|
| **macOS / Linux** | Only if a binary lives somewhere your user cannot write (e.g. a root-owned `/usr/local/bin`). A normal user install (`~/bin`, `~/.local/bin`, `~/.cargo/bin`, a dev build) needs **no `sudo`**. |
| **Windows** | Removing the `UffsAccessBroker` service or a machine-scope install under `%PROGRAMFILES%` needs an **elevated** shell. A per-user install does not. |

It lists the items that need it:

```
This terminal is not elevated (Administrator). The following can only be
removed from an elevated terminal (the broker runs as LocalSystem):

  - …
```

and asks what to do:

- **Windows:** `e` = elevate at removal time (Windows shows one UAC prompt),
  `c` = continue without those items (they are left as they are), `a` = abort.
- **macOS / Linux:** `c` = continue without them, `a` = abort (then re-run
  with `sudo`).

With `--yes` it continues without those items, without asking. `--dry-run`
skips the question.

## Channel-aware: WinGet is delegated, never hand-deleted

If UFFS was installed via **WinGet**, that root is handed to
`winget uninstall SkyLLC.UFFS` rather than deleted by hand, so WinGet's own
state stays consistent. When the `uffs.exe` you are running is itself part of
the WinGet package, the `winget uninstall` runs right after `uffs` exits.
Manual (GitHub-release) and dev-build installs are removed directly.

## Flags

| Flag | Effect |
|------|--------|
| `--dry-run` | Show the analysis + plan and change nothing (always safe). |
| `--yes`, `--assume-yes`, `-y` | Skip the confirmation prompt (for scripted removal). |
| `--verbose`, `-v` | Show the full binary table, inventory, and sweep detail (default: a one-line summary). |
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
  dir, and the logs in `logs\`). macOS: `~/Library/Application Support/uffs`;
  Linux: the XDG data dir (`~/.local/share/uffs`).
- **Cache:** `%LOCALAPPDATA%\uffs\cache\` (per-drive compact indexes + USN
  cursors) and the legacy `%TEMP%\uffs_index_cache\`. macOS:
  `~/Library/Caches/com.uffs`; Linux: `~/.cache/uffs`.
- **Config / settings** (unless `--keep-config`).
- **PATH entries** that point at a removed UFFS root (Windows: the registry,
  with open shells notified; macOS/Linux: a manual hint, since the shell owns
  PATH).

The running `uffs.exe` (and `uffs-update.exe`) cannot delete themselves in place
on Windows, so they are scheduled to delete the moment this process exits.

**Not removed:**

- **Logs on macOS and Linux:** `~/Library/Logs/uffs` (macOS) and
  `~/.local/state/uffs/logs` (Linux, or `$XDG_STATE_HOME/uffs/logs`). Delete
  them by hand if you want them gone. (On Windows the logs live in the data
  dir and go with it.)
- **Residency:** the login item. Run `uffs --daemon resident off` before
  uninstalling.

## Safety

- **Idempotent.** If a run is interrupted, run `uffs --uninstall` again: it
  says a previous uninstall did not finish, and completes it.
- **Best-effort.** A single item that cannot be removed (a locked file, a
  permission error) is reported; the rest still proceed, and the final
  verification lists anything that survived (and whether a reboot or elevation
  is needed).
- **Conservative PATH.** Only PATH entries pointing exactly at a removed UFFS
  root are touched.

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Updating](updating.md): keep UFFS current with `uffs --update`
- [Troubleshooting](troubleshooting.md): common errors and their fixes
