# Performance & Benchmarking

## Introduction

This document describes the performance characteristics of UFFS, the optimization techniques employed, and how to benchmark and profile the engine. After reading this document, you should be able to:

1. Understand why UFFS is fast and where time is spent
2. Profile and benchmark specific code paths
3. Identify optimization opportunities

---

## Benchmark Results (current: v0.5.120)

> **Publication-grade competitive benchmark report:** [`docs/benchmarks/`](../../benchmarks/) — dated snapshots, fairness methodology, archive policy, reproduction scripts. The current canonical report is [`2026-06-v0.5.120-vs-everything.md`](../../benchmarks/2026-06-v0.5.120-vs-everything.md).
>
> This engineering-reference doc holds the *raw* cross-drive measurements and per-phase diagnostics used internally. For the story-shaped version with fairness rules, competitor positioning, and TL;DR headline numbers, start at the benchmark hub.

Headline cross-tool result on v0.5.120 (from
[`docs/benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv`](../../benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv), n=10, HOT,
C/D/F/G + combined scope):

**UFFS beats Everything 30/30 at p50**, median ratio **0.36×
(UFFS ~2.8× faster)** — every cell published in the v0.5.66 snapshot
improved, median −33%.  Full table and analysis in
[`docs/benchmarks/2026-06-v0.5.120-vs-everything.md`](../../benchmarks/2026-06-v0.5.120-vs-everything.md) §Head-to-head; the prior v0.5.66 series lives in the [archived April report](../../benchmarks/archive/2026-04-v0.5.66-vs-everything-and-cpp.md) §Head-to-head 1, with the engineering-detail source at `docs/research/cross-tool-benchmark-analysis.md` §Current State (internal).

### Full-scan export — v0.5.120, all 7 drives (`*` → CSV file, HOT, p50, n=10)

Source: [`docs/benchmarks/raw/2026-06-v0.5.120_full-scan-all-drives.csv`](../../benchmarks/raw/2026-06-v0.5.120_full-scan-all-drives.csv)
(`--hide-system --hide-ads`, end-to-end through the daemon pipe):

| Drive | Wall (p50) | Rows written | Throughput |
|-------|-----------:|-------------:|-----------:|
| C: | 1.59 s | 3 295 508 | 2.08 M rec/s |
| D: | 2.26 s | 4 772 519 | 2.11 M rec/s |
| E: | 1.44 s | 2 928 074 | 2.03 M rec/s |
| F: | 0.93 s | 2 124 007 | 2.29 M rec/s |
| G: | 0.03 s | 15 126 | — (USB stick, floor-bound) |
| M: | 0.84 s | 1 908 750 | 2.28 M rec/s |
| S: | 3.83 s | 8 278 062 | 2.16 M rec/s |
| **All 7** | **11.98 s** | **23 322 046** | **≈ 1.95 M rec/s** |

vs the April snapshot (23.4 M rows, 13.6 s): **12% faster wall-clock,
+13% sustained throughput**.  The 4-drive cross-tool capture agrees as a
consistency check (C+D+F+G combined: 10 207 863 rows in 4.83 s,
2.11 M rec/s).

7-drive aggregate numbers on v0.5.62 (from
[`docs/benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt:113-479`](../../benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt)):

| Workload                                         | Value       |
|--------------------------------------------------|------------:|
| COLD start (cache deleted, 25.9 M records)       | 68.5 s      |
| WARM cache restart (25.9 M records)              | **5.7 s**   |
| Full-scan export `*` → CSV file                  | 13.5 s ¹    |
| Aggregation throughput (`by_extension` etc.)     | ~180 ms     |
| Daemon RSS                                       | 4.99 GB     |
| Index heap                                       | 4.66 GB     |

¹ Superseded by the v0.5.120 capture above (11.98 s for the same
estate).  COLD/WARM and aggregation numbers have not been re-measured
since v0.5.62.

### Three-Phase Results — v0.5.4 per-drive table (historical)

Tested on AMD Ryzen 9 3900XT (12c/24t), 64 GB DDR4.  Pattern: `*`,
limit: 100, per-drive profile.  Per-drive re-bench on v0.5.62 not
yet performed; aggregate numbers above are the latest production
measurements.

