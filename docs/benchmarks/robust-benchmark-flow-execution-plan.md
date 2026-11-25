# Robust Benchmark Flow — Execution Plan & Deep Dive

**Status:** DRAFT for review — no code written yet. This document is the build
plan for turning the current half-automated benchmark scripts into a single,
reproducible, **self-restoring** benchmark flow that a third party can run on
their own machine and trust.

**Audience:** the engineer implementing the flow, and the reviewer signing off
on each stage before it runs.

**Companion docs:** [`methodology.md`](methodology.md) (the fairness doctrine —
*what* we measure and *why*), [`README.md`](README.md) (the hub), and the
current canonical report [`2026-06-v0.5.120-vs-everything.md`](2026-06-v0.5.120-vs-everything.md).
This document is the *how we run it without wrecking the machine* layer.

---

## 1. Two guiding principles (non-negotiable)

### P1 — No crumb left behind

Every benchmark run mutates host state: it stops the UFFS daemon, purges
caches, rewrites `Everything.ini`, restarts Everything, writes temp files, and
sometimes sets environment variables. The flow **must** leave the machine in
the exact state it found it.

The contract for **every** mutating action:

```
1. SNAPSHOT  — record / back up the current state of the resource.
2. MUTATE    — apply the benchmark-required change.
3. EXECUTE   — run the measured work.
4. RESTORE   — put the resource back to its snapshot, verbatim.
5. VERIFY    — assert the restored state == the snapshot (hash / equality).
```

Restore is guaranteed even on failure or Ctrl-C (a Rust `Drop` guard around a
LIFO **restore registry**; see §6). A run that cannot prove it restored cleanly
is a **failed run** and says so loudly.

### P2 — The user validates every stage (unless they opt into autopilot)

The flow is a sequence of **gated stages**. Before each stage that mutates
state or costs real time, the orchestrator prints:

- what it is about to do,
- exactly what host state it will change,
- the backup it just took (path + hash),

then **waits for the operator** to choose: proceed / skip / abort / autopilot.
Passing `--auto` (or answering `a` at the first gate) runs the rest
unattended. `--dry-run` prints every planned action and mutates nothing.

---

## 2. Scope & non-goals

**In scope**
- A single orchestrator — the **Rust workspace crate `crates/uffs-bench`**
  (`just bench-suite` → the `uffs-bench` binary) — that runs Preflight → §1
  cross-tool → §2 parity → §3 full-suite → Assembly, delegating measurement to
  the existing leaf harnesses.
- Non-destructive competitor preflight (what is Everything actually holding?).
- "Match the weakest tool" drive/pattern matrix negotiation.
- Environment fingerprint capture.
- Competitor binary resolution + version pinning (link + hash, not redistribute).
- **Tooling lifecycle:** tools the suite *acquires* carry a keep/remove
  disposition; pre-existing host resources are restore-only, never deleted (§6.4).
- **State file + resumability:** every decision and step checkpoint is recorded
  in `bundle/state.json` so a re-run skips work already done (§6.5).
- A standardized, dated output bundle + report scaffold.
- A self-test that proves the machine was restored.

**Non-goals**
- Cross-OS benchmarking (Windows-only, as today).
- Redistributing third-party binaries we don't have a license to ship.
- Changing the *measurement* methodology in [`methodology.md`](methodology.md) —
  this is orchestration and hygiene only, not new numbers semantics.

---

## 3. Glossary

| Term | Meaning |
|---|---|
| **Resource** | A host-state item the flow may mutate (daemon, cache dir, `Everything.ini`, env var, temp file, OS page cache). |
| **Snapshot** | A recorded copy / fingerprint of a resource taken before mutation. |
| **Restore registry** | A LIFO stack of restore closures, drained by a `Drop` guard (the `try/finally` analogue in Rust). |
| **Gate / checkpoint** | A user-validation pause before a stage. |
| **Bundle** | The dated output folder holding env, CSVs, markdown tables, transcripts, the state file, and the restore manifest. |
| **State file** | `bundle/state.json` — every decision + per-step checkpoint; drives skip-completed-work resume (§6.5). |
| **Acquisition** | A tool the suite itself downloaded/installed, tracked with a `Keep`/`Remove` disposition (§6.4). |
| **Capable drive** | A drive every required tool can actually serve for a given pattern (intersection). |
| **Crumb** | Any residual host-state change left behind after a run. A bug. |

---

## 4. Current state (what we build on)

Already automated and reused as-is (wrapped, not rewritten):

- `scripts/windows/cross-tool-benchmark.rs` — §1 harness (p50/p95, sinks,
  row-validation, `--out` summary CSV). Wrapped by `just bench-cross`.
