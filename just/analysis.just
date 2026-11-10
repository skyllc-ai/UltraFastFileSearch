# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.


# Security audit.
audit:
    @printf "\033[0;34m🔒 Security audit...\033[0m\n"
    @if command -v cargo-audit >/dev/null 2>&1; then cargo audit; else printf "\033[1;33mInstalling cargo-audit...\033[0m\n"; just _install-if-missing cargo-audit cargo-audit; cargo audit; fi

# Comprehensive security audit with multiple tools.
audit-comprehensive:
    #!/usr/bin/env bash
    printf "\033[0;34m🔒 Comprehensive security audit...\033[0m\n"
    printf "\033[0;34m  → Running cargo-audit (vulnerability scan)...\033[0m\n"
    if command -v cargo-audit >/dev/null 2>&1; then cargo audit; else printf "\033[1;33m⚠️  cargo-audit not found, run 'just setup' first\033[0m\n"; fi
    printf "\033[0;34m  → Running cargo-deny (dependency analysis)...\033[0m\n"
    if command -v cargo-deny >/dev/null 2>&1; then cargo deny check; else printf "\033[1;33m⚠️  cargo-deny not found, run 'just setup' first\033[0m\n"; fi

# Audit `unsafe` / `build.rs` / proc-macro footprint across the dep tree.
#
# Why cargo-geiger (on top of cargo-audit + cargo-deny):
#
#   cargo-audit    → known CVEs in the RustSec database (advisories).
#   cargo-deny     → license policy + multiple-version / source checks.
#   cargo-geiger   → structural: how many `unsafe` blocks, how many
#                    `build.rs` scripts, how many proc-macros — the
#                    crates worth auditing *first* if you're worried
#                    about a supply-chain attack at the code level.
#
# When to run:
#
#   • Periodically (monthly) to track the trend — is the `unsafe`
#     footprint growing disproportionately to added features?
#   • Before merging a Dependabot PR that flips a crate to a new
#     major version (compare the summary before vs after).
#   • When investigating an incident — see which crates in the tree
#     execute `build.rs` (= arbitrary code at compile time).
#
# Why NOT in CI:
#
#   cargo-geiger's output goes stale fast (any dep bump changes it),
#   it occasionally breaks on fresh nightly toolchains, and the
#   information is advisory rather than binary pass/fail.  Baking it
#   into Tier 1 would produce noisy failures without real signal.
#   Keeping it as a local recipe means the dev runs it when they
#   actually want the data — and the report lives under `target/`
#   so it doesn't pollute the repo.
#
# Install once: `cargo install cargo-geiger`.  First run rebuilds
# every crate with `--release` (~3-5 min); subsequent runs hit the
# incremental cache.
geiger:
    #!/usr/bin/env bash
    printf "\033[0;34m☢️  cargo-geiger: unsafe / build.rs / proc-macro footprint\033[0m\n"
    if ! command -v cargo-geiger >/dev/null 2>&1; then
        printf "\033[1;33m⚠️  cargo-geiger not found\033[0m\n"
        printf "\033[0;34m   Install: cargo install cargo-geiger\033[0m\n"
        printf "\033[0;34m   (takes ~3 min; no special toolchain needed)\033[0m\n"
        exit 1
    fi
    mkdir -p target
    REPORT="target/geiger-$(date +%Y%m%d).txt"
    # Workspace root is a virtual manifest; cargo-geiger requires a
    # concrete package.  uffs-cli is the primary user-facing binary
    # and its dep tree is a superset of uffs-core's, so auditing it
    # covers the full attack surface that ships to end users.
    cargo geiger --manifest-path "$(pwd)/crates/uffs-cli/Cargo.toml" --all-features --update-readme=false 2>&1 | tee "$REPORT"
    printf "\033[0;32m✅ Report saved to %s\033[0m\n" "$REPORT"
    printf "\033[0;34m   Interpretation guide: docs/architecture/security/supply-chain-posture.md\033[0m\n"