| Drive | Type | Records | COLD | WARM | HOT | Cold→Hot |
|-------|------|--------:|-----:|-----:|----:|---------:|
| C: | NVMe | 3.5M | 7.7 s | 6.4 s | 27 ms | **284×** |
| D: | SATA SSD | 7.1M | 28.6 s | 6.4 s | 49 ms | **584×** |
| E: | SATA SSD | 2.9M | 42.5 s | 2.4 s | 24 ms | **1771×** |
| F: | NVMe | 2.2M | 4.3 s | 1.4 s | 19 ms | **229×** |
| G: | USB stick | 15K | 1.3 s | 572 ms | 6 ms | **219×** |
| M: | SATA HDD | 1.9M | 26.4 s | 1.4 s | 18 ms | **1469×** |
| S: | SATA HDD | 8.3M | 67 s | 4.8 s | 54 ms | **1236×** |
| **ALL (v0.5.4)** | **Mixed** | **25.9M** | **66 s** | **6.9 s** | **163 ms**² | **407×** |
| **ALL (v0.5.62)** | **Mixed** | **25.9M** | **68.5 s** | **5.7 s** | *see v0.5.66 re-bench below* | — |
| **ALL (v0.5.66)** | **Mixed** | **25.9M** | 68.5 s | **5.7 s** | **1 112 ms**² | — |

² The HOT `*` number is `uffs * --limit 100` CLI end-to-end.  The
v0.5.4 figure (163 ms) was never re-verified after the Phase 2 top-N
sort rewrite; the v0.5.66 measurement is 1 112 ms ([`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:657`](../../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt),
30 rounds, StdDev 21 ms).  Tracked as Phase 5 target #2 in the
cross-tool analysis doc (§7, bounded-heap top-N).

### Interactive Search Percentile Latency (HOT, 25.9M records)

**v0.5.4 historical (30 rounds):**

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

**v0.5.66 re-bench (30 rounds, `--limit 100`, source [`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:573-707`](../../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt)):**

| Pattern                             | CLI e2e p50 | CLI e2e p95 | daemon p50 |
|-------------------------------------|------------:|------------:|-----------:|
| `*` (full scan, top-100)            | **1 112 ms**|  1 163 ms   | 1 081 ms   |
| `notepad.exe` (exact)               |      29 ms  |     34 ms   |      0 ms  |
| `win*` (prefix)                     |      31 ms  |     34 ms   |      1 ms  |
| `*.dbt` (ext_rare)                  |      32 ms  |     36 ms   |      0 ms  |
| `*.dll` (extension, 167 K match)    |      69 ms  |     75 ms   |     42 ms  |
| `config` (substring)                |      31 ms  |     34 ms   |      1 ms  |
| `>.*\.(jpg\|png\|heic)$` (regex)    |     135 ms  |    148 ms   |    108 ms  |
| `*system32*` (in-path heavy)        |      30 ms  |     33 ms   |      0 ms  |

**Daemon-side latency is unchanged vs v0.5.4** (0–3 ms for targeted
queries, same as before).  What shifted is the CLI’s per-invocation
floor: the post-Phase-1 thin-client spawn is ~28 ms on Windows, so
any `Measure-Command { & uffs.exe ... }` measurement now includes
that 28 ms tax even when the daemon answers in 0–1 ms.  The `*`
fullscan regression is independent and tracked as Phase 5 target #2
(bounded-heap top-N).

**v0.5.120 cross-tool capture (HOT, file sink, p50, n=10 — source
[`docs/benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv`](../../benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv)):**
targeted patterns run **17–96 ms CLI end-to-end per drive** (exact
17–21 ms, ext_rare 18–20 ms, substring 17–39 ms, prefix 18–80 ms,
ext_dll 18–96 ms; 4-drive combined up to 181 ms on the 364 K-row
`*.dll` set).  The G-drive empty-result cells expose the pure
spawn+pipe floor at **17–18 ms** — down from the ~28–32 ms measured
on v0.5.66.  Daemon-side latency was not re-instrumented in this
capture; the 0–3 ms v0.5.66 figures above remain the latest
daemon-internal measurements.

### Bulk Retrieval Throughput — v0.5.4 tiers (historical)

Current bulk-export performance is the v0.5.120 full-scan table above
(**≈ 1.95 M rows/s sustained across all 7 drives**, peaks of
2.29 M rows/s per drive).  The v0.5.4 tier sweep below predates the
export-pipeline rewrite and is kept for the tier-scaling shape only;
its absolute numbers are obsolete.

| Tier | Rows | Avg Time | Avg Rows/sec |
|------|-----:|---------:|-------------:|
| 100 | 101 | 213 ms | 474/s |
| 1k | 1,001 | 202 ms | 5.0k/s |
| 10k | 10,001 | 323 ms | 31.0k/s |
| 100k | 100,001 | 1.4 s | 72.5k/s |
| 1M | 1,000,001 | 3.4 s | 292k/s |
| ALL (per-drive) | 8.3M | 25.6 s | **323k/s** |

