# UFFS Benchmarks

**Benchmark-driven NTFS search.** Every number on this page is backed by a raw log, a reproducible script, and a dated frozen report. Nothing here is evergreen — every claim has a version and a date attached to it.

---

## Current canonical report

**[2026-06 · v0.5.120 vs Everything and the UFFS C++ reference →](2026-06-v0.5.120-vs-everything.md)**

![UFFS v0.5.120 wins 30 of 30 head-to-head cells against Everything at p50](charts/2026-06-v0.5.120/head-to-head-vs-everything.svg)

![UFFS daemon HOT vs C++ per-invocation MFT re-read](charts/2026-06-v0.5.120/daemon-hot-vs-cpp.svg)

![Full-scan export: 23.3 M records to CSV in 12.0 s at 1.95 M records per second](charts/2026-06-v0.5.120/full-scan-throughput.svg)

Four numbers the report establishes on a Ryzen 9 3900XT (cross-tool: 12.8 M records on C/D/F/G; full-scan: all 7 volumes, 25.9 M records):

1. **30 / 30 head-to-head cells faster than Everything** at p50 across six pattern classes (exact, prefix, rare-ext, common-ext, regex-alternation, substring) on four drives plus the combined index. Median ratio **0.36× — UFFS is ~2.8× faster**. The historical `C: prefix` statistical tie is gone (80 ms vs 102 ms, 0.78×).
2. **Every cell published in the previous (v0.5.66) snapshot got faster — median −33%** — while Everything's own numbers held roughly flat. The gap widened, not narrowed.
3. **Full-scan export is a workload Everything cannot run** (`es.exe` ~2 GB IPC export ceiling): UFFS streams the complete **23.3 M-row** estate (all 7 volumes) to CSV in **12.0 s ≈ 1.95 M records/sec** — the April snapshot scale, 12% faster, +13% throughput.
4. **180×–3 400× faster than the C++ reference on targeted queries** (daemon HOT vs per-invocation MFT re-read); **6.6×** on combined full-scan — and the combined-drive regex cell DNF'd the C++ tool entirely (> 120 s vs UFFS 43 ms).

The report publishes **everything these numbers don't cover too** — the zero-match G-drive caveat, the C++ row-count divergences, and what this benchmark explicitly does *not* claim. The two v0.5.66-era known regressions (`*` top-100 and `--sort path` vs the v0.5.4 baseline) remain tracked in the [archived April report](archive/2026-04-v0.5.66-vs-everything-and-cpp.md#known-regressions-published-because-trust--hype); they were not re-measured in this snapshot.

---

## How UFFS benchmarks

**Ready to run a benchmark cycle?** See the **[operator runbook →](runbook.md)** for prerequisites,
step-by-step commands, crash recovery, and how to promote results to a canonical report.

Four fairness principles, documented in full in [`methodology.md`](methodology.md) (the single-link
reply to *"this comparison is rigged because..."*):

- **Separate cold / warm / hot.** Cold build + warm restart + hot query are three different workloads. We measure and publish them separately instead of averaging them into one "startup time" lie.
- **Separate interactive from bulk.** Targeted-query latency (`notepad.exe`, `*.dll`) and full-scan export (`*` → CSV for 23 M rows) are different workload classes. Different tools win each. We test both.
- **Publish the failures.** When a workload regresses against our own prior baseline it gets named, measured, root-caused, and tracked (see §Known regressions in the [archived 2026-04 report](archive/2026-04-v0.5.66-vs-everything-and-cpp.md#known-regressions-published-because-trust--hype) for the two v0.5.66-era examples).
- **Publish the raw data.** Every table above and in the canonical report cites the exact log file and line range. The **curated, verbatim raw captures** live in [`raw/`](raw/) (git-tracked, never edited after commit); all benchmark scripts under [`scripts/windows/`](../../scripts/windows/). Click any citation in the canonical report to land on the actual PowerShell log line that produced the number.

---

## Reproduce

Elevated PowerShell, repository root, after `cargo build --release`:

```powershell
# The full benchmark suite: drive negotiation (Everything RAM budget), the
# cross-tool harness (UFFS Rust vs UFFS C++ vs Everything), parity, native
# full-suite, REPORT-DRAFT.md assembly, and the brand-kit charts.
just bench-suite --drives C,D,F,G
```

The suite writes a dated bundle containing `REPORT-DRAFT.md`, `cross-tool-summary.csv`, the three competition charts, and a `## vs baseline` section comparing the run against this page's current canonical numbers ([`baseline.json`](baseline.json)). Promotion into a canonical report is the reviewed copy-edit of that draft. Individual harnesses remain runnable standalone — see [`scripts/windows/cross-tool-benchmark.rs`](../../scripts/windows/cross-tool-benchmark.rs).

---

## Archive

Frozen snapshots of prior canonical reports, never retroactively edited. See [`archive/README.md`](archive/README.md) for the archive policy.

- **[2026-04 · v0.5.66 vs Everything and the UFFS C++ reference](archive/2026-04-v0.5.66-vs-everything-and-cpp.md)** — the first canonical snapshot (12/12 cells vs Everything on C+D, median 0.51×; cold-parity and memory-scaling sections; the two tracked regressions). Superseded by the v0.5.120 report above.

---

## What's not in here

- **No "fastest on Windows" superlative.** Different workloads have different winners. UFFS is measurably faster than Everything and the C++ reference on the documented patterns — that's what we claim.
- **No WizFile numbers yet** — WizFile has no automated-comparison flag (`--out` equivalent). When we add harness support, results land here.
- **No "bring your own drives" comparison** — all numbers come from the same test machine. Per-user results will vary with CPU, drive topology, and drive fullness.

For the full positioning story (competitor landscape, claim framework, what to amplify vs qualify), see the internal strategy docs: `docs/dev/architecture/ntfs_mcp_marketing_strategy_deep_dive.md` and `docs/dev/architecture/marketing_strategy_adjustment_after_benchmark_update.md`.
