# Installation

This chapter installs UFFS on Windows, macOS or Linux, checks that the
download is genuine, and puts the programs on your PATH.

**Fastest path per platform:**

```bash
# macOS / Linux — one-line installer (no sudo, installs to ~/.local/bin)
curl -fsSL https://raw.githubusercontent.com/skyllc-ai/UltraFastFileSearch/main/install.sh | bash
```

```powershell
# Windows
winget install SkyLLC.UFFS
```

The macOS/Linux installer downloads the prebuilt binaries for your platform from
the matching [GitHub Release](https://github.com/skyllc-ai/UltraFastFileSearch/releases),
**verifies each against the release `SHA256SUMS`**, and installs them to
`~/.local/bin`. Pin a version with `UFFS_VERSION=v0.6.17`, or change the target
with `UFFS_INSTALL_DIR=~/bin`. Remove everything later with `uffs --uninstall`.

Pre-built binaries are published for three platforms: **Windows x64**,
**macOS on Apple silicon (arm64)** and **Linux x64**. You can download them
directly from [GitHub Releases](https://github.com/skyllc-ai/UltraFastFileSearch/releases)
(§2). On any other platform (for example an Intel Mac or Linux on ARM64) you
build from source (§5). Most users do not need to.

## 1. WinGet (Windows: Recommended)

If you have the [Windows Package Manager](https://learn.microsoft.com/windows/package-manager/)
(bundled with Windows 11 and modern Windows 10), install in one command:

```powershell
winget install SkyLLC.UFFS
```

This installs the `uffs` CLI together with the programs it works with (the
`uffsd` daemon, the `uffsmcp` MCP server, `uffs-mft`, `uffs-update`,
`uffs-broker`, and the free `uffs-tui` demo) and puts them on your PATH
automatically.

Update later with UFFS's own command, which runs the WinGet upgrade for you
(see [Updating](updating.md)):

```powershell
uffs --update
```

Confirm the install:

```powershell
uffs --version
```

> Live NTFS search needs elevation **once**: install the Access Broker
> (`uffs-broker --install`, one-time from an elevated terminal) and every later
> search, daemon start and daemon stop runs with **no UAC**. Updating the broker
> itself needs one elevation (see [Updating](updating.md)). Without the broker,
> use an Administrator terminal. See
> [§3 Platform Requirements](#3-platform-requirements).

## 2. Pre-Built Binaries

Download from the
[GitHub Releases page](https://github.com/skyllc-ai/UltraFastFileSearch/releases/latest).

Each release includes:

- **Zip bundles (recommended):** `uffs-<platform>.zip`, where `<platform>` is
  `windows-x64`, `macos-arm64` or `linux-x64`. Each holds every program in one
  folder. A smaller `uffs-<platform>-min.zip` and a larger
  `uffs-<platform>-full.zip` (with extra diagnostic tools) are also published.
- **Single programs:** `uffs`, `uffsd`, `uffsmcp`, `uffs-mft`, `uffs-update`
  (and `uffs-broker` and `uffs-watchdog` on Windows), each named for its platform, for example
  `uffs-windows-x64.exe`, `uffsd-macos-arm64` or `uffsmcp-linux-x64`.
- `CHECKSUMS.txt` and `SHA256SUMS`: SHA-256 hashes of every file.
- SBOM files (software bills of materials).

`uffs` alone cannot search: it starts the `uffsd` daemon and the `uffsmcp`
MCP server from the same folder (or from your PATH). Keep the programs
together, which the zip does for you.

| Program | What it is |
|---------|------------|
| `uffs` | The main CLI; starts the sibling `uffsd` / `uffsmcp` when needed |
| `uffsd` | The daemon that holds the index |
| `uffsmcp` | The MCP server for AI agents |
| `uffs-mft` | Low-level MFT tool |
| `uffs-update` | The helper `uffs --update` uses |
| `uffs-broker` | Windows only: the Access Broker service |
| `uffs-watchdog` | Windows only: the supervisor `uffs --daemon resident on` starts |

### Quick Setup (Windows)

**Option A — Using `gh` CLI (recommended for developers):**

```powershell
# Install gh CLI if needed: winget install GitHub.cli
# Then from the repo directory:
just use
```

This downloads the latest release binaries and installs them to `~/bin`.

**Option B — Manual download:**

1. Download `uffs-windows-x64.zip` from the
   [latest release](https://github.com/skyllc-ai/UltraFastFileSearch/releases/latest)
   and extract it. You get a folder `uffs-windows-x64` with `uffs.exe`,
   `uffsd.exe`, `uffsmcp.exe` and the other programs.

2. Put that folder's programs in a directory on your PATH (see §4).

3. Install the Access Broker once from an elevated terminal
   (`uffs-broker --install`), or open a terminal **as Administrator**
   (required for live NTFS access, see §3).

4. Run your first search:

   ```powershell
   uffs "*.txt" --limit 5
   ```

   The daemon starts automatically.  Every search after this is instant.

5. To start the MCP server for AI agents:

   ```powershell
   uffs --mcp start
   ```

   To connect Claude, Cursor, Windsurf and other AI agents, see
   [MCP Server §3](mcp.md#3-host-configuration).

### Verify a download

Each release lists the SHA-256 hash of every file in `CHECKSUMS.txt` (and the
same list as `SHA256SUMS`). Compare the hash of your download with its line:

```powershell
# PowerShell
Get-FileHash uffs-windows-x64.zip -Algorithm SHA256
# Compare with the line for this file in CHECKSUMS.txt
```

```bash
# In the folder that holds the download and SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing          # Linux
shasum -a 256 -c SHA256SUMS --ignore-missing      # macOS
```

Every release file also carries signed SLSA build provenance, which proves it
was built by the project's release workflow from the published source. Check it
with the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify uffs-windows-x64.zip --owner skyllc-ai
```

## 3. Platform Requirements

| Platform | Data source | Privileges |
|----------|------------|------------|
| **Windows** | Live NTFS MFT (auto-detected) | Admin **once** (Access Broker) → then none; or an Administrator terminal |
| **macOS / Linux** | Offline MFT captures (`.iocp`, `.bin`, `.mft`) | None |

### Windows

The pre-built binary reads NTFS drives directly.  The MFT is a protected
system structure, so reading it needs elevation — but you grant it **once**:

```powershell
# Option A (recommended): install the Access Broker — one-time elevation, then
# NO UAC on any later search, daemon start or daemon stop.
uffs-broker --install          # run once from an elevated PowerShell

# Option B: run your terminal as Administrator each time
# Right-click Terminal → "Run as administrator"

# Option C: per-command elevation with gsudo
gsudo uffs "*.dll"
```

With the broker installed, plain `uffs <pattern>` and `uffs --daemon …` run
unelevated — no prompts. The broker is a `LocalSystem` service that vends the
daemon a read-only volume handle, so the daemon itself never needs admin.
Updating the broker itself needs one elevation; see [Updating](updating.md).

### macOS / Linux

macOS and Linux cannot read NTFS drives directly.  You need offline
MFT captures exported from a Windows machine, pointed at via
`--data-dir`:

```bash
uffs "*.txt" --data-dir ~/uffs_data --limit 5
```

See [Cache & Data Sources](cache-and-data.md) for how to set up the
`drive_c/`, `drive_d/` directory structure.

## 4. Add to PATH

WinGet and the macOS/Linux installer do this for you. For a manual download:

### Windows (PowerShell)

Use a folder of your own, such as `%USERPROFILE%\bin`. Do not use
`%LOCALAPPDATA%\uffs`: that is UFFS's data folder, and `uffs --uninstall`
deletes it.

```powershell
# Copy every program from the extracted zip to a permanent location
$uffsDir = "$env:USERPROFILE\bin"
New-Item -ItemType Directory -Force -Path $uffsDir
Copy-Item .\uffs-windows-x64\*.exe $uffsDir

# Add to user PATH (persists across sessions)
[Environment]::SetEnvironmentVariable("Path",
    "$uffsDir;$([Environment]::GetEnvironmentVariable('Path', 'User'))", "User")
```

After restarting your terminal, `uffs` is available everywhere:

```powershell
uffs --version
uffs "*.txt" --limit 5
```

### macOS / Linux

Copy all the programs into one directory on your PATH, for example
`~/.local/bin`:

```bash
mkdir -p ~/.local/bin
cp uffs-macos-arm64/* ~/.local/bin/     # or uffs-linux-x64/*
```

If you built from source (§5), link every program, not just `uffs`: `uffs`
looks for `uffsd` and `uffsmcp` next to itself or on your PATH.

```bash
for bin in uffs uffsd uffsmcp uffs-mft uffs-update; do
  ln -sf "$(pwd)/target/release/$bin" ~/.local/bin/$bin
done
```

## 5. Build from Source

Building from source is for development and contributing, or for a platform
without a prebuilt binary (such as an Intel Mac or Linux on ARM64). Release
binaries are built natively on each platform; developer notes on
cross-compiling are in [CONTRIBUTING.md](../../CONTRIBUTING.md).

### Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| **Rust** | The nightly pinned in `rust-toolchain.toml` | Installed automatically by `rustup` on the first build |
| **just** | 1.0+ | Task runner — `cargo install just` |
| **cargo-nextest** | Latest | Test runner — `cargo install cargo-nextest` |
| **Git** | Any | To clone the repository |

### Build

```bash
git clone https://github.com/skyllc-ai/UltraFastFileSearch.git
cd UltraFastFileSearch

# Build release binaries (Rust nightly installs automatically)
cargo build --release
```

The programs land in `target/release/`:

| Binary | Purpose |
|--------|---------|
| `uffs` | CLI search tool |
| `uffsd` | The daemon |
| `uffsmcp` | The MCP server |
| `uffs-mft` | Low-level MFT tool |
| `uffs-update` | The update helper |
| `uffs-broker` | Windows only: the Access Broker service |
| `uffs-watchdog` | Windows only: the residency supervisor |

A few more programs (developer and diagnostic tools) are built too.

### Using just (recommended)

```bash
just build          # Dev (debug) build of the workspace
just build-release  # Release build of the workspace
just use            # Install binaries to ~/bin from the latest GitHub Release
just use-local      # Build workspace from source and install to ~/bin (dev)
just check          # File-size policy + cargo check + format check (no clippy, no tests)
just go             # Full validation lane
just test           # Run all tests with nextest
```

`just use` is the easiest way to get going on any OS — it downloads
the Release bundle that GitHub Actions built for your platform and
installs the binaries to `~/bin`.  Those bytes are identical to what
every other end user runs, so bug reports are reproducible.

`just use-local` is the dev-loop variant — it builds the workspace in
release mode and installs the just-built binaries to `~/bin`.  Use this
when you want to test local changes before opening a PR.

Because it replaces binaries that may be **running**, it manages the
service lifecycle around the copy and puts things back as it found
them:

* **Unchanged binaries are skipped.**  Only files whose contents
  actually differ are replaced, so a rebuild that changed one crate
  does not disturb the other running processes — and cannot fail with
  `os error 32` (file in use) for no reason.
* **The Access Broker is a service**, so it cannot simply be
  overwritten: if its binary changed, the installer stops the service,
  replaces it, and starts it again (`⏸️ stopping` / `▶️ restarted`).
* **The watchdog is stopped first and restarted last.**  If it kept
  running during the install it would dutifully restart the daemon
  mid-teardown — the supervisor fighting the installer.
* **AI-host MCP sessions survive.**  Teardown force-kills only the
  daemon and the HTTP gateway *by PID*, never `uffsmcp` by image name —
  that would also kill the stdio supervisors your AI hosts spawned, and
  a session killed mid-request looks exactly like a hung tool call.  A
  binary that is locked by a running process is **renamed aside** rather
  than deleted, so the new one lands at the canonical path and the
  [stdio supervisor](mcp.md#zero-downtime-upgrades-stdio-supervisor)
  hot-swaps its worker without dropping the host's connection.  The
  renamed `*.oldN` sidecars are swept by the next install, once the old
  process has exited.
* **The daemon and MCP server are restarted only if they were running
  before**, and the daemon comes back through the normal start path, so
  a resident daemon returns resident (`--no-retire`).

The net effect is that `use-local` leaves the machine in the state it
found it, which is what [residency](daemon.md#permanent-residency-start-at-login-never-retire)
promises.

If `~/bin` is not on your PATH, either recipe prints the line to add
to your shell profile.

Run `just` with no arguments to see all available tasks.

## 6. Verify Installation

```bash
# Check version
uffs --version

# Show help
uffs --help

# Run a test search (Windows — live NTFS)
uffs "*.txt" --limit 5

# Run a test search (macOS/Linux — offline MFT files)
uffs "*.txt" --data-dir ~/uffs_data --limit 5
```

If the search returns results, UFFS is working.  The daemon starts
automatically on first search — see [Daemon](daemon.md) for details.

## See also

- [Getting Started](getting-started.md): your first search and everyday recipes
- [CLI Overview](cli-overview.md): every flag and command at a glance
- [Daemon](daemon.md): the resident index server and how to manage it
- [Cache & Data Sources](cache-and-data.md): live drives, MFT captures and the index cache
- [Updating](updating.md): keep UFFS current with `uffs --update`
- [Uninstalling](uninstall.md): remove UFFS and its data
