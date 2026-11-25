# Performance

UFFS is designed to search millions of files in milliseconds.  This page
documents measured performance across seven NTFS drives totalling
**25.9 million records**, captured on real hardware with the standard
benchmark and profiling scripts.

> **Competitive benchmark report:** for the story-shaped, dated
> head-to-head against Everything and the UFFS C++ reference, see the
> [**benchmark hub**](../benchmarks/) — specifically the current
> canonical report
> [`2026-06-v0.5.120-vs-everything.md`](../benchmarks/2026-06-v0.5.120-vs-everything.md)
> (the April v0.5.66 snapshot is
> [archived](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md)).
> This page focuses on UFFS's own per-drive and per-phase numbers; the
> deep-dive sections below are dated forensic captures and carry their
> version tags.

## What these numbers mean

This page intentionally separates **readiness** from **interactive query latency**.

- **COLD** measures raw MFT read, parse, compact index build, and cache write.
- **WARM CACHE** measures daemon restart from the serialized cache.
- **HOT** measures interactive search against a running in-memory daemon.

Unless explicitly noted otherwise, the headline tables on this page measure **end-to-end CLI latency** for the query `*` with `--limit 100`. That means the HOT numbers are **interactive top-N timings**, not full-result export timings. Daemon-side search time is reported separately from process spawn, IPC, and stdout formatting.

This separation is deliberate. Different tools and interfaces optimize different workloads. UFFS therefore treats readiness, interactive search, bulk retrieval, and scale ceiling as separate benchmark classes instead of forcing them into one number.

## Benchmark classes and fairness rules

When UFFS publishes cross-tool benchmarks, the rules are:

1. Compare like-for-like workloads only.
2. Report exact hardware, OS build, tool versions, settings, and query shape.
3. Keep interactive top-N and bulk export benchmarks separate.
4. Report daemon-side timings and end-to-end client timings separately.
5. Record crashes, timeouts, OOMs, interface limits, and incomplete results as **DNF**, not as missing data.

A benchmark is only useful if readers can see both the fastest successful run and the point where a tool stops being operational.

## 1. Test System

| Component | Specification |
|-----------|--------------|
| OS | Windows 11 Pro 64-bit (24H2 / Build 26100) |
| CPU | AMD Ryzen 9 3900XT — 12 cores / 24 threads (Matisse, 7 nm) |
| RAM | 64 GB Dual-Channel DDR4 @ 1312 MHz |
| Motherboard | ASUS ProArt B550-CREATOR (AM4) |
| NVMe SSD | Samsung SSD 990 PRO 2 TB (C:, F:) |
| SATA SSD | Samsung SSD 980 PRO 1 TB (D:, E:) |
| SATA HDD | WD 8 TB × 2 (WDC WD82PURZ, M: and S:) |
| USB storage | SanDisk Extreme 58 GB USB stick (G:) |
| Power profile | AMD Ryzen High Performance |
| UFFS version | 0.5.120 (latest cross-tool + full-scan-export captures; older per-phase tables carry their version tags — see §11 for current cross-tool numbers) |

### Drives Under Test

| Drive | Type | Records | Description |
|-------|------|--------:|-------------|
| C: | NVMe SSD | 3,510,866 | Windows system drive |
| D: | SATA SSD | 7,066,019 | Data drive (Dropbox, projects) |
| E: | SATA SSD | 2,929,519 | Media / archive |
| F: | NVMe SSD | 2,221,343 | Secondary Windows install |
| G: | USB stick | 15,090 | SanDisk Extreme 58 GB |
| M: | SATA HDD | 1,908,805 | WD 8 TB spinning disk (WDC WD82PURZ) |
| S: | SATA HDD | 8,278,102 | WD 8 TB spinning disk (WDC WD82PURZ) |
| **ALL** | **Mixed** | **25,929,744** | **All 7 drives in parallel** |

> **Corpus note:** The 25.9M-record benchmark corpus is a live working Windows machine, so per-drive record counts can drift slightly between runs as files are created, deleted, or updated. Minor differences of a few thousand records across tables reflect different benchmark passes on the same system, not different methodology.
>
> **Scale-ceiling note:** The 42.5M–100.4M tiers are constructed by adding offline MFT clones to the live 7-drive corpus on the same machine. They are scale-ceiling workloads, not additional live volumes.

