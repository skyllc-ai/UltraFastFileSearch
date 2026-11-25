# Build System & CI

## Introduction

This document describes how UFFS is built, tested, and distributed. After reading this document, you should be able to:

1. Build UFFS from source on Windows, macOS, and Linux
2. Understand the Cargo workspace structure and profiles
3. Cross-compile from macOS to Windows
4. Run tests and benchmarks

---

## Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| **Rust** | 1.91+ | Edition 2024, nightly required |
| **Cargo** | Latest | Workspace resolver v2 |
| **Windows SDK** | 10.0+ | For Windows API bindings |
| **Git LFS** | Any | For test fixtures |

### Recommended Tools

| Tool | Purpose |
|------|---------|
| `cargo-nextest` | Faster test runner |
| `cargo-xwin` | macOS → Windows cross-compilation |
| `cargo-flamegraph` | Performance profiling |
| `just` | Task runner (justfile recipes) |
| `cargo-deny` | Dependency audit |
| `cargo-dist` | Binary distribution |

---

## Workspace Structure

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = [
    "crates/uffs-polars",   # Polars facade (compilation isolation)
    "crates/uffs-mft",      # MFT reading → Polars DataFrame
    "crates/uffs-core",     # Query engine using Polars lazy API
    "crates/uffs-cli",      # Command-line interface
    "crates/uffs-tui",      # Terminal UI
    "crates/uffs-gui",      # Graphical UI (future)
    "crates/uffs-diag",     # Diagnostic tools (not shipped)
]
```

### Crate Dependency Chain

```
uffs-cli ──► uffs-core ──► uffs-mft ──► uffs-polars ──► polars (git)
                                    └──► windows (0.62)
                                    └──► tokio, rayon, zerocopy
```

### Key Dependencies

| Dependency | Version | Purpose |
|------------|---------|---------|
| `windows` | 0.62.2 | Windows API bindings (IOCP, volume access) |
| `tokio` | 1.50.0 | Async runtime (multi-drive orchestration) |
| `rayon` | 1.11.0 | Parallel parsing on NVMe |
| `polars` | git main | DataFrame analytics and Parquet I/O |
| `clap` | 4.6.0 | CLI argument parsing |
| `zerocopy` | 0.8 | Zero-copy NTFS structure parsing |
| `mimalloc` | 0.1.48 | Global memory allocator |
| `regex` | 1.12.3 | Regex pattern matching |
| `aho-corasick` | 1.1.4 | Multi-pattern string matching |
| `globset` | 0.4.18 | Glob pattern compilation |
| `tracing` | 0.1.44 | Structured logging |
| `chrono` | 0.4.44 | Timestamp formatting |
| `criterion` | 0.8.2 | Benchmarking framework |

---

## Build Profiles

### Development

```bash
cargo build                    # Debug build (fast compile, slow runtime)
cargo build --profile debug-optimized  # Debug with opt-level=2
```

### Release

```bash
cargo build --release          # Production build
```

Release profile settings:
```toml
[profile.release]
opt-level = 3
lto = "fat"           # Full link-time optimization
codegen-units = 1     # Single codegen unit for maximum optimization
panic = "abort"       # Smaller binary, no unwinding
strip = "symbols"     # Strip debug symbols
```

### Profiling

```bash
cargo build --profile profiling
```

```toml
[profile.profiling]
inherits = "release"
debug = true          # Debug symbols for flamegraph
strip = false         # Keep symbols
lto = false           # Disable LTO for cleaner call stacks
codegen-units = 16    # Balance compile time and stack clarity
```

### Cross-Compilation (macOS → Windows)

```bash
cargo xwin build --profile xwin-dev --target x86_64-pc-windows-msvc
```

```toml
[profile.xwin-dev]
inherits = "dev"
debug = 2
incremental = false   # Required: incremental breaks xwin archives

# Tame polars to keep .rlib under COFF limits
[profile.xwin-dev.package.polars]
debug = 1
opt-level = 1
codegen-units = 1
```

**Note:** Polars debug builds produce ~5.5 GB `.rlib` files that exceed COFF archive limits. The `xwin-dev` profile reduces Polars debug info to work around this.

---

## Linting Configuration

UFFS uses **extremely strict** clippy settings:

```toml
[workspace.lints.clippy]
# Core groups at strictest levels
cargo = { level = "deny", priority = -1 }
nursery = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }

# DENY-level (no exceptions)
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
missing_docs_in_private_items = "deny"
undocumented_unsafe_blocks = "deny"

# WARN-level (200+ additional lints)
# See workspace Cargo.toml for complete list
```

Rust compiler lints:
```toml
[workspace.lints.rust]
unsafe_code = "deny"
missing_docs = "warn"
future_incompatible = { level = "deny", priority = -1 }
```

---

## Testing

### Unit Tests

```bash
cargo test                     # All workspace tests
cargo test -p uffs-mft         # MFT crate only
cargo test -p uffs-core        # Core crate only
cargo nextest run              # Faster parallel test runner
```

### Test Configuration

```toml
# .config/nextest.toml
[profile.default]
retries = 0
slow-timeout = { period = "60s", terminate-after = 2 }
```

### Key Test Modules

| Test Module | Tests |
|------------|-------|
| `index/tests_core.rs` | FileRecord creation, FRS lookup |
| `index/tests_children.rs` | Parent-child relationships |
| `index/tests_extensions.rs` | Extension record merging |
| `index/tests_merge.rs` | Fragment merging correctness |
| `index/tests_tree.rs` | Tree metrics computation |
| `parse/tests.rs` | MFT record parsing from raw bytes |
| `ntfs/tests.rs` | NTFS structure parsing, USA fixup |
| `index_search/tests.rs` | Pattern matching correctness |

### Regression Testing

UFFS includes output comparison scripts for verifying correctness across versions:

```bash
# On Windows:
cargo run --release -- * --drive C > output_current.csv
# Compare against a known-good baseline
python compare_outputs.py output_baseline.csv output_current.csv
```

### Chaos Testing

`uffs-mft`'s `ChaosMftReader` parses an offline MFT with its chunks in a
seeded random, reversed or interleaved order, and the chaos tests require
the same records as a sequential parse (the `uffs` command line has no
chaos flag):

```bash
UFFS_MFT_TEST_FILE=/path/to/D_mft.bin \
  cargo test -p uffs-mft --lib -- chaos_order --ignored --nocapture
```

---

## CI Pipeline

### GitHub Actions (`ci.yml`)

The CI pipeline runs on every push and PR:

```yaml
# .github/workflows/ci.yml
jobs:
  check:
    # cargo check, clippy, rustfmt
  test:
    # cargo nextest on Linux, macOS, Windows
  build:
    # Release build on all targets
```

### Supported Targets

| Target | Platform | Notes |
|--------|----------|-------|
| `x86_64-pc-windows-msvc` | Windows x64 | Primary target |
| `x86_64-unknown-linux-gnu` | Linux x64 | CI + offline MFT |
| `aarch64-unknown-linux-gnu` | Linux ARM64 | CI + offline MFT |
| `x86_64-apple-darwin` | macOS Intel | Development |
| `aarch64-apple-darwin` | macOS Apple Silicon | Development |

### Distribution (`cargo-dist`)

Binary releases are built and published via `cargo-dist`:

```toml
[workspace.metadata.dist]
cargo-dist-version = "0.30.0"
ci = ["github"]
installers = ["shell", "powershell"]
```

---

## Justfile Recipes

UFFS uses `just` for common development tasks:

```bash
just build                     # Build release
just test                      # Run all tests
just bench                     # Run benchmarks
just lint                      # Run clippy + rustfmt
just check                     # cargo check all targets
```

Recipe files are organized under `just/`:

| File | Recipes |
|------|---------|
| `bench_uffs.just` | MFT reading benchmarks |
| `analysis.just` | Code analysis tasks |
| `analysis_ci.just` | CI-specific analysis |

---

## Unsafe Code Safety

### Workspace Posture

UFFS enforces a strict **deny-by-default** policy on unsafe code:

```toml
# Cargo.toml — workspace-wide
[workspace.lints.rust]
unsafe_code = "deny"
```

Any module that requires `unsafe` must explicitly opt in with a scoped `#[expect(unsafe_code, reason = "...")]` annotation. The `uffs-core` crate goes further with `#![forbid(unsafe_code)]` — the query engine, pattern matching, and output formatting contain **zero unsafe code**.

### Why Unsafe Is Needed

UFFS reads the NTFS Master File Table by directly interfacing with the Windows kernel via raw Win32 APIs. These are Foreign Function Interface (FFI) calls that Rust cannot verify at compile time — they require `unsafe` by definition. There is no safe alternative for:

1. **Volume handle management** — `CreateFileW` / `CloseHandle` for raw device access
2. **Device I/O control** — `DeviceIoControl` for `FSCTL_GET_NTFS_VOLUME_DATA`, `FSCTL_GET_RETRIEVAL_POINTERS`
3. **Overlapped I/O** — `ReadFile` with `OVERLAPPED` structures and `GetQueuedCompletionStatus` for IOCP
4. **I/O Completion Ports** — `CreateIoCompletionPort` for async I/O orchestration
5. **USN Journal access** — `DeviceIoControl` with `FSCTL_READ_USN_JOURNAL`
6. **Privilege checking** — `OpenProcessToken` / `GetTokenInformation` for elevation detection

### Where Unsafe Lives

All unsafe code is concentrated in **three Windows-only modules** within `uffs-mft`:

| Module | Unsafe Operations | Scope |
|--------|-------------------|-------|
| **`platform/volume.rs`** | `CreateFileW`, `CloseHandle`, `DeviceIoControl`, `GetVolumeInformationW` | Volume handle RAII, volume metadata retrieval |
| **`platform/system.rs`** | `OpenProcessToken`, `GetTokenInformation`, WMI COM calls | Privilege checking, drive type detection |
| **`io/readers/iocp/`** | `ReadFile` (overlapped), `CreateIoCompletionPort`, `GetQueuedCompletionStatus` | Async I/O read pipeline |
| **`io/readers/parallel/`** | `ReadFile`, `SetFilePointerEx` | Sequential/parallel read operations |
| **`usn.rs`** | `DeviceIoControl` for USN journal | Change journal reading |

**Not in the unsafe surface:**
- All NTFS structure parsing (`ntfs/`, `parse/`) — pure safe Rust using `zerocopy`
- All index building (`index/`) — safe Rust data structures
- All pattern matching and search (`uffs-core`) — `#![forbid(unsafe_code)]`
- All CLI and output formatting (`uffs-cli`) — safe Rust

### How Unsafe Is Kept Safe

Each unsafe block follows strict discipline:

**1. One operation per unsafe block:**
```rust
#[expect(unsafe_code, reason = "FFI: CreateIoCompletionPort to create IOCP handle")]
pub fn new(concurrency: u32) -> Result<Self> {
    // SAFETY: Creates a new completion port with no associated file handle.
    let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, concurrency) };
    // Immediately convert to Result — safe from here on
    match handle {
        Ok(h) => Ok(Self { handle: h }),
        Err(e) => Err(MftError::Io(...)),
    }
}
```

**2. RAII wrappers for handles:**
All Win32 handles are wrapped in Rust types with `Drop` implementations that call `CloseHandle`. No manual cleanup, no leaks:
```rust
impl Drop for IoCompletionPort {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}
```

**3. Pinned memory for OVERLAPPED:**
The `OverlappedRead` structure is pinned (`Pin<Box<>>`) because Windows holds a kernel pointer to the `OVERLAPPED` field until I/O completes. Moving it would cause undefined behavior.

**4. Bounds checking before all buffer access:**
All NTFS record parsing uses safe Rust slice operations with bounds checks. The `zerocopy` crate provides zero-copy deserialization with compile-time layout verification — no `unsafe` pointer casts.

**5. Immediate error conversion:**
Every FFI call's return value is checked and immediately converted to a typed `MftError`. No error codes are silently ignored.

### Unsafe Audit Summary

| Metric | Value |
|--------|-------|
| **Crates with unsafe** | 1 (`uffs-mft`) |
| **Crates with `forbid(unsafe_code)`** | 1 (`uffs-core`) |
| **Modules with unsafe blocks** | ~6 (all Windows-only, all FFI) |
| **Unsafe operations** | Win32 FFI calls only — no pointer arithmetic, no transmute, no manual memory management |
| **Cross-platform code with unsafe** | 0 — all NTFS parsing, indexing, and search is safe Rust |

---

## Code Coverage

```toml
# .config/coverage.toml
# Configuration for cargo-llvm-cov or tarpaulin
```

Coverage can be generated with:

```bash
cargo llvm-cov --workspace
```

---

*Document Version: 2.0*
*Last Updated: 2026-04-12*
*UFFS Version: 0.4.106*
