# UFFS Workspace Crate Graph and Layering

**Audience:** Contributors deciding where to put new code, which crate to depend on, or whether to extract a new crate.

**Scope:** The 18-member UFFS workspace (`/Cargo.toml::[workspace.members]`).  This document defines the workspace's **crate-level** architecture — module layout *within* a crate is Phase-3 / #190 territory.

**Source of truth for:**

- Which crates exist, why, and what each owns
- Allowed dependency directions (Layer N → Layer M)
- Async / unsafe / FFI permissions per crate
- Public-vs-internal partition (publishable to crates.io vs internal tooling)

**Cross-references:**

- Engine functionality details: `docs/architecture/engine/01-overview.md`
- Publishable subset rationale: `docs/architecture/release-automation-baseline.md` §10 row 5
- F5 protocol-extraction decision: `docs/dev/baseline/2026-05-12/f5_broker_test_coverage_decision.md` (local-only)

---

## 1. Layer model

UFFS uses a strict layered architecture with **5 layers** plus a parallel **tooling tree**.  Dependencies flow exclusively downward (higher layer → lower layer).  No same-layer or upward arrows are permitted.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ Layer 4 — Apps (binaries)                                                   │
│   uffs-daemon   uffs-cli   uffs-mcp                                         │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Layer 3 — Application logic (libraries consumed only by apps)               │
│   uffs-core   uffs-client                                                   │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Layer 2 — Domain composition (libraries that compose Layer-1 primitives)    │
│   uffs-format   uffs-diag                                                   │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Layer 1 — Domain primitives (libraries that wrap Layer-0 with domain logic) │
│   uffs-mft   uffs-broker                                                    │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Layer 0 — Foundation (zero internal deps; only external crates)             │
│   uffs-polars  uffs-security  uffs-text  uffs-time  uffs-broker-protocol     │
│   uffs-winsvc  uffs-fetch                                                    │
└─────────────────────────────────────────────────────────────────────────────┘