## 2. The Three-Phase Model

Every UFFS search goes through a startup phase before the first query
can be answered.  The cost depends on how "warm" the system is:

```
Phase 1: COLD                 Phase 2: WARM CACHE           Phase 3: HOT
─────────────────────         ─────────────────────         ──────────────
Kill daemon                   Kill daemon                   Daemon running
Delete cache files            Cache files stay on disk      Index in memory
                              ↓                             ↓
Read raw MFT from disk        Deserialize .iocp cache       Query directly
Parse → build index           → build index                 ↓
Write .iocp cache             ↓                             Results
↓                             Results                       (17 ms–1.1 s CLI e2e)
Results
(seconds to minutes)          (~0.6–6.9 s)
```

| Phase | What happens | When it occurs |
|-------|-------------|----------------|
| **COLD** | Daemon reads raw NTFS MFT, parses every record, builds in-memory index, writes cache | First run ever, or after `daemon kill` + cache deletion |
| **WARM CACHE** | Daemon loads serialized `.iocp` cache from disk — skips expensive MFT parse | Daemon restart (reboot, manual kill) with cache intact |
| **HOT** | Daemon already running, index in memory — pure query execution | Every search after the first one |

## 3. Per-Drive Results

All timings are wall-clock, end-to-end (process spawn → exit), averaged
over 3 rounds.  Pattern: `*` (full scan), limit: 100 rows.

### Cold Start (Raw MFT Read)

| Drive | Records | Total | Startup | Search |
|-------|--------:|------:|--------:|-------:|
| G: | 15,094 | 1.3 s | 540 ms | <1 ms |
| F: | 2,221,347 | 4.3 s | 3.6 s | 13 ms |
| C: | 3,512,541 | 7.7 s | 6.8 s | 30 ms |
| M: | 1,908,809 | 26.4 s | 24.9 s | 11 ms |
| D: | 7,066,020 | 28.6 s | 27.1 s | 94 ms |
| E: | 2,929,523 | 42.5 s | 41.7 s | 18 ms |
| S: | 8,278,106 | 67 s | 65 s | 99 ms |
| **ALL** | **25,931,436** | **66 s** | **65 s** | **235 ms** |

> **Note:** M: and S: are SATA spinning disks where I/O is the
> bottleneck — raw MFT reads are bound by HDD seek time, not CPU.
> NVMe drives (C:, F:) achieve 470K+ records/sec.
> The ALL-drives cold start runs all drives in parallel, so total time
> ≈ slowest individual drive.

### Warm Cache (Serialized .iocp / .uffs Load)

**v0.5.4 per-drive table (historical):**

| Drive | Records | Total | Speedup vs Cold |
|-------|--------:|------:|----------------:|
| G: | 15,094 | 572 ms | 2.3× |
| F: | 2,221,347 | 1.4 s | 3.1× |
| M: | 1,908,809 | 1.4 s | 19.5× |
| E: | 2,929,523 | 2.4 s | 18.0× |
| C: | 3,512,541 | 6.4 s | 1.2× |
| D: | 7,066,020 | 6.4 s | 4.4× |
| S: | 8,278,106 | 4.8 s | 14.0× |
| **ALL (v0.5.4)** | **25,931,436** | **6.9 s** | **9.6×** |
| **ALL (v0.5.62)** | **25,931,436** | **5.7 s** | **12.0×** |

On v0.5.62 the all-drives warm restart dropped 17 % to **5.7 s** (measured
n=1 in [`docs/benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt:369-391`](../benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt)).  Per-drive warm numbers not re-captured on v0.5.62
yet — they are expected to scale proportionally.

### Hot (In-Memory Query)

**v0.5.4 per-drive (historical, before the thin client):**

| Drive | Records | Total | Cold→Hot |
|-------|--------:|------:|---------:|
| G: | 15,094 | 6 ms | **219×** |
| M: | 1,908,809 | 18 ms | **1469×** |
| F: | 2,221,347 | 19 ms | **229×** |
| E: | 2,929,523 | 24 ms | **1771×** |
| C: | 3,512,541 | 27 ms | **284×** |
| D: | 7,066,020 | 49 ms | **584×** |
| S: | 8,278,106 | 54 ms | **1236×** |
| **ALL** | **25,931,436** | **163 ms** | **407×** |