- `scripts/windows/cold-parity-per-drive.ps1` — §2 parity, emits markdown
  tables inside a `Start-Transcript` log; `-PurgeCacheFirst` for COLD.
- `scripts/windows/everything_capacity_probe.rs` — Everything per-drive IPC
  ceiling + readiness; **already rewrites and restores `Everything.ini`** (the
  pattern we generalize in §6). Today it is destructive + standalone.
- `scripts/windows/parity_check.rs` — correctness (sha256 of result sets).

Gaps this plan closes: no single orchestrator; the §3 "full-benchmark-suite"
numbers come from **hand-pasted PowerShell** (not a script); no env capture;
no competitor preflight wired into the real run; no drive negotiation; no
binary pinning; no unified restore guarantee across all resources.

## 5. Architecture

One orchestrator, six stages, one restore guarantee around all of them.

```
just bench-suite  ──>  crates/uffs-bench  (the `uffs-bench` orchestrator binary)
   │
   ├─ Stage 0  PREFLIGHT
   │     0a env fingerprint        (read-only)
   │     0b binary resolve + pin   (read-only / staging dir only)
   │     0c competitor preflight   (read-only: what is Everything holding?)
   │     0d matrix negotiation     (compute capable drive × pattern cells)
   │     0e GATE: show plan, wait for operator approval
   │
   ├─ Stage 1  CROSS-TOOL  (§1)    shells out to cross-tool-benchmark.rs
   ├─ Stage 2  PARITY      (§2)    shells out to cold-parity-per-drive.ps1 (pwsh)
   ├─ Stage 3  FULL-SUITE  (§3)    NEW: ported into uffs-bench `stages.rs`
   ├─ Stage 4  ASSEMBLY            env.json + *.csv + *.md + transcripts
   └─ Stage 5  TEARDOWN + VERIFY   drain restores, resolve tooling, prove no crumbs
```

The whole body runs under a Rust `Drop` guard around the restore registry, so
Stage 5 fires even on early return, `?`-error, or Ctrl-C. Progress is persisted
to `bundle/state.json` after every step, so a re-run is resumable and each stage
is independently runnable for debugging (`--only-stage 1`); the default is the
full chain.

**Orchestration choice:** a **Rust workspace crate**, not PowerShell. Rationale:
(1) its self-tests run under `cargo nextest` in the *same* `just go`/CI lane, so
the "no crumbs" proof (§13) is actually enforced — a separate Pester ecosystem
would never run in the pipeline; (2) the hardest "Windows-only" actions
(`taskkill`, spawning Everything, backing up/restoring `Everything.ini`) are
already done in Rust by `everything_capacity_probe.rs`; (3) `serde` gives the
state file, resume, and restore manifest for free with typed errors. The
orchestrator **shells out** to the existing measurement harnesses — we do **not**
reimplement the measurement loops.

---

## 6. The "no crumb left behind" engine (§P1 in detail)

### 6.1 Mutated-state inventory

Every resource the flow can touch, how we snapshot it, how we restore it, and
whether restore is *lossless* (we can put it back byte-for-byte) or *managed*
(we can only return it to a functionally-equivalent state and must say so).

| # | Resource | Mutated by | Snapshot | Restore | Lossless? |
|---|----------|-----------|----------|---------|-----------|
| R1 | UFFS daemon run-state (running? which drives? PID) | every stage | `uffs --status` + `uffs --daemon status --json` | restart with same drive set, or leave stopped if it was stopped | Managed |
| R2 | UFFS cache dir `%LOCALAPPDATA%\uffs\cache` (+ legacy `%TEMP%\uffs_index_cache`) | COLD runs (`-PurgeCacheFirst`) | **copy** per-drive `*.uffs` files to bundle `backup/uffs-cache/` | copy files back, restoring the user's pre-run cache so no forced rebuild | Lossless |
| R3 | `Everything.ini` (`%APPDATA%\Everything\Everything.ini`) | capacity/isolation probe (drive isolation) | **copy** whole file + sha256 | write file back verbatim, verify sha256 | Lossless |
| R4 | Everything.exe process (running? minimized? with which config) | preflight / isolation restarts | record running state + path | restart Everything the way we found it (or kill if it was not running) | Managed |
| R5 | Temp/output files (`uffs_bench_out.csv`, `es_probe_*.tmp`, `rust_*/cpp_* .csv`) | all harnesses | n/a (we own them) | delete on teardown | Lossless |
| R6 | Environment variables (`UFFS_EXTRA_ARGS`, `UFFS_BENCH_DRIVES`, `RUSTFLAGS`) | bench wrappers | set **only in child scope**, never `setx` | scope ends with process; assert not leaked to user shell | Lossless |
| R7 | Staged/acquired competitor binaries (`es.exe` pin) | binary provisioning | staged to bundle `tools/`, never into `~/bin` unless absent + asked; tracked as an **Acquisition** (§6.4) | teardown removes `Remove`-disposition acquisitions; `Keep` ones are logged in `state.json` | Lossless |
| R8 | OS page cache (NTFS MFT pages) | COLD reads warm it | **cannot** snapshot | self-healing, non-persistent; we *document* its state, never claim to restore it | N/A (declared) |