> **Pipe vs `--out-dir` (v0.5.4):** Shell pipe throughput peaked at ~122k rows/s.
> Using `--out-dir` (direct file write) reached **323k rows/s** — a **2.6× speedup** on full exports.
> The direct-file-write advantage still holds on the current pipeline; the absolute rates are the v0.5.120 table above.

### Scale Ceiling (interactive search, `--limit 100`, 30 rounds)

**v0.5.4 historical (with offline MFT clones up to 100.4 M records):**

| Total Records | Drives | `*` p50 (e2e) | targeted p50 | Status |
|--------------:|-------:|--------------:|-------------:|--------|
| 25.9M | 7 | 161 ms | 9–10 ms | ✅ PASS |
| 42.5M | 9 | 259 ms | 9–10 ms | ✅ PASS |
| 59.0M | 11 | 471 ms | 10–12 ms | ✅ PASS |
| 75.6M | 13 | 600 ms | 10–12 ms | ✅ PASS |
| 92.2M | 15 | 670 ms | 11–14 ms | ✅ PASS |
| **100.4M** | **16** | **808 ms** | **11–13 ms** | **✅ PASS** |
| >100M | 17+ | — | — | ❌ OOM |

**v0.5.66 drive-accumulation sweep (real drives only, no synthetic
clones — [`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:933-1044`](../../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt), n=1 per drive set):**

| Total Records | Drives      | Daemon RSS | `*` e2e    |
|--------------:|-------------|-----------:|-----------:|
|  3.67 M       | C           |   777 MB   |   1 416 ms |
| 10.74 M       | C,D         | 2 112 MB   |   3 407 ms |
| 13.67 M       | C,D,E       | 2 587 MB   |   5 304 ms |
| 15.89 M       | C,D,E,F     | 3 059 MB   |   6 264 ms |
| 15.91 M       | C,D,E,F,G   | 3 063 MB   |   6 178 ms |
| 17.81 M       | C,D,E,F,G,M | 3 351 MB   |   7 069 ms |
| **26.09 M**   | **All 7**   | **4 722 MB**| **15 176 ms** |

**Memory scales linearly at ~180 MB per million records** (ranges
192–212 MB/M-rec across the sweep).  The drive-scale sweep is a
stand-in for a proper synthetic-clone re-bench — the 42.5 M /
75.6 M / 100.4 M v0.5.4 rows still need MFT clone tooling that does
not yet exist in `scripts/dev/` to re-verify on v0.5.66.

> Targeted queries stay at **0–3 ms daemon-side** regardless of
> corpus size (confirmed on v0.5.66, see Interactive Search table
> above).  Only unfiltered `*` scans scale linearly with total
> records; that path is also the one tracked as Phase 5 target #2
> (bounded-heap top-N).

### What the benchmark shows

- **Scale is the headline** — UFFS keeps **100M+ records across 16 drives** searchable from one daemon.
- **Cold-start time is storage-bound** — NVMe is parse-bound, while HDD cold runs are dominated by seek time and raw MFT I/O.
- **Warm restart is the operator win** — the full 25.9M-record searchable state returns in **5.7 s** from serialized cache (v0.5.62; 6.9 s on v0.5.4).
- **Hot queries are media-independent** — once the daemon is warm, single-drive end-to-end queries complete in **6–54 ms** depending on drive size (v0.5.4 per-drive table).  Targeted queries on v0.5.4 returned in **9–13 ms** end-to-end; on v0.5.66 they are **29–32 ms** CLI end-to-end with **0–3 ms daemon-side** — the extra ~20 ms comes from the Phase 1+ thin-client spawn floor on Windows (v0.5.4 predates it).
- **`*` full-scan top-N has regressed from v0.5.4** — the 163 ms all-drive hot number was never re-verified after the Phase 2 sort rewrite; v0.5.66 measures **1 112 ms** CLI-e2e / 1 081 ms daemon-side on the same hardware ([`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:657`](../../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt)).  Tracked as Phase 5 target #2 (bounded-heap top-N).  (Distinct from full-scan *export*, which has improved — next bullet.)
- **Bulk export sustains ≈ 1.95 M rows/sec** (v0.5.120) — the complete 23.3 M-row estate streams to CSV in **12.0 s** end-to-end through the daemon pipe; the largest single drive (S:, 8.3 M rows) exports in 3.83 s at 2.16 M rows/s.  This is ~6× the 323k rows/s the v0.5.4 harness measured.

> 📖 **Full benchmark data:** [Performance](../../user-manual/performance.md)

---

## Optimization Layers