┌──── Parallel tree (not part of the layer hierarchy) ────┐
│ Tooling (workspace orchestration / diagnostics)         │
│   uffs-ci-pipeline   uffs-gen-hooks   uffs-gen-workflow │
└─────────────────────────────────────────────────────────┘
```

## 2. Per-layer crate inventory

### Layer 0 — Foundation (7 crates, all publishable)

| Crate | Description | External-dep footprint |
|---|---|---|
| `uffs-polars` | Polars facade for compilation isolation | Polars (git-pinned); deliberately a thin re-export to keep Polars rebuild costs scoped |
| `uffs-security` | Crypto, keystore, secure FS ops, FILE_FLAG_RANDOM_ACCESS handling | Windows DPAPI / DACL, libc flock, memmap2 |
| `uffs-text` | Unicode/NTFS case folding, trigram keys, i18n primitives | Pure logic; zero unsafe |
| `uffs-time` | NTFS FILETIME arithmetic (`const fn`) | Pure logic; zero deps |
| `uffs-broker-protocol` | Cross-platform broker wire-protocol types (`PIPE_NAME`, `SERVICE_NAME`, the Snapshot Manager messages, the VSS `HRESULT` retry table in `vss_hresult`) | Pure logic; zero unsafe |
| `uffs-winsvc` | Native Windows service control (SCM) + broker-pipe readiness probe; the single home for the `sc`/SCM mechanics shared by uffs-broker, uffs-update, uffs-cli | `windows` (windows-target only); pure stubs off Windows |
| `uffs-fetch` | Hardened release-asset transport: GitHub release lookup, streaming any-URL download (retry, connect/inactivity timeouts, caller-chosen byte cap, caller-supplied user-agent), `SHA256SUMS` verification. Extracted from `uffs-update` for reuse by external products; keeps the HTTP/TLS stack out of the lean `uffs` CLI | reqwest (blocking, rustls-tls-native-roots), sha2, hex; cross-platform, zero unsafe |

**Layer-0 contract:** Zero internal-crate dependencies.  Any new Layer-0 crate must compile against `cargo check -p <crate>` with no `uffs-*` deps in `[dependencies]`.

### Layer 1 — Domain primitives (2 crates, both non-publishable)

| Crate | Description | Internal deps | External-dep footprint |
|---|---|---|---|
| `uffs-mft` | NTFS MFT reading library (Win32 IOCP, USA fixup, attribute parsing) | uffs-polars, uffs-security, uffs-text | Tokio (limited), Windows FFI |
| `uffs-broker` | Windows elevated-handle vendor service (bin-only) | uffs-broker-protocol | Windows FFI (named pipes, security descriptors) |

**Layer-1 contract:** Allowed to depend on any subset of Layer 0.  Owns one **specific NTFS subsystem** each.  `uffs-mft` owns "raw MFT bytes → typed records → Parquet snapshot"; `uffs-broker` owns "Windows-only privileged handle vending".

### Layer 2 — Domain composition (2 crates, both non-publishable)

| Crate | Description | Internal deps |
|---|---|---|
| `uffs-format` | Shared CSV/columnar formatter (used by daemon + thin CLI for byte-identical output) | uffs-mft (single function), uffs-time |
| `uffs-diag` | Multi-binary diagnostic tools for MFT analysis (forensics / parity-validation suite; not shipped in dist) | uffs-mft, uffs-polars |

**Layer-2 contract:** Composes Layer-1 primitives into reusable middle-layer logic without exposing the underlying Win32 / Parquet machinery.  `uffs-format`'s `FormatRow` trait is the boundary that lets higher layers feed any row-shaped type into the canonical formatter without depending on `uffs-mft`'s record types.

### Layer 3 — Application logic (2 crates, both non-publishable)

| Crate | Description | Internal deps |
|---|---|---|
| `uffs-core` | Query engine (pattern compilation, index search, path resolution, aggregation, compact storage) | uffs-format, uffs-mft, uffs-polars, uffs-security, uffs-text, uffs-time |
| `uffs-client` | Thin client library (IPC + connection + query lifecycle) | uffs-format, uffs-security |

**Layer-3 contract:** Application-facing libraries.  Allowed to compose all lower layers.  Single consumer pattern: only Layer-4 apps depend on Layer-3 crates.  `uffs-core` has 1 consumer (daemon); `uffs-client` has 3 (daemon, cli, mcp).

### Layer 4 — Apps (3 binaries, all non-publishable)

| Crate | Description | Internal deps |
|---|---|---|
| `uffs-daemon` | Background service process (`uffsd`); owns the index + serves clients over IPC | uffs-client, uffs-core, uffs-format, uffs-mft, uffs-security, uffs-broker-protocol |
| `uffs-cli` | Thin CLI binary (`uffs`); user-facing search frontend | uffs-client, uffs-format, uffs-time |
| `uffs-mcp` | MCP stdio adapter for AI agents | uffs-client |

**Layer-4 contract:** Bin-only or bin-dominant.  Allowed to depend on any subset of Layers 0-3 directly (not only via Layer 3).  The CLI's direct dep on `uffs-format` (in addition to its transitive dep via `uffs-client`) is intentional: it avoids forcing `uffs-client` to re-export formatter API for the CLI's benefit.

### Tooling tree (3 crates, all permanently non-publishable)

| Crate | Description | Path |
|---|---|---|
| `uffs-ci-pipeline` | Workspace CI driver (promoted from rust-script) | `scripts/ci-pipeline/` |
| `uffs-gen-hooks` | Gate-manifest hook generator | `scripts/ci/uffs-gen-hooks/` |
| `uffs-gen-workflow` | Gate-manifest workflow structural validator | `scripts/ci/uffs-gen-workflow/` |

**Tooling contract:** Parallel tree, NOT part of the runtime dep graph.  Tooling crates MUST NOT depend on any `uffs-*` runtime crate.  Tooling crates carry their own minimal deps (clap, anyhow, etc.) and exist solely to orchestrate the workspace's CI / hook lifecycle.

## 3. Allowed dependency directions

| Consumer layer | May depend on | Examples |
|---|---|---|
| Layer 4 (apps) | Layers 0, 1, 2, 3 (any subset) | `uffs-daemon` → `uffs-broker-protocol`, `uffs-cli` → `uffs-format` |
| Layer 3 (app logic) | Layers 0, 1, 2 | `uffs-core` → `uffs-mft`, `uffs-client` → `uffs-format` |
| Layer 2 (domain composition) | Layers 0, 1 | `uffs-format` → `uffs-mft`, `uffs-diag` → `uffs-polars` |
| Layer 1 (domain primitives) | Layer 0 only | `uffs-mft` → `uffs-security`, `uffs-broker` → `uffs-broker-protocol` |
| Layer 0 (foundation) | Nothing internal | — (must compile with zero `uffs-*` deps) |
| Tooling tree | Nothing internal | Parallel tree; never depends on runtime crates |

**Forbidden patterns:**

- ❌ Same-layer deps (e.g., `uffs-format` → `uffs-diag` would be Layer 2 ↔ Layer 2)
- ❌ Upward deps (e.g., `uffs-mft` → `uffs-core` would be Layer 1 ← Layer 3)
- ❌ Tooling → runtime deps (e.g., `uffs-ci-pipeline` → `uffs-core`)
- ❌ Layer-0 → Layer-0 deps (e.g., `uffs-text` → `uffs-time` is currently absent and should stay absent — each Layer-0 crate is independently buildable)

**Adding a new dep edge requires:**

1. Checking the layer table above
2. Verifying the proposed edge is downward
3. Updating `phase_2_findings.md` (or its successor) with the rationale
4. If the edge is for a single utility function, considering whether the utility belongs in a Layer-0 crate instead

## 4. Async / unsafe / FFI permissions per crate

| Crate | Layer | Async (tokio)? | Unsafe (#blocks) | FFI deps | Notes |
|---|---|---|---|---|---|
| `uffs-polars` | 0 | No | 0 | none | Pure facade |
| `uffs-security` | 0 | No | 31 | Win32 DPAPI/DACL, libc, memmap2 | Crypto + Keychain + flock |
| `uffs-text` | 0 | No | 0 | none | Pure logic |
| `uffs-time` | 0 | No | 0 | none | Pure `const fn` |
| `uffs-broker-protocol` | 0 | No | 0 | none | Wire-protocol types |
| `uffs-mft` | 1 | Limited | 161 | Windows-sys | IOCP + USA fixup + attribute decode |
| `uffs-broker` | 1 | No | 23 | Windows-sys | Named pipes + handle vending |
| `uffs-format` | 2 | No | 0 | none | Pure formatter |
| `uffs-diag` | 2 | No | 0 | none | Pure analysis binaries |
| `uffs-core` | 3 | Yes (tokio) | 1 | memmap2 | Query engine; tokio for async paths |
| `uffs-client` | 3 | Yes (tokio) | 57 | Windows-sys, libc, memmap2 | IPC + memmap + cross-platform |
| `uffs-daemon` | 4 | Yes (tokio) | 25 | Windows-sys, libc | Service host, owns the index |
| `uffs-cli` | 4 | No | 0 | none | Pure CLI; uses `uffs-client` for async |
| `uffs-mcp` | 4 | Yes (tokio) | 0 | none | MCP stdio adapter |
| `uffs-ci-pipeline` | tool | No | 0 | none | Workspace CI driver |
| `uffs-gen-hooks` | tool | No | 0 | none | Hook generator |
| `uffs-gen-workflow` | tool | No | 0 | none | Workflow validator |

**Permission rules:**

- **Layer 0 cap:** No async, minimal unsafe (only crypto / FFI sites in `uffs-security`).  Other Layer-0 crates must remain unsafe-free.
- **Layer 1 expectation:** Unsafe is concentrated here for Win32 FFI.  `uffs-mft` and `uffs-broker` are the workspace's primary unsafe surfaces (184 blocks combined).
- **Layer 2 cap:** Zero unsafe.  Layer 2's job is composition; unsafe code belongs lower.
- **Layer 3 cap:** Minimal unsafe.  `uffs-core` has 1 unsafe block (memmap); `uffs-client` has 57 (IPC handle + memmap + cross-platform glue — under audit for Phase 11 / #198).
- **Async permission:** Anchored at Layer 3.  `tokio` may appear at Layer 1 (`uffs-mft`) only for limited concurrency; Layer 2 must be sync.

## 5. Public-vs-internal partition

UFFS workspace members fall into two visibility categories:

### Publishable (5 crates, `publish = true`)

These ship to crates.io as standalone libraries for external consumers:

- `uffs-broker` — Windows elevated-handle vendor (binary; intended for `cargo install`)
- `uffs-broker-protocol` — Wire-protocol types (depend-and-implement library)
- `uffs-security` — General-purpose Rust security primitives (DPAPI + DACL + memmap helpers)
- `uffs-text` — Unicode/NTFS text utilities (case folding + trigrams)
- `uffs-time` — NTFS FILETIME arithmetic (pure `const fn`)

Each has explicit `publish = true` in its `Cargo.toml` overriding the workspace default of `publish = false`.

### Non-publishable (12 crates, `publish.workspace = true` → inherit `false`)

These are workspace-internal.  They split into two subgroups:

**Polars-blocked / deferred-publishable (8):** `uffs-cli`, `uffs-client`, `uffs-core`, `uffs-daemon`, `uffs-format`, `uffs-mcp`, `uffs-mft`, `uffs-polars`.  Transitively depend on the git-pinned `uffs-polars`; `cargo package` hard-fails on git revs.  These flip to publishable when Polars upstream releases the nightly-API patches our git rev carries.

**Permanently internal (4):** `uffs-diag`, `uffs-ci-pipeline`, `uffs-gen-hooks`, `uffs-gen-workflow`.  Internal-by-design tooling; will remain non-publishable forever.

Each crate's `Cargo.toml` carries an inline comment recording which subgroup it belongs to.  The full rationale lives in `release-automation-baseline.md` §10 row 5.

## 6. Decision matrix — adding new code to UFFS

When you need to add new code, use this matrix to decide where it goes:

| New code is about... | Add it to | Rationale |
|---|---|---|
| New NTFS attribute parsing | `uffs-mft` | Layer 1 — domain primitive |
| New CSV/columnar output format | `uffs-format` | Layer 2 — formatter composition |
| New query operator (e.g., `.has_extension()`) | `uffs-core` | Layer 3 — query engine |
| New IPC command type | `uffs-client::protocol` + `uffs-daemon::request_handler` | Layer 3 + Layer 4 |
| New CLI subcommand | `uffs-cli::commands` | Layer 4 |
| New crypto primitive | `uffs-security` | Layer 0 — foundation |
| New `usize → integer-N` conversion | `uffs-text` (preferred Layer 0 home) OR the consumer crate itself | Avoid creating cross-layer utility-only deps |
| Internal CI tooling script | `scripts/ci/<name>/` | Tooling tree |
| Windows-specific privileged operation | `uffs-broker` (Windows binary) + `uffs-broker-protocol` (cross-platform types) | F5 split |

When you need to add a new dependency:

1. **Check the layer:** Use `cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "<consumer>") | .dependencies[] | select(.path != null) | .name'` to see current internal deps.
2. **Verify direction:** Is the proposed target in a lower layer than the consumer?  If not, STOP — file a Phase-2 / #189 follow-up issue.
3. **Verify necessity:** Is the consumer importing **substantial** API surface, or just a single utility function?  If single-utility-only, consider relocating the utility instead.
4. **Update this doc:** Add the new edge to the per-layer inventory + the dep-direction table if it represents a new pattern.