# Quick summary: top crates by unsafe + build-script hotspot.
geiger-summary:
    #!/usr/bin/env bash
    if ! command -v cargo-geiger >/dev/null 2>&1; then
        printf "\033[1;33m⚠️  cargo-geiger not found; run 'cargo install cargo-geiger' first\033[0m\n"
        exit 1
    fi
    printf "\033[0;34m☢️  cargo-geiger — top hotspots\033[0m\n"
    cargo geiger --manifest-path "$(pwd)/crates/uffs-cli/Cargo.toml" --all-features --update-readme=false 2>&1 | \
        grep -E '^[[:space:]]*[0-9]+/[0-9]+' | \
        sort -t/ -k2,2 -n -r | \
        head -25


# Dependency optimization and cleanup.
deps-optimize:
    #!/usr/bin/env bash
    printf "\033[0;34m🔧 Optimizing dependencies...\033[0m\n"
    printf "\033[0;34m  → Finding unused dependencies...\033[0m\n"
    if command -v cargo-udeps >/dev/null 2>&1; then cargo udeps; else printf "\033[1;33m⚠️  cargo-udeps not found, run 'just setup' first\033[0m\n"; fi
    printf "\033[0;34m  → Checking for outdated dependencies...\033[0m\n"
    if command -v cargo-outdated >/dev/null 2>&1; then cargo outdated; else printf "\033[1;33m⚠️  cargo-outdated not found, run 'just setup' first\033[0m\n"; fi

# Advanced debugging and analysis.
debug-deep:
    #!/usr/bin/env bash
    printf "\033[0;34m🔬 Deep debugging and analysis...\033[0m\n"
    printf "\033[0;34m  → Expanding macros...\033[0m\n"
    if command -v cargo-expand >/dev/null 2>&1; then cargo expand; else printf "\033[1;33m⚠️  cargo-expand not found, run 'just setup' first\033[0m\n"; fi
    printf "\033[0;34m  → Running Miri (undefined behavior detection)...\033[0m\n"
    if rustup component list --installed | grep -q "miri"; then cargo miri test; else printf "\033[1;33m⚠️  miri component not found, run 'just setup' first\033[0m\n"; fi