### Layer 1: I/O Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **Direct MFT reading** | ~15× vs FindFirstFile | Bypass file system APIs entirely |
| **Bitmap skip** | 50-80% I/O reduction | Skip deleted records using $MFT::$BITMAP |
| **IOCP async I/O** | Overlaps I/O with CPU | Multiple reads in flight simultaneously |
| **LCN-ordered reads** | 20-30% HDD improvement | Minimize disk seeks by reading in physical order |
| **Drive-type tuning** | 10-50% per drive | NVMe: 32 concurrent reads, 4MB chunks; HDD: 2-6, 1MB |
| **Aligned buffers** | Required for NO_BUFFERING | Sector-aligned allocation avoids extra copies |

### Layer 2: Memory Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **Compact FileRecord** | 224 bytes/record | Bit-packed flags, inline first_name/first_stream |
| **Contiguous names buffer** | Cache-friendly | All names in one `String`, no per-name allocation |
| **Pre-allocated vectors** | Eliminates resizing | Sized from bitmap popcount before parsing starts |
| **Extension interning** | 8 bytes per name ref | 16-bit extension ID instead of string per record |
| **mimalloc** | ~10-15% throughput | Reduces fragmentation for millions of small allocs |
| **NO_ENTRY sentinel** | No Option overhead | `u32::MAX` instead of `Option<u32>` saves 4 bytes |

### Layer 3: Algorithm Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **Extension index** | 50× for *.ext queries | O(matches) instead of O(all) for extension patterns |
| **Inline parsing** | No intermediate copies | Records parsed directly into MftIndex during I/O |
| **Zero-alloc case compare** | Eliminates 8M allocs | Byte-level ASCII comparison instead of `.to_lowercase()` |
| **Leaf-peeling tree metrics** | O(n) no recursion | Array-based Kahn sort instead of recursive DFS |
| **Lazy path resolution** | Only for matched records | Paths computed after all filters applied |
| **Pattern classification** | Optimal matcher per type | `*.txt` → suffix check; `*foo*` → substring; etc. |

### Layer 4: Concurrency Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **IOCP sliding window** | Saturates I/O device | N reads always in flight (N tuned per drive type) |
| **Lock-free hot path** | Zero contention | Single-owner MftIndex during build, no mutexes |
| **Multi-drive parallelism** | Near-linear scaling | Bounded tokio tasks, independent IOCP per drive |
| **Rayon parallel parsing** | NVMe: overlaps CPU/IO | Parse completed buffers on worker threads |
| **Buffer recycling** | Zero allocs after warmup | Completed buffers returned to pool, not freed |

---

## Where Time Is Spent

### NVMe Drive (C:, 3.5M files, 7.5 s cold)

```
Phase                Time     %    Notes
───────────────────────────────────────────────
Volume open          <1ms   <1%   CreateFile + FSCTL
Metadata collection    5ms   <1%   Volume data + retrieval pointers
Bitmap read           10ms   <1%   ~250KB bitmap
Chunk planning         1ms   <1%   In-memory calculation
IOCP read + parse   5.9s    79%   ★ DOMINANT — parsing is bottleneck
Tree metrics        0.8s    10%   Leaf-peeling O(n)
Extension index     0.3s     4%   Build interned lookup
Stats + finalize    0.5s     6%   Recompute, cleanup
```

On NVMe, **parsing is the bottleneck** (not I/O). The disk can deliver data faster than the CPU can parse it. This is why parallel parsing helps on NVMe.

### SATA HDD Drive (S:, 8.3M files, 67 s cold)

```
Phase                Time     %    Notes
───────────────────────────────────────────────
Volume open          <1ms   <1%
Metadata collection    8ms   <1%
Bitmap read           20ms   <1%
Chunk planning         2ms   <1%
IOCP read + parse  65.0s    97%   ★ DOMINANT — I/O is bottleneck
Tree metrics        1.0s     1%
Extension index     0.5s     1%
Stats + finalize    0.5s     1%
```

On HDD, **I/O is the bottleneck**. The disk head seek time dominates. Bitmap skip and LCN-ordered reads are critical here.

---

## Profiling

### Built-in Profiling (`--profile`)

```bash
uffs * --drive C --profile
```

Outputs detailed timing for each phase:
```
=== PROFILE: Client → Daemon ===
  Connect:              3 ms
  Await ready:          0 ms
  Search (IPC):       152 ms  (daemon: 151 ms, transfer: 1 ms)
  Convert rows:         0 ms  (10 rows)

=== PROFILE: Daemon Internals ===
  Startup:           3861 ms
  Search:             151 ms  (25,929,744 records scanned)
  Row build:            0 ms  (10 → SearchRow)

=== TOTAL: 182 ms ===
```

### Benchmark Mode (`--benchmark`)