The v0.5.66 numbers for the same patterns (7-drive daemon, `--limit 100`,
30 rounds) are in §5.

> **HOT v0.5.4 vs v0.5.66 in one sentence:** daemon-side targeted
> latency is unchanged (0–3 ms), but CLI end-to-end now has a ~28 ms
> Windows process-creation floor that v0.5.4 did not report (the
> thin client shaved the cold-spawn from ~50 ms to ~28 ms;
> per-process startup now dominates sub-30 ms queries).  On v0.5.120
> that floor has dropped further to **17–18 ms** (see §11).  The
> `*` top-100 path has separately regressed from 163 ms to 1 112 ms
> and is a known regression with a planned fix (a bounded-heap top-N) —
> see [Known regressions](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md#known-regressions-published-because-trust--hype)
> in the archived v0.5.66 report.


## 4. Speedup Summary

The table below shows end-to-end speedup from cold start to hot query
for each drive.  The Cold→Hot ratio is the primary performance metric.

**v0.5.4 per-drive table (historical, `--limit 100` interactive workload):**

| Drive | Cold | Warm | Hot | Cold→Hot | Cold→Warm |
|-------|-----:|-----:|----:|---------:|----------:|
| C: | 7.7 s | 6.4 s | 27 ms | **284×** | 1.2× |
| D: | 28.6 s | 6.4 s | 49 ms | **584×** | 4.4× |
| E: | 42.5 s | 2.4 s | 24 ms | **1771×** | 18.0× |
| F: | 4.3 s | 1.4 s | 19 ms | **229×** | 3.1× |
| G: | 1.3 s | 572 ms | 6 ms | **219×** | 2.3× |
| M: | 26.4 s | 1.4 s | 18 ms | **1469×** | 19.5× |
| S: | 67 s | 4.8 s | 54 ms | **1236×** | 14.0× |
| **ALL (v0.5.4)** | **66 s** | **6.9 s** | **163 ms** | **407×** | **9.6×** |
| **ALL (v0.5.62)** | **68.5 s** | **5.7 s** | see §5 / §11 | — | **12.0×** |

> On spinning disks (M:, S:) the cold-start penalty is extreme —
> reading raw MFT from a HDD is 10–60× slower than NVMe.
> The daemon eliminates this entirely: once loaded, every drive
> responds in **6–54 ms** regardless of media type.

## 5. HOT Query Patterns

Different search patterns exercise different code paths in the query
engine.  The benchmark tests eight representative patterns against a
hot daemon across all drives (25.9 M records, 30 rounds).

**v0.5.4 historical:**

| Pattern | e2e p50 | e2e p95 | daemon p50 | daemon p95 |
|---------|--------:|--------:|-----------:|-----------:|
| `*` (full scan) | 161 ms | 183 ms | 152 ms | 172 ms |
| `notepad.exe` (exact) | 9 ms | 9 ms | 0 ms | 0 ms |
| `win*` (prefix) | 10 ms | 10 ms | 1 ms | 1 ms |
| `*.dll` (extension) | 9 ms | 10 ms | 1 ms | 1 ms |
| `config` (substring) | 10 ms | 11 ms | 1 ms | 1 ms |
| date filter | 152 ms | 156 ms | 143 ms | 147 ms |
| size filter | 153 ms | 160 ms | 144 ms | 150 ms |
| combined | 9 ms | 10 ms | 0 ms | 0 ms |

**v0.5.66 (30 rounds, `--limit 100`, [`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:573-707`](../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt)):**

| Pattern                                | CLI e2e p50 | CLI e2e p95 | Daemon-side |
|----------------------------------------|------------:|------------:|------------:|
| `*` (full scan, top-100)               | **1 112 ms**|  1 163 ms   |   1 081 ms  |
| `notepad.exe` (exact)                  |      29 ms  |     34 ms   |       0 ms  |
| `win*` (prefix)                        |      31 ms  |     34 ms   |       1 ms  |
| `*.dbt` (ext_rare)                     |      32 ms  |     36 ms   |       0 ms  |
| `*.dll` (extension, 167 K match)       |      69 ms  |     75 ms   |      42 ms  |
| `config` (substring)                   |      31 ms  |     34 ms   |       1 ms  |
| `>.*\.(jpg\|png\|heic)$` (regex alt)   |     135 ms  |    148 ms   |     108 ms  |
| `*system32*` (in-path heavy)           |      30 ms  |     33 ms   |       0 ms  |

> **Observations (v0.5.66):**
> - **Daemon-side is unchanged from v0.5.4** — targeted patterns
>   (exact / prefix / ext / substring / in-path) complete in **0–3 ms**
>   daemon-side.
> - CLI end-to-end adds ~28 ms for the cold-spawn floor of the thin client;
>   the thin-client already shaved this from ~50 ms on v0.5.4 era, but
>   Windows per-process startup remains the dominant cost for sub-30 ms
>   queries.
> - `*` top-100 is now **1 112 ms** (vs 161 ms v0.5.4) — a regression
>   introduced when the top-N modified-sort path was rewritten; a
>   bounded-heap top-N fix is planned (see
>   [Known regressions](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md#known-regressions-published-because-trust--hype)).
> - `*.dll` (167 K matches) is 42 ms daemon-side — dominated by the
>   per-row path-resolution walk, not filtering.
> - `regex alternation` is 108 ms daemon-side — scans the MFT for the
>   full regex rather than hitting the extension fast path (the
>   rewrite to the fast path needs a trailing `$` anchor).

## 6. Profile Internals

The `--profile` flag breaks down where time is spent inside the daemon.

**v0.5.4 historical breakdown (before the thin client):**

| Component | Time | Notes |
|-----------|-----:|-------|
| Process spawn | ~8 ms | OS creates `uffs.exe` process |
| IPC connect | 1 ms | Named pipe handshake |
| Daemon search | 155 ms | Scan 25.9M records across 7 drives |
| IPC transfer | <1 ms | Send result rows back |
| Row conversion | <1 ms | Deserialize rows |
| Output formatting | ~5 ms | Format and write to stdout |
| **Total** | **~163 ms** | |

**v0.5.66 breakdown (`*` --limit 100 on 26.1 M records, 30 rounds,
[`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:657-664`](../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt)):**

| Component          | Time (v0.5.66) | Notes |
|--------------------|--------------:|-------|
| CLI cold-spawn     | ~28 ms         | Thin client + Windows process start |
| IPC connect        | <1 ms          | Named pipe handshake (shmem fast path) |
| Daemon search      | **1 081 ms**   | Top-N modified-sort scanning full 26 M rows — regression target |
| IPC transfer       | <1 ms          | 100 rows back |
| Row conversion     | <1 ms          | Deserialize |
| Output formatting  | ~2 ms          | Format + CSV write |
| **Total**          | **~1 112 ms**  | |

> The v0.5.4 155 ms daemon figure and the v0.5.66 1 081 ms daemon
> figure describe the same code path (`uffs * --limit 100` on 26 M
> records) but the top-N rewrite materialised the full sort-key set
> instead of a bounded-heap top-N.  The fix is planned (see
> [Known regressions](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md#known-regressions-published-because-trust--hype));
> estimated after it: ~150–200 ms.
>
> Targeted queries (`notepad.exe` / `win*` / `*.dll` / `config` /
> `*system32*`) skip the full scan and return in **0–3 ms daemon-side**
> on v0.5.66 — **unchanged from v0.5.4**.  The `*` in-memory scan rate
> is still ~167 M rec/s when not materialising; end-to-end CSV export
> of the full estate is **≈ 1.95 M rec/s on v0.5.120** (23.3 M rows in
> 12.0 s; was 1.72 M rec/s on v0.5.66 — see §7).

### Per-Drive Profile (Cold Start)

| Drive | Records | Cold Total | Startup | Search |
|-------|--------:|-----------:|--------:|-------:|
| C: | 3,512,541 | 7.7 s | 6.8 s | 30 ms |
| D: | 7,066,020 | 28.6 s | 27.1 s | 94 ms |
| E: | 2,929,523 | 42.5 s | 41.7 s | 18 ms |
| F: | 2,221,347 | 4.3 s | 3.6 s | 13 ms |
| G: | 15,094 | 1.3 s | 540 ms | <1 ms |
| M: | 1,908,809 | 26.4 s | 24.9 s | 11 ms |
| S: | 8,278,106 | 67 s | 65 s | 99 ms |

> Cold-start time is dominated by **MFT read + parse** (the "Startup"
> column).  Search itself is always <100 ms even on 8M records.

## 7. Bulk Retrieval Throughput

Bulk retrieval measures how fast UFFS can export large result sets.

### Full-Scan CSV Export: v0.5.120 current (all 7 drives, `*` → file, HOT, p50, n=10)

Source: [`docs/benchmarks/raw/2026-06-v0.5.120_full-scan-all-drives.csv`](../benchmarks/raw/2026-06-v0.5.120_full-scan-all-drives.csv)
(`--hide-system --hide-ads`, end-to-end through the daemon pipe):

| Drive | Wall (p50) | Rows written | Throughput |
|-------|-----------:|-------------:|-----------:|
| C: | 1.59 s | 3,295,508 | 2.08 M rows/s |
| D: | 2.26 s | 4,772,519 | 2.11 M rows/s |
| E: | 1.44 s | 2,928,074 | 2.03 M rows/s |
| F: | 0.93 s | 2,124,007 | 2.29 M rows/s |
| G: | 0.03 s | 15,126 | — (floor-bound) |
| M: | 0.84 s | 1,908,750 | 2.28 M rows/s |
| S: | 3.83 s | 8,278,062 | 2.16 M rows/s |
| **All 7** | **11.98 s** | **23,322,046** | **≈ 1.95 M rows/s** |

The complete estate streams to CSV in **12.0 s** — the largest single
drive (S:, 8.3 M rows) exports in **3.83 s**, where the v0.5.4 harness
below measured 25.6 s for the same drive (~6× improvement).

### CSV Export Tiers: v0.5.4 historical (7 drives, 25.9M records, `--out-dir`)

Kept for the tier-scaling shape; absolute rates are superseded by the
v0.5.120 table above.

| Tier | Rows | Avg Time | Rows/sec |
|------|-----:|---------:|---------:|
| 100 | 101 | 213 ms | 474/s |
| 1k | 1,001 | 202 ms | 5.0k/s |
| 10k | 10,001 | 323 ms | 31k/s |
| 100k | 100,001 | 1.4 s | 73k/s |
| 1M | 1,000,001 | 3.4 s | 292k/s |
| ALL (per-drive) | 8.3M | 25.6 s | **326k/s** |

### Pipe vs Direct File Write (v0.5.4 historical)

| Mode | 8.3M rows | Rows/sec | Relative |
|------|----------:|---------:|---------:|
| Pipe (stdout) | 68 s | 122k/s | 1.0× |
| `--out-dir` | 25.6 s | 323k/s | **2.6×** |

> **Recommendation:** For exports exceeding ~100k rows, write to a file
> (`--out` / `--out-dir`) to bypass the shell pipe bottleneck.  The
> direct-write advantage still holds on the current pipeline; absolute
> rates are the v0.5.120 table above.

### CSV vs JSON

Format makes no material difference to throughput.  Both CSV and JSON
achieve comparable rows/sec at each tier — the bottleneck is query
evaluation and IPC, not serialization.

## 8. Scale Ceiling

The scale ceiling test loads progressively larger MFT collections
(cloned offline drives + live drives) and measures interactive
search latency at each tier.

### v0.5.4 scale-ceiling (with offline MFT clones, not re-verified on v0.5.66)

| Total Records | Drives | `*` e2e p50 | `*` e2e p95 | targeted p50 | Status |
|--------------:|-------:|------------:|------------:|-------------:|--------|
| 25.9M | 7 | 161 ms | 183 ms | 9–10 ms | ✅ |
| 42.5M | 9 | 259 ms | 312 ms | 9–10 ms | ✅ |
| 59.0M | 11 | 471 ms | 502 ms | 10–12 ms | ✅ |
| 75.6M | 13 | 600 ms | 626 ms | 10–12 ms | ✅ |
| 92.2M | 15 | 670 ms | 731 ms | 11–14 ms | ✅ |
| **100.4M** | **16** | **808 ms** | **855 ms** | **11–13 ms** | **✅** |
| >100M | 17+ | — | — | — | ❌ OOM |

### v0.5.66 drive-accumulation sweep (real drives only, [`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:933-1044`](../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt))

The v0.5.66 re-bench did **not** use offline MFT clones (that tooling
has not been re-ported to the new data format).  Instead, drives were
added one at a time up to the real 26 M-record envelope:

| Total Records | Drives              | Daemon RSS | `*` e2e (n=1) | MB / M records |
|--------------:|---------------------|-----------:|--------------:|---------------:|
| 3.67 M        | C                   |   777 MB   |   1 416 ms    |    212 |
| 10.74 M       | C,D                 | 2 112 MB   |   3 407 ms    |    197 |
| 13.67 M       | C,D,E               | 2 587 MB   |   5 304 ms    |    189 |
| 15.89 M       | C,D,E,F             | 3 059 MB   |   6 264 ms    |    192 |
| 15.91 M       | C,D,E,F,G           | 3 063 MB   |   6 178 ms    |    193 |
| 17.81 M       | C,D,E,F,G,M         | 3 351 MB   |   7 069 ms    |    188 |
| **26.09 M**   | **C,D,E,F,G,M,S**   | **4 722 MB**| **15 176 ms** |    **181** |

> **Key insights:**
> - **Memory scales linearly at 180–212 MB / million records**; the
>   per-record cost *drops* as drives are added (shared overhead
>   amortises).
> - **`*` scan time scales roughly linearly** with record count until
>   the S: drive (8.3 M records) where CSV write-out becomes the
>   limit.
> - **Targeted queries stay at 0–3 ms daemon-side** regardless of
>   corpus size (v0.5.66 confirmed in §5 above; v0.5.4 synthetic-clone
>   result at 100.4 M extrapolates the same shape).
> - **v0.5.66 `*` at 26 M is 15.2 s** (wall time) vs v0.5.4's
>   implied extrapolation of ~300 ms (808 ms × 26/100 ≈ 210 ms).
>   This is the same 6.8× `*` --limit 100 regression flagged in §5.
> - The OOM at >100 M records (v0.5.4) is a memory ceiling on this
>   test machine (64 GB DDR4); at 181 MB / M rec the headroom is ~350 M
>   records, so the 100 M ceiling was set by per-record cost being
>   higher on v0.5.4 (~640 B in the older DataFrame layout).

## 9. Validation Suite Throughput

UFFS ships three validation suites that double as performance
benchmarks for the query engine under realistic workloads.  All suites
run against a hot daemon loaded with 25.9M records across 7 drives.

Latest figures from the v0.5.62 validation run (hot daemon, 25.9M records across 7 drives). The raw capture of this run is not published (it is too large), so these figures cannot be re-checked from the repository.

### CLI Validation (248 tests, parallel)

| Metric | Value |
|--------|------:|
| Parallelism | 24 concurrent |
| Wall time | **16.3 s** |
| Sum CPU time | 176.2 s |
| Avg per test | **710 ms** |
| Slowest | 3,603 ms (duplicates verify=hash) |
| Fastest | 64 ms (simple search) |
| Pass rate | **248/248 (100%)** |

### API Validation (227 tests, parallel)

| Metric | Value |
|--------|------:|
| Parallelism | 24 concurrent |
| Wall time | **12.2 s** |
| Sum CPU time | 143.3 s |
| Avg per test | **631 ms** |
| Slowest | 1,598 ms (`--attr !system`) |
| Fastest | <1 ms (status RPC) |
| Pass rate | **227/227 (100%)** |

### MCP Validation (254 tests, parallel)

| Metric | Value |
|--------|------:|
| Parallelism | 24 concurrent (over one MCP session) |
| Wall time | **11.3 s** |
| Sum CPU time | 257.6 s |
| Avg per test | **1,014 ms** |
| Slowest | 2,137 ms (`--min-name-length 50`) |
| Fastest | <1 ms (protocol version) |
| Pass rate | **254/254 (100%)** |

> **Total:** 729 tests across CLI, API, and MCP — all pass, all
> exercising the same hot daemon with 25.9M records.  On v0.5.62,
> CLI wall time dropped from 65.7 s to **16.3 s** (−75 %) — largely
> because the per-test CLI tax fell from ~160 ms to ~28 ms after
> the Run 10/11 RPC-consolidation and watchdog fixes.

## 10. Daemon Runtime Statistics

After a v0.5.62 session (from [`docs/benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt:392-410`](../benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt)):

| Metric | v0.5.4 | v0.5.62 |
|--------|-------:|--------:|
| Startup (warm cache, all 7 drives) | 3.7 s | **5.7 s** |
| Total records | 25,922,252 | 25,991,693 |
| Index heap (sum of drives) | — | **4,662 MB** |
| Working set (RSS) | — | **4.99 GB** |
| Private memory | — | 5.10 GB |
| Virtual memory | — | 19.52 GB |
| Cache deserialization throughput | 7.0 M rec/s | **4.6 M rec/s** |

> Warm-start on v0.5.62 is **5.7 s** for all 7 drives (25.9 M records).
> That's an improvement vs the §3 v0.5.4 number of 6.9 s (−1.2 s / −17 %);
> the earlier §10 v0.5.4 figure of 3.7 s was a best-case measurement on a
> pre-warmed OS page cache and should not be compared directly against
> the v0.5.62 cold-OS-cache measurement.  Settled memory footprint
> dropped from ~6 GB to **4.99 GB (−17 %)** thanks to mimalloc +
> `mi_collect(true)` (OPT-6) and trigram pruning (OPT-7).

## 11. Cross-Tool vs Everything (v0.5.120, C+D apples-to-apples)

On v0.5.120 UFFS wins **30/30 cells at p50** against Everything
(1.4.1.1032) across C/D/F/G and the combined index — median ratio
**0.36× (~2.8× faster)**. The C+D subset below is the apples-to-apples
continuation of the series this page has tracked since v0.5.66.
Source: [`docs/benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv`](../benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv) (n=10, HOT, file sink); full four-drive table in the
[current canonical report](../benchmarks/2026-06-v0.5.120-vs-everything.md).

| Drive | Pattern         | UFFS p50 | ES p50 | UFFS/ES | Rows    |
|-------|-----------------|---------:|-------:|--------:|--------:|
| C:    | exact           |    20 ms |  69 ms | **0.29×** |      30 |
| C:    | prefix          |    80 ms | 102 ms | **0.78×**¹ |  38 285 |
| C:    | ext_rare        |    19 ms |  59 ms | **0.32×** |       1 |
| C:    | ext_dll         |    96 ms | 237 ms | **0.41×** | 166 684 |
| C:    | **ext_regex_alt** | **30 ms** | 82 ms | **0.37×** |  18 085 |
| C:    | substring       |    39 ms | 105 ms | **0.37×** |  25 320 |
| D:    | exact           |    20 ms |  67 ms | **0.30×** |       3 |
| D:    | prefix          |    39 ms |  75 ms | **0.52×** |   8 732 |
| D:    | ext_rare        |    19 ms |  61 ms | **0.31×** |      11 |
| D:    | ext_dll         |    37 ms | 117 ms | **0.32×** |  44 529 |
| D:    | **ext_regex_alt** | **26 ms** | 74 ms | **0.35×** |  10 438 |
| D:    | substring       |    35 ms |  85 ms | **0.41×** |  12 458 |

¹ `C:prefix` was a statistical tie in every snapshot through v0.5.66
(99 ms vs 97 ms); on v0.5.120 it is a clear win.

Every one of these 12 cells improved over the
[archived v0.5.66 snapshot](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md)
— median **−33%** (substring C: 67 → 39 ms) — while Everything's own
numbers held roughly flat. The per-cell v0.5.66 → v0.5.120 deltas are in the
[current report](../benchmarks/2026-06-v0.5.120-vs-everything.md#vs-the-previous-published-snapshot-v0566-2026-04);
the full v0.5.66 table (p50 and p95, both tools) is in the
[archived report](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md#head-to-head-1-uffs-vs-everything-interactive-queries).

> **Note:** The 17–20 ms UFFS floor on every small-result cell is the
> Windows CLI process-creation + pipe tax — exposed directly by the
> G-drive empty-result cells in the same v0.5.120 capture (17–18 ms
> p50 for zero rows).  It was ~28 ms on v0.5.66 (see the
> [archived report](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md#the-targeted-query-fast-path-measured-at-26-m-records)).
> The daemon itself responds in 0–3 ms on targeted queries.

## 12. C++ vs Rust Parity Comparison

UFFS was rewritten from C++ to Rust.  The parity test runs both
implementations on the same drives and compares output.  The Rust
implementation reads raw MFT cold (no cache, no daemon), while the
C++ baseline runs warm:

| Drive | C++ (warm) | Rust (cold) | Ratio | Rust records/sec |
|-------|----------:|------------:|------:|-----------------:|
| C: | 12.4 s | 17.4 s | 1.40× slower | 201,658 |
| D: | 39.8 s | 47.1 s | 1.18× slower | 150,015 |
| E: | 43.6 s | 48.8 s | 1.12× slower | 59,998 |
| F: | 7.0 s | 11.0 s | 1.57× slower | 202,343 |
| M: | 24.1 s | 31.7 s | 1.31× slower | 60,160 |
| S: | 1m 1.6 s | 1m 26.8 s | 1.41× slower | 95,326 |
| **TOTAL** | **3m 8.6 s** | **4m 2.9 s** | **1.29× slower** | **106,695** |

> **Context:** The C++ times are warm (OS has cached MFT pages); the
> Rust times are cold (MFT read from disk + full parse + cache write).
> With the daemon running (HOT), Rust answers the same queries in
> **17–39 ms CLI end-to-end for targeted single-drive queries** (v0.5.120;
> v0.5.66 measured 29–32 ms — daemon-side 0–3 ms + the Windows
> cold-spawn tax).  Unfiltered `*` with
> `--limit 100` is 1 112 ms CLI e2e on v0.5.66 (was 163 ms on v0.5.4
> — a [known regression](../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md#known-regressions-published-because-trust--hype);
> not re-measured since).
> The C++ tool re-reads the MFT on every invocation; the Rust daemon
> never needs to re-read after the initial cold build — on v0.5.120 that
> gap measures **180×–3 400× for targeted queries** (see the
> [current canonical report](../benchmarks/2026-06-v0.5.120-vs-everything.md)).

## 13. Running Your Own Benchmarks

UFFS includes two profiling scripts in `scripts/windows/`:

### Full Benchmark (`benchmark.rs`)

Three-phase benchmark with multi-round statistics, per-drive isolation,
and multi-pattern HOT testing:

```bash
# Default: all drives, 3 rounds, patterns: *, *.txt, test
rust-script scripts/windows/benchmark.rs

# Specific drives, more rounds
rust-script scripts/windows/benchmark.rs --drives C,D --rounds 5

# HOT phase only with custom patterns
rust-script scripts/windows/benchmark.rs --phase hot --pattern "*.dll" --pattern "config"

# Non-Windows with offline MFT data
rust-script scripts/windows/benchmark.rs --data-dir ~/uffs_data
```

### Profile Script (`profile.rs`)

Detailed `--profile` output for each phase, showing daemon internals:

```bash
# Profile all drives
rust-script scripts/windows/profile.rs --drives C,D,E,F,G,M,S

# Single drive
rust-script scripts/windows/profile.rs --drives C
```

### Quick One-Off Profile

```bash
# Profile a single search
uffs "*.dll" --profile

# Benchmark mode (suppress output, measure engine only)
uffs "*.dll" --benchmark --limit 5
```

## See also

- [Advanced Diagnostics](advanced-diagnostics.md): profiling, logging and environment variables
- [Daemon](daemon.md): the resident index server and how to manage it
- [Cache & Data Sources](cache-and-data.md): live drives, MFT captures and the index cache
- [Concepts](concepts.md): what each size, count and attribute means
