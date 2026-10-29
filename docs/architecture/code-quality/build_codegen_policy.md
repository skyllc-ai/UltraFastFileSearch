# UFFS Build Script, Macro, Codegen, and Environment Policy

> **Companion documents:**
> [`panic_policy.md`](panic_policy.md) (Phase 5e),
> [`allocation_policy.md`](allocation_policy.md) (Phase 6f),
> [`trait_policy.md`](trait_policy.md) (Phase 7g),
> [`dependency_policy.md`](dependency_policy.md) (Phase 8c),
> [`lint-posture.md`](lint-posture.md).

UFFS keeps **compile-time magic justified and traceable** so the
workspace builds the same way on every supported host, contributors
can reason about what each `build.rs` and macro does without running
the build, and the environment-variable surface stays inventoried.
This document is the project's **build / macro / codegen / env
contract**: it codifies *which* `build.rs` files exist, *what each
generates*, *which* `macro_rules!` declarations the workspace allows,
*which* codegen binaries run and how their drift is detected, and
*every* environment variable the workspace reads.

For the per-crate strategy that produced the current posture, see
[`../../dev/architecture/code_clean/phase_9_build_scripts_macros_codegen_implementation_plan.md`](../../dev/architecture/code_clean/phase_9_build_scripts_macros_codegen_implementation_plan.md)
*(local-only — internal plan)*.

---

## 1  The rule

Stated as a one-liner contributors can quote:

> **Every `build.rs` falls into one of the three playbook §1041-1046
> justification classes.  Every `macro_rules!` falls into one of the
> three playbook §1064 justification classes; control-flow hiding is
> forbidden.  Every codegen binary has a drift detector or a
> documented "no idempotency contract" rationale.  Every environment
> variable the workspace reads is in the §5 registry below with name,
> scope, type, default, where read, and semver class.**

The categories:

### 1.1 `build.rs` justification classes (playbook §1041-1046)

| Class | Pattern | Verdict |
|---|---|---:|
| **B1 — Native library detection** | `pkg-config` / `vcpkg` / probing for an externally-installed C/C++ library | **KEEP** if necessary; document the platform expectations inline |
| **B2 — Code generation tied to build inputs** | Reading a non-Rust input file (icon, manifest, schema) and producing a build artifact (PE resource section, generated `.rs`, etc.) | **KEEP** if no Rust-source equivalent exists |
| **B3 — Compile-time probing that cannot move elsewhere** | Emitting `cargo:rustc-link-arg-*`, `cargo:rustc-cfg=*`, or similar directives whose only sink is `build.rs` | **KEEP** if the directive has no `#[link]` / `#[cfg_attr]` equivalent |
| **B-X — Convenience** | "It seemed convenient once" | **FORBIDDEN** — refactor to plain Rust, a checked-in generated file, or a one-time tool |

### 1.2 `macro_rules!` justification classes (playbook §1062-1064)

| Class | Pattern | Verdict |
|---|---|---:|
| **M1 — Syntax shaping** | Variadic args, embedded `?`-propagation, implicit closure-like captures — a shape a function cannot express cleanly | **KEEP** |
| **M2 — Trait/impl repetition** | Generates repeated `impl Trait for Type` blocks where bodies differ only mechanically | **KEEP** |
| **M3 — Pattern capture** | Captures a syntactic pattern that has no first-class expression in Rust (e.g. `const` declarations from a list) | **KEEP** |
| **M-X — Control-flow hiding** | A macro that could be a plain function but is a macro for stylistic reasons | **FORBIDDEN** |

### 1.3 Codegen binary classes

| Class | Pattern | Verdict |
|---|---|---:|
| **C1 — Emitter / validator with drift detector** | Reads a source-of-truth, emits an artifact (or `--check`-validates one); paired with a `*-drift` gate in `scripts/ci/gates.toml` | **KEEP** |
| **C2 — Orchestrator without idempotency contract** | Drives a process (release, deploy) whose output is state, not a static artifact | **KEEP** if integration tests cover the logic; document the C2 rationale |
| **C-X — Bespoke unaudited generator** | Codegen binary outside the gates manifest, with no drift detector and no integration tests | **FORBIDDEN** |

Test code is exempt from this policy — see
[`clippy.toml`](../../../clippy.toml) `allow-*-in-tests = true` and the
test-substitution boundary documented in
[`panic_policy.md` §1](panic_policy.md).

---

## 2  The lint posture