# Generate flamegraph for performance analysis.
flamegraph target="uffs-cli" pattern="*.txt":
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[0;34m🔥 Generating Flamegraph for {{target}}...\033[0m\n"
    if ! command -v cargo-flamegraph >/dev/null 2>&1; then printf "\033[1;31m❌ cargo-flamegraph not found\033[0m\n"; printf "\033[0;34m   Install with: cargo install flamegraph\033[0m\n"; exit 1; fi
    printf "\033[0;34m  → Building release binary...\033[0m\n"
    cargo build --release -p {{target}} --quiet
    printf "\033[0;34m  → Generating flamegraph (this may require elevated permissions)...\033[0m\n"
    FLAMEGRAPH_OUTPUT="target/flamegraph-$(date +%Y%m%d-%H%M%S).svg"
    cargo flamegraph --bin uffs -o "$FLAMEGRAPH_OUTPUT" -- "{{pattern}}" 2>/dev/null || { printf "\033[1;33m⚠️  Flamegraph requires elevated permissions\033[0m\n"; printf "\033[0;34m   On macOS: sudo just flamegraph\033[0m\n"; printf "\033[0;34m   On Linux: ensure perf is available\033[0m\n"; exit 1; }
    printf "\033[0;32m✅ Flamegraph generated: $FLAMEGRAPH_OUTPUT\033[0m\n"
    printf "\033[0;34m   Open in browser to analyze CPU hotspots\033[0m\n"

# Profile-Guided Optimization for the uffs-cli launcher.
#
# Three-phase workflow:
#
#   1. Build an instrumented `uffs` binary (emits profraw samples on each
#      run).  The full release profile + per-package `opt-level = "z"`
#      override is still applied.
#   2. Drive the instrumented binary through a representative corpus of
#      queries (matches the cross-tool bench patterns).  Each invocation
#      appends to the profraw directory.
#   3. Merge the raw samples with `llvm-profdata` and rebuild the binary
#      with `-Cprofile-use`, which tells LLVM to lay out the hot blocks
#      contiguously and inline the hot call graph.
#
# Prerequisites:
#   - `llvm-tools-preview` rustup component:
#       rustup component add llvm-tools-preview
#   - A running UFFS daemon (`uffs daemon start`).  The corpus queries
#     will auto-spawn one if not already running.
#
# Output:
#   - `target/pgo-data/`    raw samples (discarded on each fresh run)
#   - `target/pgo-data/merged.profdata`  merged profile
#   - `target/release/uffs` PGO-optimized binary (overwrites existing)
#
# See docs/research/perf-phase2-measurement-plan.md §2.3 for expected
# gains and A/B methodology.
[unix]
pgo:
    #!/usr/bin/env bash
    set -euo pipefail
    printf "\033[1;34m🏁 PGO pipeline for uffs-cli\033[0m\n"
    printf "\033[0;34m   Three phases: instrumented build → corpus run → optimized rebuild\033[0m\n"
    echo ""

    # Locate llvm-profdata shipped with the Rust toolchain.
    LLVM_PROFDATA=""
    for candidate in \
        "$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's|host: ||p')/bin/llvm-profdata" \
        "$(command -v llvm-profdata || true)"; do
        if [[ -n "$candidate" && -x "$candidate" ]]; then
            LLVM_PROFDATA="$candidate"
            break
        fi
    done
    if [[ -z "$LLVM_PROFDATA" ]]; then
        printf "\033[1;31m❌ llvm-profdata not found\033[0m\n"
        printf "\033[0;34m   Install with: rustup component add llvm-tools-preview\033[0m\n"
        exit 1
    fi
    printf "\033[0;34m  llvm-profdata: %s\033[0m\n" "$LLVM_PROFDATA"

    # Honour CARGO_TARGET_DIR / .cargo/config.toml `build.target-dir`.
    # The workspace ships windows.toml with `target-dir = "C:/rust-target/uffs"`
    # and advises Unix users to set `CARGO_TARGET_DIR` manually — so a
    # hard-coded `target/` here fails whenever either override is live.
    TARGET_DIR="${CARGO_TARGET_DIR:-$(pwd)/target}"
    printf "\033[0;34m  target dir:    %s\033[0m\n" "$TARGET_DIR"
    PGO_DIR="${TARGET_DIR}/pgo-data"
    rm -rf "$PGO_DIR"
    mkdir -p "$PGO_DIR"

    # Phase 1: instrumented build
    printf "\n\033[0;34m🔨 Phase 1/3: building instrumented uffs-cli\033[0m\n"
    RUSTFLAGS="-Cprofile-generate=${PGO_DIR}" cargo build --release -p uffs-cli
    INSTRUMENTED="${TARGET_DIR}/release/uffs"
    if [[ ! -x "$INSTRUMENTED" ]]; then
        INSTRUMENTED="${TARGET_DIR}/release/uffs.exe"
    fi
    if [[ ! -x "$INSTRUMENTED" ]]; then
        printf "\033[1;31m❌ instrumented binary missing under %s/release/\033[0m\n" "$TARGET_DIR"
        printf "\033[0;34m   Set CARGO_TARGET_DIR or check .cargo/config.toml 'build.target-dir'.\033[0m\n"
        exit 1
    fi
    printf "\033[0;32m  ✓ instrumented binary: %s\033[0m\n" "$INSTRUMENTED"

    # Phase 2: corpus run.  Patterns match scripts/windows/cross-tool-benchmark.rs
    # so the PGO optimizer tunes for the exact queries we benchmark.
    printf "\n\033[0;34m🏃 Phase 2/3: driving corpus (10× each pattern)\033[0m\n"
    DRIVE_FLAG=""
    if [[ -n "${UFFS_PGO_DRIVE:-}" ]]; then
        DRIVE_FLAG="--drive ${UFFS_PGO_DRIVE}"
        printf "\033[0;34m  drive filter: %s\033[0m\n" "$UFFS_PGO_DRIVE"
    fi
    for pattern in "notepad.exe" "win*" "*.dbt" "*.dll" "config"; do
        printf "\033[0;34m    → %s\033[0m\n" "$pattern"
        for _ in $(seq 1 10); do
            "$INSTRUMENTED" "$pattern" $DRIVE_FLAG --columns path --out=/tmp/uffs_pgo.csv >/dev/null 2>&1 || true
        done
    done
    rm -f /tmp/uffs_pgo.csv
    printf "\033[0;32m  ✓ corpus run complete\033[0m\n"

    # Phase 3: merge + rebuild
    printf "\n\033[0;34m🧬 Phase 3/3: merging profraw and rebuilding\033[0m\n"
    MERGED="${PGO_DIR}/merged.profdata"
    "$LLVM_PROFDATA" merge -o "$MERGED" "$PGO_DIR"
    printf "\033[0;32m  ✓ merged profile: %s\033[0m\n" "$MERGED"
    RUSTFLAGS="-Cprofile-use=${MERGED}" cargo build --release -p uffs-cli
    printf "\033[0;32m✅ PGO build complete — %s/release/uffs now uses the merged profile\033[0m\n" "$TARGET_DIR"
    printf "\033[0;34m   Run 'just use-local' to install, then re-bench against the pre-PGO baseline.\033[0m\n"

# Windows variant of the PGO pipeline.  Uses PowerShell because
# `shared.just` sets `windows-shell := powershell.exe` — the bash
# recipe above would fail with "No such file or directory" under the
# default Windows shell selection.
#
# Optional env var: `$env:UFFS_PGO_DRIVE = 'D'` scopes the corpus run
# to a single drive, cutting phase 2 wall time roughly in half.  The
# daemon must already be running (`uffs daemon start`) — spawning an
# elevated daemon mid-PGO would conflict with the instrumented binary.
[windows]
pgo:
    #!powershell
    $ErrorActionPreference = 'Stop'
    Write-Host "[pgo] UFFS-CLI Profile-Guided Optimization" -ForegroundColor Blue
    Write-Host "      Three phases: instrumented build -> corpus run -> optimized rebuild`n"

    # ── Locate llvm-profdata ──────────────────────────────────────
    $sysroot = (& rustc --print sysroot).Trim()
    $hostTriple = ((& rustc -vV) | Select-String -Pattern '^host: (.+)$').Matches.Groups[1].Value
    $candidate = Join-Path $sysroot "lib\rustlib\$hostTriple\bin\llvm-profdata.exe"
    if (Test-Path $candidate) {
        $LLVM_PROFDATA = $candidate
    } elseif (Get-Command llvm-profdata.exe -ErrorAction SilentlyContinue) {
        $LLVM_PROFDATA = (Get-Command llvm-profdata.exe).Source
    } else {
        Write-Host "[pgo] llvm-profdata not found" -ForegroundColor Red
        Write-Host "      Install with: rustup component add llvm-tools-preview" -ForegroundColor DarkGray
        exit 1
    }
    Write-Host "  llvm-profdata: $LLVM_PROFDATA" -ForegroundColor DarkGray

    # Honour CARGO_TARGET_DIR / .cargo/config.toml `build.target-dir`.
    # The workspace ships windows.toml with
    # `target-dir = "C:/rust-target/uffs"` and the primary config.toml
    # documents the CARGO_TARGET_DIR override, so a hard-coded
    # `target\` here breaks on any developer who applies either
    # mechanism.  Prefer the env var so we pick up values set in the
    # user's PowerShell profile without reading the cargo config.
    if ($env:CARGO_TARGET_DIR) {
        $TARGET_DIR = $env:CARGO_TARGET_DIR
    } else {
        $TARGET_DIR = Join-Path (Get-Location) 'target'
    }
    Write-Host "  target dir:    $TARGET_DIR" -ForegroundColor DarkGray

    $PGO_DIR = Join-Path $TARGET_DIR 'pgo-data'
    if (Test-Path $PGO_DIR) { Remove-Item -Recurse -Force $PGO_DIR }
    New-Item -ItemType Directory -Path $PGO_DIR | Out-Null

    # ── Phase 1: instrumented build ───────────────────────────────
    Write-Host "`n[pgo 1/3] Building instrumented uffs-cli" -ForegroundColor Blue
    $env:RUSTFLAGS = "-Cprofile-generate=$PGO_DIR"
    try {
        cargo build --release -p uffs-cli
        if ($LASTEXITCODE -ne 0) { throw "instrumented build failed (exit $LASTEXITCODE)" }
    } finally {
        Remove-Item Env:RUSTFLAGS
    }
    $INSTRUMENTED = Join-Path $TARGET_DIR 'release\uffs.exe'
    if (-not (Test-Path $INSTRUMENTED)) {
        Write-Host "[pgo] instrumented binary missing: $INSTRUMENTED" -ForegroundColor Red
        Write-Host "      Set CARGO_TARGET_DIR or check .cargo/config.toml build.target-dir." -ForegroundColor DarkGray
        exit 1
    }
    Write-Host "  ok: $INSTRUMENTED" -ForegroundColor Green

    # ── Phase 2: corpus run ───────────────────────────────────────
    # Patterns match scripts/windows/cross-tool-benchmark.rs so the
    # PGO optimiser tunes for the exact queries we benchmark.
    Write-Host "`n[pgo 2/3] Driving corpus (10x each pattern)" -ForegroundColor Blue
    $driveArgs = @()
    if ($env:UFFS_PGO_DRIVE) {
        $driveArgs = @('--drive', $env:UFFS_PGO_DRIVE)
        Write-Host "  drive filter: $($env:UFFS_PGO_DRIVE)" -ForegroundColor DarkGray
    }
    $tmpOut = Join-Path $env:TEMP 'uffs_pgo.csv'
    foreach ($pattern in @('notepad.exe', 'win*', '*.dbt', '*.dll', 'config')) {
        Write-Host "    -> $pattern" -ForegroundColor DarkGray
        for ($i = 0; $i -lt 10; $i++) {
            & $INSTRUMENTED $pattern @driveArgs --columns path "--out=$tmpOut" `
                *> $null
        }
    }
    if (Test-Path $tmpOut) { Remove-Item $tmpOut }
    Write-Host "  corpus run complete" -ForegroundColor Green

    # ── Phase 3: merge + rebuild ──────────────────────────────────
    Write-Host "`n[pgo 3/3] Merging profraw + optimised rebuild" -ForegroundColor Blue
    $MERGED = Join-Path $PGO_DIR 'merged.profdata'
    & $LLVM_PROFDATA merge -o $MERGED $PGO_DIR
    if ($LASTEXITCODE -ne 0) { throw "llvm-profdata merge failed (exit $LASTEXITCODE)" }
    Write-Host "  merged: $MERGED" -ForegroundColor Green

    $env:RUSTFLAGS = "-Cprofile-use=$MERGED"
    try {
        cargo build --release -p uffs-cli
        if ($LASTEXITCODE -ne 0) { throw "optimised rebuild failed (exit $LASTEXITCODE)" }
    } finally {
        Remove-Item Env:RUSTFLAGS
    }
    Write-Host "`n[pgo] complete -- $INSTRUMENTED now uses the merged profile" -ForegroundColor Green
    Write-Host "      Run 'just use-local' to install, then re-bench against the pre-PGO baseline." -ForegroundColor DarkGray