R8 is the honest exception: you cannot "back up and restore" the OS page
cache. The methodology already accounts for its *direction* (Rust cold first,
C++ benefits from the warm cache). The flow's duty is to **record** page-cache
intent per run, not to pretend it restored it. If a true-cold OS read is
required, that is a **manual, opt-in** step (`--drop-os-cache`, requires an
external tool such as RAMMap/`EmptyStandbyList`, prompted and logged loudly) —
never silently performed.

### 6.2 The restore registry pattern

```rust
// Pseudocode — the spine of crates/uffs-bench (restore.rs)
pub struct RestoreRegistry { stack: Vec<(String, Box<dyn FnOnce(&dyn Host) -> Result<()>>)> }

impl RestoreRegistry {
    pub fn register(&mut self, label: &str, undo: Box<dyn FnOnce(&dyn Host) -> Result<()>>) {
        self.stack.push((label.into(), undo));          // push
    }
    pub fn drain(&mut self, host: &dyn Host) -> Vec<CrumbError> {
        let mut crumbs = Vec::new();
        while let Some((label, undo)) = self.stack.pop() {   // LIFO: last mutation undone first
            if let Err(e) = undo(host) { crumbs.push(CrumbError::new(label, e)); } // never panics
        }
        crumbs
    }
}
// A Drop guard wraps the run so drain() fires on early return / panic / Ctrl-C
// (the try/finally analogue):
impl Drop for RunGuard<'_> { fn drop(&mut self) { let _ = self.registry.drain(self.host); } }

// Every mutation registers its own undo BEFORE mutating:
fn set_everything_ini_isolated(host: &dyn Host, reg: &mut RestoreRegistry, drive: char) -> Result<()> {
    let ini = everything_ini_path();
    let backup = backup_file(host, &ini)?;               // SNAPSHOT (copy + sha256)
    reg.register("R3 Everything.ini", Box::new(move |h| {
        restore_file(h, &backup, &ini)?; assert_hash(h, &ini, &backup.hash)  // RESTORE+VERIFY
    }));
    edit_ini_isolate_drive(host, &ini, drive)            // MUTATE
}
```

Key properties:
- **Register-before-mutate.** The undo is on the stack before the change lands,
  so a crash *during* the mutation still triggers restore.
- **LIFO.** Restores unwind in reverse order (e.g. restart Everything → restore
  ini → restart UFFS daemon), matching dependency order.
- **Idempotent + non-panicking.** Each undo tolerates being run twice and never
  panics out of the `Drop` guard; failures become loud `CRUMB` warnings, not silent.
- **Crash recovery.** The same undo set is serialized to
  `bundle/restore-manifest.json` so a *separate* `just bench-restore <bundle>`
  recipe can re-apply restores if the orchestrator process was hard-killed.

### 6.3 Restore self-test (the proof, see §13)

Before Stage 0 and after Stage 5 the flow computes a **host fingerprint** over
all snapshot-able resources (ini hash, cache file list+hashes, daemon
run-state, relevant env vars, temp-file presence). Teardown asserts
`fingerprint_after == fingerprint_before` (modulo the intended bundle output).
Any diff is reported as a named crumb and fails the run.

### 6.4 Tooling lifecycle (keep or remove what we acquired)

Two classes of host resource, treated differently:

- **Pre-existing** (UFFS caches, `Everything.ini`, the daemon, the operator's
  `~/bin`): only ever snapshotted + restored via §6.2. **Never deleted.**
- **Acquired by the suite** (a downloaded `es.exe`, an installed helper): tracked
  as an `Acquisition { name, path, source, sha256, acquired_at, disposition }`
  where `disposition ∈ { Keep, Remove }`.

Rules:
- Interactive / guided: at teardown the operator is **prompted** per acquisition
  ("keep `es.exe` in `bundle/tools/`? [k]eep / [r]emove").
- `--auto`: default **Remove** (override with `--keep-tools`); `--dry-run` never
  acquires anything.
- Teardown removes only `Remove`-disposition acquisitions; every `Keep` is
  **logged** in `state.json` + the bundle, so nothing is silently left behind.
- Acquisitions and kept tooling are **excluded** from the §6.3 fingerprint diff —
  they are logged decisions, not crumbs.