## 7. Open architectural questions (Phase-2 carry-overs)

| Question | Status | Tracked in |
|---|---|---|
| Should `uffs_mft::len_to_u16` move to a Layer-0 crate (eliminate `uffs-format → uffs-mft` utility-only dep)? | Deferred to Phase 3 module-layout review | (Phase-2 findings; follow-up issue TBD) |
| Should `uffs-core`'s 22-pub-mod surface split into 2-3 sibling crates (e.g., `uffs-query` / `uffs-search-engine` / `uffs-index`)? | Deferred — single consumer (daemon) means cohesion outweighs split cost | (Phase-2 findings) |
| Should a shared `uffs-test-support` crate extract common dev-deps (proptest, tempfile, fixture helpers) from the 7 + 3 + 3 dev-dep crates? | Open — defer to Phase 13 testing strategy | (Phase-2 findings) |

These questions are recorded here so future contributors know the **decision rationale** for the current shape, not so they're treated as bugs to fix.

## 8. Maintenance cadence

This document is updated when:

- A new workspace member crate is added → add to §2 + §3
- An existing crate's layer changes → update §1 + §2 + §3 + §4
- A new async / unsafe / FFI dependency is introduced → update §4
- A crate flips between publishable / non-publishable → update §5
- An open architectural question is resolved → move from §7 into the closed-decision history

A drift-detector that fails CI when `cargo metadata`'s graph disagrees with §2-§3 is a future enhancement (tracked similarly to manifest drift-detector #211).
