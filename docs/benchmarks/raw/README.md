# Raw benchmark captures

**Every number in every UFFS benchmark report is backed by one of these files.** The files are verbatim copies of the PowerShell capture logs from the test machine at the moment the canonical report was written. They are **never edited** after commit — if a follow-up run produces different numbers, those numbers go into a *new* file in this directory, not an edit of an existing one.

The full local forensic log directory (`LOG/` in the repo) is intentionally gitignored and contains many more captures than what lives here. This `raw/` directory is the curated, publication-quality subset that every claim in [`docs/benchmarks/`](../) cites.

---

## Files in this directory

| File | Captured | Records | What it contains | Cited by |
|---|---|---|---|---|
| [`2026-06-v0.5.120_cross-tool-summary.csv`](2026-06-v0.5.120_cross-tool-summary.csv) | 2026-06-11 | 12.8 M | 10-round cross-tool benchmark (UFFS Rust vs UFFS C++ vs Everything 1.4.1.1032) across 6 pattern classes + full-scan on drives C/D/F/G and the combined index. Machine CSV: p50/p95/rows/verdict per (tool, drive, pattern) cell. Suite-generated (`just bench-suite`, bundle `bench-20260611T221143Z-v0.5.120`). | [`2026-06-v0.5.120-vs-everything.md`](../2026-06-v0.5.120-vs-everything.md) (all tables), [`09-performance.md`](../../architecture/engine/09-performance.md) §Headline, [`README.md`](../../../README.md) proof strip |
| [`2026-06-v0.5.120_full-suite.csv`](2026-06-v0.5.120_full-suite.csv) | 2026-06-11 | 12.8 M | Stage-3 native UFFS count-sink suite (`all_dlls` / `full_scan` per drive, hot tier, n=10). Note: the `G: all_dlls` cell is invalid — it exposed the empty-extension-bucket `--count` bug fixed 2026-06-11 (an extension filter resolving to no IDs counted every record). | [`2026-06-v0.5.120-vs-everything.md`](../2026-06-v0.5.120-vs-everything.md) provenance footer |
| [`2026-06-v0.5.120_full-scan-all-drives.csv`](2026-06-v0.5.120_full-scan-all-drives.csv) | 2026-06-11 | 25.9 M | Dedicated UFFS-only full-scan capture across **all seven volumes** (`*` → CSV, HOT, file sink, n=10, per-drive + combined). The full-scan workload is UFFS-only (es.exe ~2 GB IPC export ceiling), so it is not constrained by the Everything RAM-budget drive negotiation — this capture restores the full-estate scale figure: 23 322 046 rows in 11.98 s ≈ 1.95 M rec/s. | [`2026-06-v0.5.120-vs-everything.md`](../2026-06-v0.5.120-vs-everything.md) §Full-scan export, full-scan chart, [`README.md`](../../../README.md) proof strip |
| [`2026-06-v0.5.120_parity.txt`](2026-06-v0.5.120_parity.txt) | 2026-06-11 | 12.8 M | Stage-2 per-drive parity transcript (Rust HOT vs C++ MFT re-read, 10 rounds per drive on C/D/F/G; row-count and SHA comparison). | [`2026-06-v0.5.120-vs-everything.md`](../2026-06-v0.5.120-vs-everything.md) §C++ reference |
| [`2026-06-v0.5.120_drives.json`](2026-06-v0.5.120_drives.json) · [`…_env.json`](2026-06-v0.5.120_env.json) · [`…_matrix.json`](2026-06-v0.5.120_matrix.json) · [`…_competitor-preflight.json`](2026-06-v0.5.120_competitor-preflight.json) | 2026-06-11 | — | Stage-0 provenance set: full storage inventory (`uffs-mft drives` — drive kinds, capacities, MFT records), environment fingerprint (CPU/RAM/OS + tool versions incl. the IPC-captured Everything engine version), the negotiated drive matrix, and the per-drive Everything RAM-budget data. Backs the report's Storage devices / Test environment / Negotiated matrix / RAM-budget sections. | [`2026-06-v0.5.120-vs-everything.md`](../2026-06-v0.5.120-vs-everything.md) §Test environment |
| [`2026-04-v0.5.66_cross-tool-vs-everything.txt`](2026-04-v0.5.66_cross-tool-vs-everything.txt) | 2026-04-21 | 26.1 M | 30-round cross-tool benchmark (UFFS Rust vs Everything) across 6 pattern classes on drives C+D. Head-to-head p50/p95 tables. | [`2026-04-v0.5.66-vs-everything-and-cpp.md`](../archive/2026-04-v0.5.66-vs-everything-and-cpp.md) §Head-to-head 1, [`09-performance.md`](../../architecture/engine/09-performance.md) §Benchmark Results |
| [`2026-04-v0.5.66_full-benchmark-suite.txt`](2026-04-v0.5.66_full-benchmark-suite.txt) | 2026-04-21 | 26.1 M | 30-round targeted-query latency sweep (`notepad.exe`, `win*`, `*.dll`, `config`, regex, substring) + drive-accumulation scale sweep (3.67 M → 26.09 M records) + per-drive daemon-hot p50. | [`2026-04-v0.5.66-vs-everything-and-cpp.md`](../archive/2026-04-v0.5.66-vs-everything-and-cpp.md) §Head-to-head 2, §Scale ceiling, [`09-performance.md`](../../architecture/engine/09-performance.md) §Targeted queries + §Drive-accumulation, [`11-performance-deep-dive.md`](../../architecture/engine/11-performance-deep-dive.md) §HOT re-bench |
| [`2026-04-v0.5.66_cold-parity-per-drive.txt`](2026-04-v0.5.66_cold-parity-per-drive.txt) | 2026-04-21 | 26.1 M | Cold-start parity benchmark: Rust cold (daemon spawn + MFT read + compact index build) vs C++ MFT reread, per drive. Both tools writing output to file for fair row counting. Summary table at tail. | [`2026-04-v0.5.66-vs-everything-and-cpp.md`](../archive/2026-04-v0.5.66-vs-everything-and-cpp.md) §Head-to-head 2, [`11-performance-deep-dive.md`](../../architecture/engine/11-performance-deep-dive.md) §Parity Comparison |
| [`2026-04-v0.5.62_aggregate-baseline.txt`](2026-04-v0.5.62_aggregate-baseline.txt) | 2026-04-21 (pre-v0.5.66 run) | 25.9 M | 7-drive aggregate baseline on v0.5.62: COLD, WARM cache, full-scan export, aggregation throughput, daemon RSS, initial cross-tool Everything comparison. The "before" half of every v0.5.66 regression callout. | [`09-performance.md`](../../architecture/engine/09-performance.md) §Workload table, `cross-tool-benchmark-analysis.md` §Current State (internal) |