Phase 9 introduces **no new clippy lints**.  Build, macro, codegen, and
env-var hygiene is enforced through five complementary tools wired into
pre-push and CI:

| Tool | What it catches | Where |
|---|---|---|
| `scripts/dev/build_codegen_audit.sh` | New `build.rs` / proc-macro / `macro_rules!` / codegen binary / env-var without a corresponding registry entry | On-demand (re-run before merging refactors that touch build / macros / env) |
| `scripts/ci/uffs-manifest-audit` (`manifest-drift` gate) | Workspace-inheritance invariants in `Cargo.toml` | Pre-push + `pr-fast.yml::manifest-drift` |
| `scripts/ci/uffs-gen-hooks` (`hooks-drift` + `fast-drift` gates) | `_lint_pre_push.sh` + `_lint_fast.sh` drift from `gates.toml` | Pre-push + `pr-fast.yml::hooks-drift` + `pr-fast.yml::fast-drift` |
| `scripts/ci/uffs-gen-workflow` (`workflow-drift` gate) | `pr-fast.yml` drift from `gates.toml` | Pre-push + `pr-fast.yml::workflow-drift` |
| `cargo build --workspace --timings` | Compile-time regression from a new `build.rs` or macro-heavy site | Pre-release + on-demand via `build_codegen_audit.sh --with-cargo` |

