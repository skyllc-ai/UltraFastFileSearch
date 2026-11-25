# Installation

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

Pre-built binaries for all platforms are also available directly from
[GitHub Releases](https://github.com/skyllc-ai/UltraFastFileSearch/releases) (§2),
and you can always build from source (§5). Most users do not need to.

## 1. WinGet (Windows: Recommended)

If you have the [Windows Package Manager](https://learn.microsoft.com/windows/package-manager/)
(bundled with Windows 11 and modern Windows 10), install in one command:

```powershell
winget install SkyLLC.UFFS
```

This installs the `uffs` CLI (daemon + MCP + MFT tools) and puts it on
your PATH automatically.  Upgrade later with:

```powershell
winget upgrade SkyLLC.UFFS
```

Confirm the install:

```powershell
uffs --version
```

> Live NTFS search needs elevation **once**: install the Access Broker
> (`uffs-broker --install`, one-time from an elevated terminal) and every later
> search — plus daemon start/stop and non-elevated updates — runs with **no
> UAC**. Without the broker, use an Administrator terminal. See
> [§3 Platform Requirements](#3-platform-requirements).

## 2. Pre-Built Binaries

Download the latest Windows x64 binaries from the
[GitHub Releases page](https://github.com/skyllc-ai/UltraFastFileSearch/releases/latest).

Each release includes:
- `uffs-windows-x64.exe` — main CLI (starts daemon + MCP)
- `uffs-mft-windows-x64.exe` — low-level MFT tool
- `CHECKSUMS.txt` — SHA-256 hashes for verification

### Quick Setup (Windows)

**Option A — Using `gh` CLI (recommended for developers):**

```powershell
# Install gh CLI if needed: winget install GitHub.cli
# Then from the repo directory:
just use
```

This downloads the latest release binaries and installs them to `~/bin`.

**Option B — Manual download:**

1. Download `uffs-windows-x64.exe` from the
   [latest release](https://github.com/skyllc-ai/UltraFastFileSearch/releases/latest).

2. Copy to a directory on your PATH and **rename to `uffs.exe`**.

3. Open a terminal **as Administrator** (required for live NTFS access).

4. Run your first search:

   ```powershell
   uffs "*.txt" --limit 5
   ```

   The daemon starts automatically.  Every search after this is instant.

5. To start the MCP server for AI agents:

   ```powershell
   uffs --mcp start
   ```

   Run `uffs --mcp --help` for instructions on how to configure
   Claude, Cursor, Windsurf, and other AI agents to connect to the
   MCP server.

### Verify Checksums

Each release includes `CHECKSUMS.txt` with SHA-256 hashes:

```powershell
# PowerShell
Get-FileHash uffs-windows-x64.exe -Algorithm SHA256
# Compare with CHECKSUMS.txt from the release
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
# NO UAC on any later search, daemon start/stop, or non-elevated update.
uffs-broker --install          # run once from an elevated PowerShell

# Option B: run your terminal as Administrator each time
# Right-click Terminal → "Run as administrator"

# Option C: per-command elevation with gsudo
gsudo uffs "*.dll"
```

With the broker installed, plain `uffs <pattern>` and `uffs --daemon …` run
unelevated — no prompts. The broker is a `LocalSystem` service that vends the
daemon a read-only volume handle, so the daemon itself never needs admin.

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

### Windows (PowerShell)

```powershell
# Copy to a permanent location
$uffsDir = "$env:LOCALAPPDATA\uffs"
New-Item -ItemType Directory -Force -Path $uffsDir
Copy-Item dist\v0.4.105\uffs\uffs-windows-x64.exe "$uffsDir\uffs.exe"

# Add to user PATH (persists across sessions)
[Environment]::SetEnvironmentVariable("Path",
    "$uffsDir;$([Environment]::GetEnvironmentVariable('Path', 'User'))", "User")
```

After restarting your terminal, `uffs` is available everywhere:

```powershell
uffs --version
uffs "*.txt" --limit 5
```

### macOS / Linux (build from source: see §5)

```bash
ln -s "$(pwd)/target/release/uffs" /usr/local/bin/uffs
```

## 5. Build from Source

Building from source is needed for development, contributing, or
running on macOS/Linux.

> **⚠ Windows native build limitation:** Due to a Polars COFF archive
> size issue, `cargo build` does **not** currently work on Windows
> natively.  Windows binaries are cross-compiled from macOS or Linux.
> See [xwin workaround](../xwin-msvc-rlib-size-root-cause-and-workarounds.md)
> for technical details.

### Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| **Rust** | Nightly (pinned in `rust-toolchain.toml`) | Polars requires `nightly` + `simd` |
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

| Binary | Location | Purpose |
|--------|----------|---------|
| `uffs` | `target/release/uffs` | CLI search tool |
| `uffs-tui` | `target/release/uffs-tui` | Terminal UI |

### Using just (recommended)

```bash
just build       # Release build
just use         # Install binaries to ~/bin from the latest GitHub Release
just use-local   # Build workspace from source and install to ~/bin (dev)
just check       # Format + lint + build (no tests)
just go          # Full validation: format, lint, test, coverage
just test        # Run all tests with nextest
```

`just use` is the easiest way to get going on any OS — it downloads
the Release bundle that GitHub Actions built for your platform and
installs the binaries to `~/bin`.  Those bytes are identical to what
every other end user runs, so bug reports are reproducible.

`just use-local` is the dev-loop variant — it runs a full
`cargo build --release --workspace` and installs the just-built
binaries to `~/bin`.  Use this when you want to test local changes
before opening a PR.

Because it replaces binaries that may be **running**, it manages the
service lifecycle around the copy and puts things back as it found
them:

* **Unchanged binaries are skipped.**  Only files whose contents
  actually differ are replaced, so a rebuild that changed one crate
  does not disturb the other 23 processes — and cannot fail with
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

### Cross-Compiling Windows Binaries (from macOS / Linux)

This is how the release binaries are cross-compiled:

```bash
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

See [xwin workaround](../xwin-msvc-rlib-size-root-cause-and-workarounds.md)
for details on the `xwin-dev` profile and COFF archive size limits.

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