### 6.5 State file & resumability

A single `bundle/state.json` (atomic write after every step) records every
operator decision (mode, drives, tools, rounds, drop-cache, tool dispositions)
and a per-step checkpoint, so a re-run **skips work already done**:

```
State { suite_version, started_at, updated_at,
        decisions,                 // mode, drives, tools, rounds, …
        acquisitions: [Acquisition],
        steps: { <step_id>: { status: Pending|Done|Skipped|Failed,
                              input_hash, outputs, started_at, finished_at } } }
```

Resume engine:
- On start, load `state.json` from `--bundle <dir>` (or auto-detect the newest);
  otherwise start a `New` bundle.
- Per step, compute `input_hash` from the decisions it depends on. A `Done`
  record with the **same** `input_hash` ⇒ **skip** ("✓ cached"); change a
  decision ⇒ dependents' hashes change ⇒ they re-run automatically.
- `--redo <step>` invalidates one step; `--force` invalidates all.
- Completed result files stay in the bundle, so resuming never re-measures.
- `just bench-resume <bundle>` is exactly "load state, skip `Done`, continue";
  `just bench-restore <bundle>` (§6.2) is the orthogonal crash-recovery path.

## 7. The validation-gate protocol (§P2 in detail)

### 7.1 Gate contract

A gate is `gate::confirm()` called at the top of every mutating / time-costly
stage. It prints a fixed block and reads one keystroke.

```
────────────────────────────────────────────────────────────────────
 STAGE 0c · COMPETITOR PREFLIGHT
 About to (READ-ONLY):
   • parse Everything.ini  (no write)
   • run es.exe "<D>:\" -get-result-count  for D in {C,D,E,F,M,S}
 Will mutate:           nothing
 Backups taken:         none required (read-only stage)
 Estimated time:        ~15 s
────────────────────────────────────────────────────────────────────
 [Enter] proceed   [s] skip stage   [a] autopilot rest   [q] abort+restore
>
```

For mutating stages the block additionally lists the exact resources (§6.1 row
IDs) and the backup path + hash just taken:

```
 Will mutate:           R3 Everything.ini, R4 Everything.exe (restart)
 Backups taken:         bundle/backup/Everything.ini  (sha256 a1b2c3…)
                        restore registered (LIFO slot #4)
```

### 7.2 Operator choices

| Key | Action |
|-----|--------|
| `Enter` | Proceed with this stage. |
| `s` | Skip this stage (its results are marked `SKIPPED` in the bundle; downstream stages that depend on it are auto-skipped with a reason). |
| `a` | **Autopilot**: proceed now and run all remaining stages without further prompts (equivalent to having launched with `--auto`). |
| `q` | Abort: stop immediately, drain the restore registry (`RestoreRegistry::drain`), write a partial bundle, exit non-zero. |

### 7.3 Modes

> **Flag style:** the orchestrator is a Rust `clap` binary, so flags use standard
> long form — `--guided`, `--auto`, `--dry-run`, `--bundle`, `--keep-tools`, etc.

- **Guided.** Rich teaching card before each step; the first-run default
  (verbatim commands, blast radius, backup-taken, time estimate). Press `a` to
  upgrade the rest of the run to autopilot.
- **Interactive (default after first run).** Terse gate prompt per stage.
- **`--auto`.** No prompts; all stages run. Still snapshots, restores, and
  verifies — autopilot relaxes *prompting*, never *hygiene*.
- **`--dry-run`.** Prints every gate block and every planned mutation with its
  intended backup path, but performs **zero** mutations and **zero**
  measurement. Used to review the full plan and the negotiated matrix before
  committing host changes. `--dry-run` implies read-only and ignores `--auto`.
- **`--only-stage <n>` / `--from-stage <n>`.** Run a single stage or resume from
  one (still wrapped in the restore guarantee).
- **`--bundle <dir>` (resume).** Load that bundle's `state.json` and skip every
  step already marked `Done` (§6.5); `--redo <step>` / `--force` re-invalidate.

### 7.4 Non-interactive safety

If stdin is not a TTY (CI, piped) and neither `--auto` nor `--dry-run` is set,
the flow **refuses to start** rather than blocking forever or guessing. This
prevents an unattended run from silently mutating a machine no one is watching.

---

## 8. Stage 0 — Preflight (the heart of the new work)

### 8.1 Stage 0a — Environment fingerprint (read-only)

Emit `bundle/env.json` and a rendered `bundle/env.md` (the §Test-environment
table). Collected via:

| Field | Source |
|---|---|
| CPU model / cores / threads / base clock | `sysinfo` crate (cross-platform) |
| RAM total / speed | `sysinfo` crate |
| OS name + build | `sysinfo` crate |
| Per-drive: filesystem, bus type (NVMe/SATA/USB), free/used | `sysinfo` disks (+ `Get-PhysicalDisk` via `Host::run` for bus type on Windows) |
| Per-drive record counts | `uffs --daemon status --json` after warm-up (via `Host::run`) |
| Tool versions | `Host::run`: `uffs --version`, `uffs.com --version`, `es.exe -get-everything-version` |
| Elevation state | `Host::is_elevated()` |
| Page-cache intent | recorded per stage (cold/warm/hot), see R8 |

This makes every third-party bundle **self-describing** — no hand-typed
environment table, and the numbers are never orphaned from the machine.

### 8.2 Stage 0b — Binary resolution + pinning (read-only / staging only)

- Resolve `uffs.exe`, `uffs.com`, `es.exe` using the **same precedence the
  existing scripts use** (explicit flag → `~/bin` → PATH / known install dirs),
  so behavior is consistent with `cold-parity-per-drive.ps1` and
  `cross-tool-benchmark.rs`.
- Read each tool's version and compare against `competitors.toml` (§12). On
  mismatch: **warn loudly, record actual version in env.json, continue** (we
  never silently benchmark a different version than the report claims).
- Never overwrite the user's `~/bin`. If a pinned `es.exe` must be staged, it
  goes to `bundle/tools/` and is used from there for this run only (R7).

### 8.3 Stage 0c — Competitor preflight: *what is Everything actually holding?*

This is the question the user raised. Mechanism, **fully read-only**:

1. **Configured volumes.** Parse `Everything.ini` → `ntfs_volume_paths`,
   `ntfs_volume_includes` (reuse `parse_drives_from_ini`). Tells us which
   drives Everything is *configured* to index.
2. **Live readiness + record counts.** For each candidate drive `D`, run
   `es.exe "D:\" -get-result-count`. Result semantics:
   - `> 0` → Everything has that drive indexed and **hot** (its index lives in
     RAM continuously); the count is the live record total it will serve.
   - `0` or error / empty → drive **not loaded** (not configured, still
     indexing, or excluded). Excluded from cross-tool cells.
3. **Readiness gate.** If a drive reports `0` but is configured, poll
   `-get-result-count` up to a timeout (reuse the probe's `wait_for_index`
   logic) to distinguish "still indexing" from "not configured". We never
   *force* a rebuild here — we only observe.
4. **Per-pattern feasibility.** For each pattern we intend to run, estimate the
   result-set size on each capable drive (cheaply, via UFFS) and compare to
   Everything's known IPC ceiling (~150 K-row `-export-csv` / ~2 GB IPC). Cells
   above the ceiling are flagged `ES_INFEASIBLE` (UFFS still runs them solo).

Output: `bundle/competitor-preflight.json` — per drive `{configured, loaded,
hot, record_count}` and per (drive,pattern) `{es_feasible, est_rows}`.

### 8.4 Stage 0d — Matrix negotiation: "match the weakest tool"

Goal: compare apples to apples. A cross-tool cell `(drive, pattern)` only runs
for *all* tools when *every required tool can serve it*. Everything else still
runs as **UFFS-only**, clearly labeled, so we never throw away coverage.

```
INPUTS
  required_tools      = {uffs, uffs_cpp, everything}        # from --tools
  candidate_drives    = drives the OPERATOR asked for       # from --drives
  es_state[d]         = competitor-preflight.json           # loaded/hot/count
  es_feasible[d,p]    = pattern feasibility vs IPC ceiling

ALGORITHM
  capable_drives = candidate_drives
  for each tool t in required_tools:
      capable_drives = capable_drives ∩ drives_servable_by(t)
      # uffs:     all candidate drives
      # uffs_cpp: all candidate drives (re-reads MFT)
      # everything: { d : es_state[d].loaded and es_state[d].hot }

  cross_cells   = { (d,p) : d ∈ capable_drives and es_feasible[d,p] }
  uffs_only     = { (d,p) : d ∈ candidate_drives } \ cross_cells

OUTPUT (printed at the 0e gate, written to bundle/matrix.json)
  • cross-tool cells  (apples-to-apples)        → counted in head-to-head
  • UFFS-only cells   (with PER-CELL reason)     → reported separately
  • excluded competitors per cell, with reason:
      "M: es not loaded (not in Everything.ini)"
      "full_scan: es infeasible (>2 GB IPC)"
      "E: es still indexing after 60 s — excluded"
```