The contract is positive (the audit script flags what's missing) rather
than negative (no new clippy lints to suppress) because the surface area
is too small to justify a dedicated lint — the workspace has 1
`build.rs`, 0 proc-macros, 6 `macro_rules!` sites, 4 codegen binaries,
and 42 distinct env-var names.  A registry diff catches drift more
cheaply than a custom lint.

---

## 3  Per-section contracts (the four §988-mirror sub-contracts)

### 3.1  `build.rs` contract

Every `build.rs` added to the workspace must:

- **Document its justification class** (B1 / B2 / B3) inline at the top of the file in the crate-level rustdoc.
- **Declare every filesystem read** via `cargo:rerun-if-changed=<path>`.
- **Declare every env-var read at build time** via `cargo:rerun-if-env-changed=<NAME>` *unless* the env var is in the `CARGO_*` family (auto-tracked by Cargo).
- **Gate platform-specific work** on `target_os` / `target_env` / `target_family` / `target_arch` cfg values read from `CARGO_CFG_*` env vars.
- **Use `#[allow(clippy::expect_used, reason = "build-host panic")]`** for build-host failure modes; `panic_policy.md` §1 exempts build scripts from the runtime panic policy.

### 3.2  Proc-macro crate contract

Introducing a proc-macro crate (a crate with `proc-macro = true` in its
`[lib]` table) requires:

- **A unanimous-review decision** captured in §10 below with the trade-offs.
- **A compile-time impact analysis** — proc-macros add compile cost workspace-wide because every consumer must link the proc-macro at compile time; the analysis must show the cost is justified.
- **A boundary contract** — which crates may depend on the proc-macro crate, and the public API surface (`#[proc_macro]` / `#[proc_macro_derive]` / `#[proc_macro_attribute]` exports).
- **A test suite** — proc-macros are unit-testable via `trybuild`; the new crate must ship with a failing-and-passing test matrix.

The current workspace has **0 proc-macro crates** as a deliberate
posture (see §6).  This is not a hard ban — it is a "high bar to
clear" posture.

### 3.3  `macro_rules!` contract

Every `macro_rules!` declaration must:

- **Justify itself** in its rustdoc comment per playbook §1064 (M1 / M2 / M3).
- **Be `pub(crate)`-scoped or narrower** unless the macro is part of a published library API (no such macros exist today).
- **Live in a single crate** (no cross-crate macro graphs without an explicit `#[macro_export]` justification).
- **Have a non-macro test surface** when feasible — the *output* of the macro should be covered by ordinary unit tests, not the macro itself.

### 3.4  Codegen binary contract

Every workspace-internal codegen binary (under `scripts/ci/` or
`scripts/ci-pipeline/`) must:

- **Have a `--check` mode** (class C1) OR **a documented "process, not file" rationale** (class C2).
- **Be wired into `scripts/ci/gates.toml`** as a `*-drift` gate when class C1.
- **Have an integration test suite** under `<binary>/tests/`.
- **Document its source-of-truth → artifact relationship** in its crate-level rustdoc.

### 3.5  Environment variable contract

Every environment variable the workspace reads (via `env::var(…)` /
`env::var_os(…)` / `env!(…)` / `option_env!(…)`, including reads via a
`const NAME: &str = "VAR";` indirection) must be in the §5 registry with:

- **Name** — the exact env var key.
- **Type** — `bool` (parsed permissively: `"1"` / `"true"` / `"yes"` truthy; everything else falsy unless documented otherwise; `env::var_os(…).is_some()` shape treats *any* set value as truthy and is noted inline) / `int` / `path` / `token` / `string`.
- **Default** — value used when the variable is unset.
- **Set by** — who is expected to write it: `Cargo` (automatic), `OS / shell` (system), `operator / user shell` (manual export), `CI workflow` (set by a `scripts/ci/` runner), `test harness`, ``just ship` cross-check`, etc.  This is the *expected* writer, not the only possible writer.
- **Where read** — the canonical use-site (file:line).
- **Semver class** — `STANDARD` (system-provided, never breaks: `HOME`, `PATH`), `CARGO` (Cargo-provided, see Cargo's stability promises), or `INTERNAL` (UFFS-defined, can be added / removed / renamed in any minor version with a CHANGELOG entry).

---

## 4  Hygiene rules

### 4.1 No new `build.rs` without justification

Adding a new `build.rs` to a member crate requires:

1. A `build_codegen_policy.md` §6 registry entry naming the justification class (B1 / B2 / B3).
2. The crate-level rustdoc on the new `build.rs` must document the class inline.
3. The `scripts/dev/build_codegen_audit.sh` output must show the new file with non-zero `cargo:` directives and a documented target gate.

### 4.2 No new proc-macro crate without unanimous review

Adding `proc-macro = true` to any crate requires a §10 decisions-log
entry recording the unanimous review.  The default disposition is
"don't add one" — proc-macros impose a workspace-wide compile-time
cost.

### 4.3 No new `macro_rules!` that hides ordinary control flow

If a macro could be written as a plain function (taking ordinary types
and returning ordinary values, with `?`-propagation at the call site
instead of inside the macro body), it should be a plain function.
The audit script does not detect this automatically — class M-X
violations are caught at PR review.

### 4.4 No new codegen binary without a drift detector

Adding a new emitter under `scripts/ci/` requires:

1. A corresponding `*-drift` gate in `scripts/ci/gates.toml`.
2. The gate wired into both `_lint_pre_push.sh` (via `uffs-gen-hooks`) and `.github/workflows/pr-fast.yml`.
3. An integration test suite under `scripts/ci/<binary>/tests/`.

An orchestrator (class C2) is exempt from the drift-detector requirement
but must document its C2 rationale in its crate-level rustdoc.

### 4.5 No new env var without a §5 registry entry

Adding `env::var("UFFS_FOO")` (or `env!("FOO")` / `option_env!("FOO")`)
requires a corresponding row in §5.  The audit script flags any env var
name in the source tree that does not appear in this policy doc.

### 4.6 Env-var name conventions

- `UFFS_*` — internal knobs; INTERNAL semver class.
- `UFFS_CLIENT_*` — `uffs-client` consumer knobs; INTERNAL.
- `UFFS_MCP_*` — `uffs-mcp` server knobs; INTERNAL.
- `UFFS_MFT_TEST_*` — test-only env vars; INTERNAL (no semver class because not user-facing).
- `RUST_LOG` / `RUST_LOG_FILE` — `tracing-subscriber` standard; STANDARD class.
- `CARGO_*` — Cargo-provided; CARGO class.

---

## 5  Environment variable registry

**As of:** 2026-05-19, SHA `aeb9807ac` (Phase 9 gap-closure entry).

**Source:** `scripts/dev/build_codegen_audit.sh` §6 — 42 distinct env-var
names consumed across `crates/` and `scripts/` (excluding `tests/`,
`benches/`, `examples/`, and `*_tests.rs` files).  Detection covers four
shapes: `env::var(LITERAL)`, `env::var_os(LITERAL)`, `env!(LITERAL)` /
`option_env!(LITERAL)`, and `const NAME: &str = "VAR";` indirection
(where the read site uses the const at a non-literal call site).

### 5.1 Build-time (Cargo-provided)

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `CARGO_CFG_TARGET_ENV` | `string` | (set by Cargo) | Cargo | `crates/uffs-cli/build.rs:80` | Cargo-provided cfg value for `target_env` (e.g. `"msvc"`, `"gnu"`).  CARGO class. |
| `CARGO_CFG_TARGET_OS` | `string` | (set by Cargo) | Cargo | `crates/uffs-cli/build.rs:79` | Cargo-provided cfg value for `target_os`.  CARGO class. |
| `CARGO_MANIFEST_DIR` | `path` | (set by Cargo) | Cargo | `crates/uffs-daemon/tests/ipc_integration.rs` | Absolute path to the crate's manifest directory.  CARGO class. |
| `CARGO_PKG_VERSION` | `string` | (set by Cargo) | Cargo | `crates/uffs-cli/src/args.rs` + 9 other sites | Crate version string, used in `--version` output + log preludes.  CARGO class. |
| `CARGO_TARGET_DIR` | `path` | `target/` | Cargo / user shell | `scripts/ci/build-cross-all.rs` | Custom target directory override.  CARGO class. |

### 5.2 Standard runtime (system-provided paths + identity)

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `APPDATA` | `path` | (Windows: `%USERPROFILE%\AppData\Roaming`) | Windows OS | `scripts/verify_parity.rs` | Windows-only.  STANDARD class. |
| `HOME` | `path` | (Unix: user home) | Unix shell login | `scripts/ci-pipeline/src/context.rs` + 18 other sites | Unix-only. STANDARD class. |
| `LOCALAPPDATA` | `path` | (Windows: `%USERPROFILE%\AppData\Local`) | Windows OS | `scripts/dev/daemon-readiness.rs` + 9 other sites | Windows-only.  STANDARD class. |
| `PATH` | `string` | (system) | OS / shell | `crates/uffs-daemon/src/main.rs` + 7 other sites | Executable search path; read for cross-tool discovery.  STANDARD class. |
| `SHELL` | `path` | (Unix: `/bin/sh`) | Unix shell login | `scripts/dev/build-local.rs` | Unix-only.  STANDARD class. |
| `TEMP` | `path` | (Windows: `%USERPROFILE%\AppData\Local\Temp`) | Windows OS | `scripts/dev/daemon-readiness.rs` + 5 other sites | Windows-preferred temp path.  STANDARD class. |
| `USERNAME` | `string` | (Windows: current user) | Windows OS | `crates/uffs-security/src/fs.rs` | Used for SID-hash derivation per `daemon_socket_path` flow.  STANDARD class. |
| `USERPROFILE` | `path` | (Windows: user home) | Windows OS | `scripts/dev/daemon-readiness.rs` + 20 other sites | Windows-only.  STANDARD class. |
| `XDG_CACHE_HOME` | `path` | (XDG: `$HOME/.cache`) | Unix shell / desktop env | `scripts/dev/daemon-readiness.rs` + 1 other | XDG Base Directory spec; Linux/macOS.  STANDARD class. |
| `XDG_DATA_HOME` | `path` | (XDG: `$HOME/.local/share`) | Unix shell / desktop env | `scripts/dev/mcp-readiness.rs` | XDG Base Directory spec.  STANDARD class. |
| `XDG_RUNTIME_DIR` | `path` | (XDG: `/run/user/$UID`) | Linux systemd / login session | `crates/uffs-client/src/daemon_ctl.rs` + 1 other | XDG Base Directory spec; used to locate the daemon socket on Linux.  STANDARD class. |

### 5.3 Logging

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `RUST_LOG` | `string` | `info` (set explicitly per binary) | operator / user shell | `crates/uffs-cli/src/commands/daemon_mgmt.rs` + 4 other sites | `tracing-subscriber` filter directive.  STANDARD class (tracing convention). |
| `RUST_LOG_FILE` | `path` | (none) | operator / test harness | `crates/uffs-cli/tests/cli_integration.rs` + 1 other | Optional log file path override; test-only outside `daemon_mgmt.rs`.  INTERNAL class. |
| `UFFS_LOG` | `string` | `info` | operator / user shell | `crates/uffs-cli/src/commands/daemon_mgmt.rs` + 3 other sites | UFFS-specific log level override (used when `RUST_LOG` is not set).  INTERNAL class. |
| `UFFS_LOG_DIR` | `path` | (platform default — `%LOCALAPPDATA%\UFFS\logs` / `$XDG_CACHE_HOME/uffs/logs`) | operator / `--log-dir` CLI flag | `crates/uffs-cli/src/commands/daemon_mgmt.rs` + 3 other sites | Log directory override.  INTERNAL class. |
| `UFFS_LOG_FILE` | `path` | (none — auto-generated under `UFFS_LOG_DIR`) | operator / `--log-file` CLI flag | `crates/uffs-cli/src/commands/search/args.rs` + 1 other | Log file path override.  INTERNAL class. |

### 5.4 UFFS runtime knobs

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `UFFS_API_LOG` | `path` | (none) | CI harness | `scripts/windows/api-validation.rs` | API-validation harness log path.  INTERNAL class (dev/CI only). |
| `UFFS_CACHE_PROFILE` | `bool` (`env::var_os(…).is_some()` — *any* value enables) | `false` (unset) | dev / `scripts/dev/tier-load-compare.rs` | `crates/uffs-core/src/compact_cache.rs` + 7 other sites across `uffs-core` + `uffs-mft` | Emits per-phase cache I/O timings to stderr (`[CACHE_PROFILE]` prefix).  INTERNAL class (dev / benchmark only). |
| `UFFS_DEV` | `bool` | `false` | dev / user shell | `crates/uffs-security/src/keystore.rs` | Enables dev-mode keystore relaxation (no DPAPI binding).  INTERNAL class. |
| `UFFS_ELEVATE` | `string` (`auto` / `never` / `always` / `prefer`) | `auto` | operator / user shell | `crates/uffs-cli/src/main.rs` + 1 other | Elevation policy override per `ElevationPolicy::from_env`.  INTERNAL class. |
| `UFFS_EXTRA_ARGS` | `string` | (none) | CI harness | `scripts/windows/cross-tool-benchmark.rs` | Extra CLI args appended in the cross-tool benchmark harness.  INTERNAL class (dev/CI only). |
| `UFFS_HOT_TO_WARM_IDLE_SECS` | `int` (seconds) | `60` | operator / benchmark harness | `crates/uffs-daemon/src/cache/policy.rs:135,180` (read via `HOT_TO_WARM_IDLE_ENV` const indirection) | Cache tier `Hot → Warm` transition timer override.  INTERNAL class. |
| `UFFS_MCP_AUTH_TOKEN` | `token` | (auto-generated per session) | MCP client / `just ship` | `crates/uffs-mcp/src/bin/http_gateway.rs` | MCP HTTP gateway bearer-token override.  INTERNAL class. |
| `UFFS_PARITY_DEBUG` | `bool` | `false` | dev / parity harness | `crates/uffs-mft/src/io/readers/parallel/to_index.rs` | Enables verbose chaos-order parity debugging in the LIVE parser.  INTERNAL class. |
| `UFFS_PARKED_TO_COLD_IDLE_SECS` | `int` (seconds) | `86_400` (24 h) | operator / benchmark harness | `crates/uffs-daemon/src/cache/policy.rs:141` (read via `PARKED_TO_COLD_IDLE_ENV` const indirection) + 2 other sites in `scripts/` | Cache tier `Parked → Cold` transition timer override.  INTERNAL class. |
| `UFFS_REBUILD_CHILDREN_ALWAYS` | `bool` (`env::var_os(…).is_some()` — *any* value enables) | `false` (unset) | dev / parity harness | `crates/uffs-mft/src/index/tree.rs:86` | Forces unconditional children-rebuild from name graph in LIVE parse; removes parse-order artifacts for validation runs.  INTERNAL class (dev only). |
| `UFFS_SEARCH_MAX_CONCURRENCY` | `int` (search permits) | auto: `max(2, cpus × 26 / (drives × 10))` | operator / `scripts/windows/concurrency-sweep.rs` | `crates/uffs-daemon/src/index/mod.rs:366,446` (read via `Self::SEARCH_CONCURRENCY_ENV` const indirection) | Overrides the auto-tuned search-permit target for `(cpus, drives)` topology.  INTERNAL class. |
| `UFFS_SINGLE_THREAD` | `bool` | `false` | dev / parity harness | `crates/uffs-mft/src/reader/persistence.rs` | Forces single-threaded reader for parity debugging.  INTERNAL class. |
| `UFFS_SKIP_ORPHANS` | `bool` (`env::var_os(…).is_some()` — *any* value enables) | `false` (unset) | dev / parity harness | `crates/uffs-mft/src/index/tree.rs:97` | Skips orphan-record sweep in tree aggregation (only paths reachable from ROOT through visible FILE_NAME edges are included).  INTERNAL class (dev only). |
| `UFFS_USN_REFRESH_INTERVAL_SECS` | `int` (seconds) | `300` (5 min) | operator | `crates/uffs-daemon/src/cache/policy.rs:144,203` (read via `USN_REFRESH_INTERVAL_ENV` const indirection) | USN journal refresh interval override; trades drift bound for per-drive replay efficiency.  INTERNAL class. |
| `UFFS_WARM_TO_PARKED_IDLE_SECS` | `int` (seconds) | `300` (5 min) | operator / benchmark harness | `crates/uffs-daemon/src/cache/policy.rs:138` (read via `WARM_TO_PARKED_IDLE_ENV` const indirection) + 2 other sites in `scripts/` | Cache tier `Warm → Parked` transition timer override.  INTERNAL class. |

### 5.5 UFFS client knobs

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `UFFS_CLIENT_SKIP_HEALTH_CHECK` | `bool` | `false` | `just ship` cross-check | `crates/uffs-client/src/daemon_ctl.rs` | Skips post-connect health probe (used by `just ship` cross-check).  INTERNAL class. |
| `UFFS_CLIENT_TIMEOUT_SECS` | `int` (seconds) | `600` | operator | `crates/uffs-client/src/connect_sync_platform.rs` | Per-request client deadline (`0` = no limit; async client default 300).  INTERNAL class. |

### 5.6 Build/release knobs (build-time, read by `scripts/ci/`)

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `UFFS_PROFILING_BUILD` | `bool` | `false` | `scripts/ci/` runner | `scripts/ci/build-cross-all.rs` | Enables profiling-build profile in `cargo dist`.  INTERNAL class (release-only). |
| `UFFS_RELEASE_BUILD` | `bool` | `false` | `scripts/ci/` runner / release pipeline | `scripts/ci/build-cross-all.rs` + 1 other | Enables release-build profile in `cargo dist`.  INTERNAL class (release-only). |

### 5.7 Test-only

| Name | Type | Default | Set by | Where read | Notes |
|---|---|---|---|---|---|
| `UFFS_MFT_TEST_DIR` | `path` | (none) | test harness | `crates/uffs-mft/src/io/readers/parallel/tests_chaos.rs` | Optional test-fixture directory for parallel-reader chaos harness.  INTERNAL class (test-only). |
| `UFFS_MFT_TEST_FILE` | `path` | (none) | test harness | `crates/uffs-mft/src/io/readers/parallel/tests_chaos.rs` | Optional test-fixture file path.  INTERNAL class (test-only). |

**Total: 42 distinct env-var names** across 7 scope categories (5 build-time + 11 standard-runtime + 5 logging + 15 UFFS runtime knobs + 2 client knobs + 2 build/release knobs + 2 test-only).

---

## 6  Per-crate registry

**As of:** 2026-05-19, SHA `0433065c7`.

### 6.1 `build.rs` registry

| Crate | `build.rs`? | Class | Generates | Inputs |
|---|:---:|---|---|---|
| `uffs-cli` | ✅ | **B2 + B3** | PE `.rsrc` section (icon + manifest) via `winresource`; `/DELAYLOAD` linker args for `combase.dll` + `oleaut32.dll` | `assets/brand/icons/uffs.ico`, `crates/uffs-cli/app.manifest` |
| `uffs-broker`, `uffs-broker-protocol`, `uffs-client`, `uffs-core`, `uffs-daemon`, `uffs-diag`, `uffs-format`, `uffs-mcp`, `uffs-mft`, `uffs-polars`, `uffs-security`, `uffs-text`, `uffs-time` | — | (none) | — | — |

### 6.2 Proc-macro registry

**0 proc-macro crates** workspace-wide.

The deliberate non-introduction is the workspace posture as of Phase 9
entry (SHA `0433065c7`).  Adding one requires the §3.2 contract.

### 6.3 `macro_rules!` registry

| Crate | Macro | File:line | Class | Justification |
|---|---|---|---|---|
| `uffs-mft` | `read_u8` | `src/index/storage/deserialize.rs:82` | **M1** | Embeds `?`-propagation + implicit `(data, &mut pos)` captures inside the deserializer.  Function form would require `(data, &mut pos)?` boilerplate at ~30 call sites. |
| `uffs-mft` | `read_u16` | `src/index/storage/deserialize.rs:89` | **M1** | Same as `read_u8`. |
| `uffs-mft` | `read_u32` | `src/index/storage/deserialize.rs:101` | **M1** | Same as `read_u8`. |
| `uffs-mft` | `read_u64` | `src/index/storage/deserialize.rs:113` | **M1** | Same as `read_u8`. |
| `uffs-mft` | `read_i64` | `src/index/storage/deserialize.rs:125` | **M1** | Same as `read_u8`. |
| `uffs-mft` | `drive_letter_consts` | `src/platform/drive_letter.rs:325` | **M2 + M3** | Generates 26 `pub const A: Self = Self(b'A')` declarations from a `letter = byte` list; `const` items cannot be generated by functions. |

### 6.4 Codegen binary registry

| Binary | Class | Source of truth | Generated artifact | Drift gate |
|---|---|---|---|---|
| `scripts/ci/uffs-gen-hooks` | **C1** | `scripts/ci/gates.toml` | `scripts/hooks/_lint_pre_push.sh` + `scripts/hooks/_lint_fast.sh` | `hooks-drift` + `fast-drift` |
| `scripts/ci/uffs-gen-workflow` | **C1** | `scripts/ci/gates.toml` | `.github/workflows/pr-fast.yml` (validated, not emitted) | `workflow-drift` |
| `scripts/ci/uffs-manifest-audit` | **C1** | 15 Phase-1 manifest invariants | every member `Cargo.toml` (validated, not emitted) | `manifest-drift` |
| `scripts/ci-pipeline` | **C2** | N/A | N/A (process: tagged release on GitHub) | N/A — see §3.4 + `release-automation-plan.md` |

---

## 7  Anti-patterns

The audit explicitly checks for and rejects:

| Anti-pattern | Why it's rejected | Correct alternative |
|---|---|---|
| `build.rs` that re-implements `cfg!()` logic in shell-style env probing | `build.rs` should *emit* cfg directives, not re-derive them; Cargo already exposes `target_*` via `CARGO_CFG_*` | Read `CARGO_CFG_TARGET_OS` etc. and gate the effectful block accordingly |
| `build.rs` that calls external commands (`git`, `make`) without `cargo:rerun-if-changed=` declarations | Causes silent stale-cache builds; CI passes, local fails | Either declare the command's input files as `rerun-if-changed=` or move the work to a one-time tool |
| Macro that takes `&self` / `&mut self` and could be an inherent method | Hides ordinary method-call shape behind macro syntax | Make it an inherent method |
| Macro that wraps a `match` or `if let` with a single arm | Hides ordinary control flow | Inline the `match` / `if let` |
| Proc-macro crate that depends on more than 3 transitive crates | Compile-time cost on every consumer | Re-implement using `syn::parse_str` or move logic to a build-script-emitted file |
| Codegen binary that emits a `.rs` file without a drift detector | Generated code can drift from source-of-truth silently | Add a `*-drift` gate to `scripts/ci/gates.toml` |
| Env var read without a §5 registry entry | Surface drift: future contributors don't know the var exists | Add a §5 row before merging the read |
| Env var with name `FOO` (single short uppercase word) | Collides with system / shell vars; `X`-style false positives in audits | Prefix `UFFS_` or use a standardized name (`HOME`, `PATH`, `XDG_*`) |

---

## 8  Audit cadence

- **On every workspace-wide refactor phase** (Phases 1–N of the playbook), re-run `scripts/dev/build_codegen_audit.sh` and refresh §6.1 (`build.rs`), §6.3 (`macro_rules!`), §6.4 (codegen), and §5 (env-var registry).  Update §10 with the phase decisions log row.
- **On every new env-var introduction**, add a §5 row in the same PR that adds the `env::var(…)` call.  The audit script will reject the PR if the new name is missing.
- **On every new `build.rs`**, add a §6.1 row in the same PR + add the file-level rustdoc justification per §3.1.
- **On every new `macro_rules!`**, add a §6.3 row + the macro's own rustdoc justification per §3.3.
- **On every new codegen binary**, add a §6.4 row + the corresponding `*-drift` gate in `scripts/ci/gates.toml`.
- **Annual cadence**, re-run the full audit script + refresh the env-var registry; catches drift from cleanups that removed an env var without removing its registry row.

---

## 9  Cross-references

- **Audit tool:** `scripts/dev/build_codegen_audit.sh` (Phase 9a — landed via PR #299).
- **Companion policies:** `panic_policy.md` (Phase 5e — exempts `build.rs` from runtime panic policy), `allocation_policy.md` (Phase 6f), `trait_policy.md` (Phase 7g), `dependency_policy.md` (Phase 8c — same §988-mirror contract shape for features).
- **Gates manifest:** `docs/architecture/gates-manifest-plan.md` (Phases 1-3 — defines the `gates.toml` substrate that `uffs-gen-hooks` + `uffs-gen-workflow` + `uffs-manifest-audit` consume; see §6.4).
- **Release automation:** `docs/architecture/release-automation-plan.md` (defines the `scripts/ci-pipeline` C2 contract).
- **Phase 1 manifest plan** (local-only): defines the 15 invariants `uffs-manifest-audit` encodes.
- **Workspace lints:** `Cargo.toml [workspace.lints.clippy]` + `clippy.toml` — Phase 9 adds **no new clippy lints**.
- **Workspace dependencies:** `Cargo.toml [workspace.dependencies]` — `winresource` (used by `uffs-cli/build.rs`) is a workspace dep per `dependency_policy.md`.

---

## 10  Decisions log

Append-only.  Each entry: date, sub-phase, decision, PR.

| Date | Sub-phase | Decision | PR |
|---|---|---|---|
| 2026-05-19 | 9a | Land `scripts/dev/build_codegen_audit.sh` as the workspace's build/macro/codegen/env baseline tool.  Mirror Phase 6a / 7a / 8a script shape.  Emits Markdown to stdout; reruns in ~1 s. | #299 |
| 2026-05-19 | 9b | `uffs-cli/build.rs` audited.  Verdict: B2 + B3 (PE resource embedding + `/DELAYLOAD` link args).  No drift.  No refactor.  See `phase_9_build_audit_findings.md`. | #300 |
| 2026-05-19 | 9c | Record deliberate "0 proc-macro crates" workspace posture.  Adding one requires the §3.2 contract. | #300 |
| 2026-05-19 | 9d | All 6 `macro_rules!` sites audited.  Verdict: 5 × M1 (binary read helpers — embedded `?`-propagation + implicit captures), 1 × M2+M3 (`drive_letter_consts!` — 26-letter const declaration repetition).  No drift.  Refactor candidate (read helpers → `Cursor` struct) deferred — see `phase_9_macro_audit_findings.md` §5.1. | #300 |
| 2026-05-19 | 9e | All 4 codegen binaries audited.  Verdict: 3 × C1 (`uffs-gen-hooks`, `uffs-gen-workflow`, `uffs-manifest-audit` — all `--check`-mode validators wired into `*-drift` gates) + 1 × C2 (`ci-pipeline` — release orchestrator, no idempotency contract).  No drift.  See `phase_9_codegen_inventory.md`. | #300 |
| 2026-05-19 | 9f | Env-var registry §5 populated: 36 distinct names across 7 scope categories (5 build-time, 11 standard-runtime, 5 logging, 9 UFFS runtime knobs, 2 client knobs, 2 build/release knobs, 2 test-only). | #300 |
| 2026-05-19 | 9g | Policy doc + CONTRIBUTING §"Build, codegen, and env-var policy" cross-link landed.  Mirrors Phase 5e / 6f / 7g / 8c cadence. | #300 |
| 2026-05-19 | 9-gap | **Gap closure post-#300.**  Deep audit against playbook §1013-1078 + plan §0.2 identified 5 gaps: (A) §5 was missing the **Set by** column required by plan §0.2 item 5; (B) per-crate `# Environment` rustdoc sections deferred (plan §1 row 9f deliverable); (C) `crates/uffs-cli/build.rs` rustdoc was missing the env-var listing required by plan §2 criterion 3; (D) `count_includes` over-counted doc-comments by 1; (E) audit script missed `env::var_os(…)` + const-name indirection detection, causing **6 env vars** to be absent from §5 (`UFFS_CACHE_PROFILE`, `UFFS_HOT_TO_WARM_IDLE_SECS`, `UFFS_REBUILD_CHILDREN_ALWAYS`, `UFFS_SEARCH_MAX_CONCURRENCY`, `UFFS_SKIP_ORPHANS`, `UFFS_USN_REFRESH_INTERVAL_SECS`).  Corrected workspace baseline 36 → 42 env vars + 2 → 1 include sites.  Also corrected stale defaults for `UFFS_PARKED_TO_COLD_IDLE_SECS` (300 → 86 400) and `UFFS_WARM_TO_PARKED_IDLE_SECS` (60 → 300). | this PR |