---

## What's a "raw capture"?

These are exactly what PowerShell recorded during benchmark runs — banner lines, working-directory prefix, occasional typing corrections, all of it. They are not cleaned, summarised, or re-rendered. If a run had a startup glitch or a warm-up anomaly, it shows in the log and the corresponding benchmark table in the canonical report calls it out.

This is deliberate. "Post-processed marketing-friendly output" is exactly the failure mode the benchmark hub is promising *not* to ship. The tables in the published reports are derivative artefacts; these files are the primary sources. If a number in a published table disagrees with what these files show, the files win.

---

## How to reproduce

Elevated PowerShell from repo root, after `cargo build --release`:

```powershell
# cross-tool-vs-everything.txt was produced by:
rust-script .\scripts\windows\cross-tool-benchmark.rs `
    --rounds 30 --tools uffs_rust,es --sinks file `
    --drives C,D

# cold-parity-per-drive.txt was produced by:
.\scripts\windows\cold-parity-per-drive.ps1 `
    -Drives C,D,E,F,M,S -PurgeCacheFirst `
    -OutputFile LOG\my_parity_run.txt

# full-benchmark-suite.txt is the full benchmark battery including:
# - scale-sweep: drive-accumulation via repeated --drive-set invocations
# - targeted-queries: 30-round latency across all 7 drives per pattern
# See the top of the file for the exact PowerShell block used.
```

Per-user results will vary with CPU, drive topology (NVMe vs HDD ratio), filesystem fullness, and Windows Defender configuration. The comparisons *between* tools on any given machine tend to be stable; the absolute numbers drift by hardware.

---

## No-edit policy

These files are append-only at the directory level (new files can be added) and read-only at the file level (existing files are never modified after commit). If UFFS v0.5.70 produces faster numbers, those numbers land in a new `2026-XX-v0.5.70_*.txt` file alongside these. The v0.5.66 files stay exactly as they are.

This is the same policy as [`docs/benchmarks/archive/`](../archive/): primary sources live forever, no retroactive edits, comparison across eras is traceable because both eras are preserved intact.