The negotiated matrix is shown at the **0e gate** before any measurement, so
the operator sees exactly which comparisons are fair and which are solo, and
*why*, before a single timed query runs. This directly answers "Everything can
only do C&D here" — the flow detects it and restricts the head-to-head to C&D
automatically, while still benchmarking UFFS on E/F/M/S as labeled solo runs.

### 8.5 Stage 0e — The plan gate

Final preflight gate. Prints: env summary, resolved binaries + versions (with
any mismatch warnings), the competitor-preflight table, the negotiated matrix,
the estimated total runtime, and the full list of resources that later stages
*will* mutate (so the operator approves the blast radius up front). Proceed /
skip-to-stage / autopilot / abort.

---

## 9. Stages 1–3 — Measurement (wrap, don't rewrite)

Each stage: snapshot the daemon/cache state it needs, register restores, run
the existing harness with the negotiated inputs, capture its transcript into
the bundle, then let teardown restore.

### 9.1 Stage 1 — Cross-tool (§1)

- Wraps `cross-tool-benchmark.rs` with `--drives <capable>`, `--tools
  <required>`, `--patterns <feasible>`, `--rounds N`, `--out
  bundle/cross-tool-summary.csv`.
- Snapshots: R1 (daemon), R5 (temp `uffs_bench_out.csv`). COLD sub-phase also
  snapshots R2 (cache) before purge and restores after.
- Re-bench discipline (the 100-round interleaved re-bench from
  `methodology.md` §30-round) is invoked automatically for cells whose StdDev
  > 10 % of p50 or whose two-tool ratio is within 5 %.

### 9.2 Stage 2 — Parity (§2)

- Wraps `cold-parity-per-drive.ps1 -Drives <capable> -OutputFile
  bundle/parity.txt` (and a second `-PurgeCacheFirst` pass for the COLD table).
- Snapshots R1, R2, R5. The script already uses `Start-Transcript`; we point
  it at the bundle and additionally register a daemon-state restore.

### 9.3 Stage 3 — Full-suite (§3) — **NEW, ported into the crate**

Port the hand-pasted "SECTION 0 … Get-Stats" PowerShell that produced
`raw/2026-04-v0.5.66_full-benchmark-suite.txt` into the orchestrator's
`stages.rs` (driving `uffs` through `Host::run`), covering:

- drive-accumulation scale sweep (3.67 M → 26 M records via repeated
  `daemon start --drive` sets),
- 30-round targeted-query latency across all drives per pattern,
- full-scan `*` → CSV export throughput (p50 over 10 rounds),
- daemon RSS sampling at each scale point.

It uses a unit-tested `percentiles()` helper (in-crate, not inlined) and writes
`bundle/full-suite.txt` + a machine-readable `bundle/full-suite.csv`. This is
what makes the **third** raw log script-reproducible instead of manual.

---

## 10. Stage 4 — Assembly & report scaffold

Lay the bundle out (§11), then scaffold a dated report from a template so the
results drop straight into the `docs/benchmarks/` discipline:

1. Render `env.md` into the report's §Test environment table.
2. Paste the §1/§2/§3 markdown tables (the harnesses already emit these).
3. Insert raw-log citations pointing at the about-to-be-committed `raw/` files.
4. Write a `bundle/REPORT-DRAFT.md` named `YYYY-MM-vX.Y.Z-<scope>.md` — a
   **draft**, never auto-committed. Promotion into `docs/benchmarks/raw/` and a
   new canonical report is a **manual, reviewed** step (honors the archive /
   no-backfill / no-edit policy in `raw/README.md` and `methodology.md`).

Assembly is read-only with respect to host state; it only writes inside the
bundle. Promotion to `docs/benchmarks/` is out of band and gated by human review.

---

## 11. Output bundle layout (standardized)

```
LOG/bench/<YYYY-MM-DD_HH-MM>-v<version>/
├── env.json                  machine + tool fingerprint
├── env.md                    rendered §Test-environment table
├── competitor-preflight.json what Everything is holding, per drive
├── matrix.json               negotiated cross-tool vs UFFS-only cells
├── cross-tool-summary.csv    §1 (one row per tool×phase×sink×drive×pattern)
├── parity.txt                §2 transcript (markdown tables at tail)
├── full-suite.txt            §3 transcript
├── full-suite.csv            §3 machine-readable
├── REPORT-DRAFT.md           dated report skeleton (NOT auto-committed)
├── state.json                decisions + per-step checkpoints (resume; §6.5)
├── restore-manifest.json     serialized undo set for crash recovery
├── fingerprint-before.json   host fingerprint pre-run
├── fingerprint-after.json    host fingerprint post-run (must match)
├── backup/                   verbatim snapshots (Everything.ini, uffs-cache/…)
├── tools/                    acquired/pinned competitor binaries (if any; §6.4)
└── run.log                   orchestrator transcript
```