Skips output formatting to isolate MFT reading performance:

```bash
uffs * --drive C --benchmark
# Only measures: volume open → index built
# No path resolution, no formatting, no stdout I/O
```

### Flamegraph Profiling

```bash
# Build with profiling profile (debug symbols, no LTO)
cargo build --profile profiling

# Use cargo-flamegraph or perf
cargo flamegraph --profile profiling -- uffs * --drive C --benchmark
```

The `profiling` Cargo profile enables debug symbols while keeping optimizations:

```toml
[profile.profiling]
inherits = "release"
debug = true
strip = false
lto = false
codegen-units = 16
```

### Tracing

UFFS uses `tracing` for structured logging. Enable with environment variables:

```bash
RUST_LOG=debug uffs * --drive C    # Info + debug messages
RUST_LOG=trace uffs * --drive C    # Maximum verbosity (very noisy)
```

Key trace points:
- `[TRIP]` markers at function entry/exit for trip-wire debugging
- I/O chunk progress (bytes read, records parsed)
- Drive detection and tuning decisions
- Extension record processing

---

## Memory Usage

### Typical Memory Footprint (C: drive — 3.5M MFT records, ~2M active files)

| Component | Size | Notes |
|-----------|------|-------|
| `records: Vec<FileRecord>` | 448 MB | 2M active × 224 bytes |
| `frs_to_idx: Vec<u32>` | 14 MB | 3.5M × 4 bytes (sparse, covers full MFT) |
| `names: String` | 46 MB | 2M × 23 bytes avg |
| `links: Vec<LinkInfo>` | 3 MB | ~125K hardlinks |
| `streams: Vec<IndexStreamInfo>` | 15 MB | ~500K ADS |
| `children: Vec<ChildInfo>` | 42 MB | 3M entries |
| I/O buffers (peak) | 32 MB | 32 × 1MB (NVMe) |
| **Total** | **~600 MB** | |

### Memory Reduction Techniques

1. **Compact types**: 224 bytes per record vs ~400+ bytes with naive layout
2. **Inline first_name/first_stream**: Avoids heap allocation for 95%+ of records
3. **Shared names buffer**: One allocation instead of 2M `String` objects
4. **Extension interning**: 2 bytes per name ref instead of string per extension
5. **NO_ENTRY sentinel**: `u32::MAX` instead of `Option<u32>` (saves 4 bytes × millions)

---

## Why UFFS Is Fast on NVMe

The dominant performance advantages come from architectural decisions in the Rust engine:

1. **Inline parsing**: Records parsed directly into `MftIndex` during IOCP completion — no intermediate copies or staging buffers.
2. **mimalloc**: Purpose-built allocator reduces fragmentation for millions of small objects.
3. **Compact `FileRecord`**: 224 bytes per record with bit-packed flags and inline first-name/first-stream.
4. **Zero-copy NTFS parsing**: `zerocopy` crate reads NTFS headers directly from I/O buffers without memcpy.
5. **Contiguous names buffer**: Single `String` allocation instead of per-name heap objects.

## Known Optimization Targets

### Multi-Drive Filtered Scan (`*.ext`)

Filtered multi-drive parallel scans are dramatically faster than full
scans.  On v0.5.4 targeted patterns (`*.dll`, `config`, `notepad.exe`)
returned in **9–13 ms e2e** vs **161 ms** for `*` on all 7 drives; on
v0.5.66 the same targeted patterns measure **29–69 ms e2e** (28 ms
CLI spawn tax + 0–42 ms daemon) while `*` is now **1 112 ms** (see
§Interactive Search above for the full v0.5.66 table); on v0.5.120
targeted patterns run **17–96 ms e2e per drive** with the spawn floor
down to 17–18 ms (the `*` top-N shape was not re-measured — the
v0.5.120 suite times full-scan *export* instead).  At 100 M
records (v0.5.4 only, not re-verified since) `*` took 808 ms
but targeted queries stayed at **11–13 ms** daemon-side.

---

## Cargo Build Profiles

| Profile | Use Case | LTO | Debug | Opt |
|---------|----------|-----|-------|-----|
| `dev` | Development | No | Full | 0 |
| `debug-optimized` | Dev with speed | No | Full | 2 |
| `release` | Production | Fat | No | 3 |
| `profiling` | Flamegraphs | No | Full | 3 |
| `bench` | Benchmarks | Thin | No | 3 |
| `dist` | Distribution | Thin | No | 3 |
| `xwin-dev` | Cross-compile dev | No | Full | 0 |

---

*Document Version: 3.1*
*Last Updated: 2026-06-11*
*UFFS Version: 0.5.120*