All CSVs share a common column convention so §1/§2/§3 can be joined:
`tool, version, phase, sink, drive, pattern, rows, p50_ms, p95_ms, stddev_ms,
rounds, verdict, notes`.

---

## 12. Competitor binary provisioning & pinning

Create `scripts/windows/competitors.toml`:

```toml
[everything]
version  = "1.1.0.30"                 # the version the canonical report cites
es_url   = "https://www.voidtools.com/.../ES-1.1.0.30.x64.zip"
es_sha256 = "<fill-in>"
note     = "Link + hash only. Not redistributed. Operator installs/stages."

[uffs_cpp]
version  = "v0.4.x (SwiftSearch lineage)"
location = "~/bin/uffs.com"
note     = "Internal reference binary; documented home, not fetched."
```

- `just bench-fetch-competitors` downloads to `bundle/tools/`, verifies the
  SHA-256, and **fails closed** on mismatch. Nothing is installed system-wide.
- **Licensing guardrail:** we *link and hash-pin*, we do **not** redistribute
  voidtools binaries unless their license explicitly permits it — an **open
  decision for the user** (see §16). Until resolved, provisioning is
  "download-to-bundle + verify", never "commit to repo".
- **Resolve the existing version drift** as part of this work: README cites es
  `1.1.0.30`, the cross-tool header references `1.5.3.1a` / `Everything 1.5a`
  paths, and the capacity probe looks elsewhere again. Pin **one** version and
  make every script + doc agree with `competitors.toml`.

---

## 13. Self-test: proving "no crumb left behind"

The flow is only trustworthy if it can *prove* it cleaned up. Two layers:

**13.1 Per-resource verify (inline).** Each restore closure asserts success:
ini sha256 matches the snapshot; every backed-up cache file is byte-identical;
daemon run-state matches the recorded baseline; no benchmark env var leaked to
the parent shell; all temp files removed.

**13.2 Whole-host fingerprint diff (Stage 5).**
`fingerprint-before.json` vs `fingerprint-after.json` must be equal except for
the intended bundle output. Any difference is emitted as a named **CRUMB** and
fails the run with a non-zero exit:

```
CRUMB DETECTED (1):
  R3 Everything.ini  expected sha256 a1b2c3…  got d4e5f6…
  → restore did not complete. Run:  just bench-restore <bundle>
```

**13.3 Harness self-test (CI-able on any OS).** `cargo nextest` tests drive the
orchestrator with a **`MockHost`** (in-memory FS, recorded calls, scripted
keypresses — the `Host`-trait fake) and assert: (a) every mutation registered a
restore before mutating; (b) `RestoreRegistry::drain` empties the stack; (c) an
injected mid-stage panic still restores all resources (Drop guard); (d)
`--dry-run` performs zero mutations; (e) **resume skips a `Done` step with a
matching `input_hash`**; (f) acquired tools are removed unless `Keep`, and
pre-existing resources are never removed. These run on macOS/Linux because they
never touch a real MFT — the **same lane** as `just go`, so the proof is
actually enforced.

---

## 14. CLI surface (`just` recipes)

Added to `just/bench_uffs.just`, all delegating to the orchestrator:

| Recipe | Purpose |
|---|---|
| `just bench-suite` | Full flow (Stage 0→5); guided on first run, interactive after. |
| `just bench-suite-auto` | `uffs-bench --auto` (still snapshots/restores/verifies). |
| `just bench-suite-dry` | `uffs-bench --dry-run` (plan + matrix only, zero mutation). |
| `just bench-preflight` | Stage 0 only — env + competitor preflight + matrix, no measurement. |
| `just bench-resume <bundle>` | Load `state.json`, skip `Done` steps, continue (§6.5). |
| `just bench-fetch-competitors` | Download + SHA-256-verify pinned competitor binaries to a bundle. |
| `just bench-restore <bundle>` | Re-apply restores from a bundle's `restore-manifest.json` (crash recovery). |
| `just bench-suite-verify <bundle>` | Re-run the host-fingerprint diff against a bundle to confirm no crumbs (renamed from `bench-verify`, which already exists for delay-load checks). |

Orchestrator signature (the `uffs-bench` `clap` binary):

```
uffs-bench
  --drives C,D,E,F,M,S       # operator candidate set (default: all NTFS fixed)
  --tools uffs,uffs_cpp,es   # required tools for cross-tool cells
  --rounds 30                # per-cell rounds (re-bench escalates per §9.1)
  --guided | --auto | --dry-run    # mode (guided = first-run default)
  --only-stage <n> | --from-stage <n>
  --bundle <dir>             # resume an existing bundle (skip Done steps)
  --redo <step> | --force    # invalidate one / all step checkpoints
  --keep-tools               # keep acquisitions instead of removing at teardown
  --drop-os-cache            # opt-in true-cold (external tool, loud, logged)
  --bundle-root LOG/bench    # where new bundles are written
```

---

## 15. Implementation roadmap (phased, each independently reviewable)

Each phase ends at a natural review point and leaves the tree green
(`just go`). Phases are ordered so the *hygiene* spine lands before anything
that mutates a real machine.

| Phase | Deliverable | Gate / proof |
|---|---|---|
| **P0** | This plan, reviewed and signed off. | User approves scope + open decisions (§16). |
| **P1** | Hygiene + UX + state spine — crate `crates/uffs-bench` (`host.rs`, `restore.rs`, `gate.rs`, `state.rs`, `tooling.rs`, `fingerprint.rs`): registry, `gate::confirm`, backup/restore, fingerprint, resume engine (§6.5), tooling dispositions (§6.4). | Self-test §13.3 green via `cargo nextest` on any OS (same `just go` lane). |
| **P2** | Stage 0a env fingerprint (`env.rs`, `sysinfo`) → `env.json`/`env.md`. | Bundle produced on a Windows box; table matches a hand check. |
| **P3** | Stage 0c competitor preflight (lift read-only helpers from `everything_capacity_probe.rs` into `preflight.rs`) → `competitor-preflight.json`. | Proves it never writes `Everything.ini` (`MockHost` assertion). |
| **P4** | Stage 0d matrix negotiation + 0e plan gate + orchestrator skeleton + resume → `matrix.json`. | Correct C&D-only head-to-head; resume of a `Done` Stage 0 jumps to Stage 1. |
| **P5** | Stage 1 & 2 wrappers (cross-tool + parity) with snapshot/restore, resumable. | Bundle CSVs + transcripts; fingerprint diff clean; re-run skips `Done` stages. |
| **P6** | Stage 3 ported into `stages.rs` (de-manualize §3). | Reproduces the existing raw log within noise. |
| **P7** | Stage 4 assembly + `REPORT-DRAFT.md` scaffold (`report.rs`). | Draft renders with real env + tables, not committed. |
| **P8** | `competitors.toml` + `bench-fetch-competitors` (tracked acquisition) + version-drift cleanup. | All scripts/docs cite one pinned es version; bad hash aborts. |
| **P9** | End-to-end on the reference machine; `bench-restore`/`bench-suite-verify`. | Fingerprint identical pre/post; mid-run kill recovered; restart resumes. |

---

## 16. Open decisions (need the user's call before/along the way)

1. **Competitor redistribution.** Link-and-hash only (operator downloads), or
   do we vendor `es.exe` into the repo / a release asset? Default in this plan:
   **link + hash, never vendor** until a license review says otherwise (§12).
2. **Pinned Everything version.** Resolve the drift (`1.1.0.30` vs `1.5.x`).
   Which single version is canonical going forward?
3. **True-cold OS page cache.** Do we standardize on an external tool
   (RAMMap / `EmptyStandbyList.exe`) for `--drop-os-cache`, or keep "cold" defined
   only as UFFS-cache-purged (current methodology)? Default: keep current,
   `--drop-os-cache` stays opt-in + manual.
4. **Bundle location.** `LOG/bench/<stamp>/` (proposed) vs a top-level
   `benchmarks-out/`. Must respect the LOG-dir convention in the repo rules.
5. **Default drive set.** Auto-detect all fixed NTFS volumes, or require the
   operator to pass `--drives` explicitly to avoid surprise long runs?

---

## 17. Acceptance criteria (definition of done)

- A third party runs `just bench-suite` on their machine and gets a complete,
  self-describing bundle with **zero hand-typed** environment data.
- Cross-tool cells are **automatically** restricted to drives every required
  tool can serve; UFFS-only cells are labeled with a per-cell reason.
- `fingerprint-after == fingerprint-before` on every successful run; any crumb
  fails the run and is recoverable via `just bench-restore`.
- A re-run is **resumable**: `state.json` skips every `Done` step, and a mid-run
  kill is recovered without redoing completed work.
- Tools the suite acquires honor a **Keep/Remove** disposition; pre-existing host
  resources are never deleted; both are logged in `state.json`.
- All three raw logs (§1/§2/§3) are **script-reproducible** — no manual
  PowerShell paste step remains.
- Every script and doc cites **one** pinned competitor version from
  `competitors.toml`.
- `--dry-run` and the §13.3 self-test run green via `cargo nextest` on non-Windows.

---

*End of plan. Implementation begins at Phase P1 only after this document and
the §16 open decisions are signed off.*
