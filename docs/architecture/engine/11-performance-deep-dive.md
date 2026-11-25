# Performance Deep Dive

## Introduction

This document explains why UFFS is a high-performance MFT search engine, the engineering decisions behind it, and real-world benchmark data from a 7-drive, 25.9-million-record production system tested up to **100M records**.

> **See also:**
> - [`docs/benchmarks/`](../../benchmarks/) — publication-grade competitive-benchmark hub (UFFS vs Everything vs UFFS C++); current canonical report: [`2026-06-v0.5.120-vs-everything.md`](../../benchmarks/2026-06-v0.5.120-vs-everything.md).
> - [User-manual performance page](../../user-manual/performance.md) — full benchmark reference with per-drive tables and validation throughput.
> - `docs/research/cross-tool-benchmark-analysis.md` — engineering-detail source (internal).

---

## Architecture: Three Caching Levels

UFFS operates in three performance tiers, each with dramatically different latency:

| Level | What Happens | Typical Latency (25.9M records) |
|-------|-------------|-------------------------------|
| **COLD** | No daemon, no cache. Raw MFT read from disk, full parse, compact index build, trigram index build, path resolution tree. | 66 s (v0.5.4) → 68.5 s (v0.5.62) — 7 drives parallel |
| **WARM CACHE** | No daemon, but serialized compact index exists on disk. Daemon starts and deserializes cached index — no MFT read. | 6.9 s (v0.5.4) → **5.7 s (v0.5.62, −17 %)** |
| **HOT** | Daemon running with in-memory index. Pure search — no I/O, no startup. | v0.5.4: **163 ms** e2e (`*` top-N). v0.5.66 measured (see re-bench § below): `*` with `--limit 100` is **1 112 ms** CLI-e2e ([`raw log line 657`](../../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt)) — the “163 ms” figure was never re-verified after the Phase 2 top-N rewrite and is now superseded.  Targeted queries measure **29–32 ms** CLI-e2e with **0–3 ms daemon-side** on v0.5.66; the v0.5.120 cross-tool capture puts them at **17–96 ms per drive** with the spawn floor down to **17–18 ms**. |

The HOT path delivers **407× speedup** over COLD.  Targeted queries
(exact name, prefix, extension, substring, combined) return in
**0–3 ms daemon-side** — even at **100 M records**.  **CLI end-to-end
on v0.5.66 is 29–32 ms** (28 ms cold-spawn tax + 0–3 ms daemon); on
**v0.5.120 the spawn+pipe floor has dropped to 17–18 ms** (G-drive
empty-result cells in
[`raw/2026-06-v0.5.120_cross-tool-summary.csv`](../../benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv)),
with targeted patterns at 17–96 ms per drive.
The “9–13 ms e2e” number shipped in v0.5.4 docs was measured before
the Phase 1 thin-client landed; since then the per-invocation
process-start cost on Windows dominates any targeted query.

---

## Real-World Benchmarks (latest: v0.5.120)

> **Latest numbers (v0.5.120, 2026-06-11):** **30/30 head-to-head cells
> faster than Everything** at p50 (median ratio 0.36×); 7-drive full-scan
> export of **23.3 M rows → CSV in 11.98 s (≈ 1.95 M rec/s)**; targeted
> CLI e2e 17–96 ms per drive.  Source:
> [current canonical report](../../benchmarks/2026-06-v0.5.120-vs-everything.md).
> Aggregations at ~180 ms, 5.7 s WARM restart, and 4.99 GB settled RSS
> remain the latest measurements from v0.5.62 (not re-run since).
> Engineering-detail table in
> `@/Users/rnio/Private/Github/UltraFastFileSearch/docs/research/cross-tool-benchmark-analysis.md` §Current State.
>
> The v0.5.4 per-drive tables below are **retained for historical
> context** — they were captured before Phase 1, Phase 2, and Phase 3
> shipped.  The HOT interactive-search shape (`*` with `--limit 100`,
> 8 patterns) was re-run on v0.5.66 (table below); the v0.5.120 suite
> measures the cross-tool pattern set with a file sink instead, so the
> top-N shape has no newer capture.

### Test Environment

**System**: AMD Ryzen 9 3900XT — 12 cores / 24 threads, 64 GB DDR4
**Drives**: 7 NTFS volumes (2× NVMe Samsung 990 PRO, 2× SATA Samsung 980 PRO, 2× SATA WD 8 TB HDD, 1× USB stick)
**Total records**: 25,931,436 across all drives (live), scaled up to 100.4M with offline MFT clones
**Binary**: v0.5.4 release build (LTO=fat, codegen-units=1, cross-compiled from macOS via `cargo xwin`)
**Latest bench binary**: v0.5.120 (cross-tool suite via `just bench-suite`; v0.5.62 was the last aggregate-baseline capture)
**Protocol**: Per-drive profile (COLD → WARM → HOT) + interactive search (30 rounds, 8 patterns) + bulk retrieval

### Per-Drive 3-Phase Results (`*` pattern)

| Drive | Type | Records | COLD | WARM | HOT | Cold→Hot |
|-------|------|--------:|-----:|-----:|----:|---------:|
| C: | NVMe | 3,512,541 | 7.7 s | 6.4 s | **27 ms** | **284×** |
| D: | SATA SSD | 7,066,020 | 28.6 s | 6.4 s | **49 ms** | **584×** |
| E: | SATA SSD | 2,929,523 | 42.5 s | 2.4 s | **24 ms** | **1771×** |
| F: | NVMe | 2,221,347 | 4.3 s | 1.4 s | **19 ms** | **229×** |
| G: | USB stick | 15,094 | 1.3 s | 572 ms | **6 ms** | **219×** |
| M: | SATA HDD | 1,908,809 | 26.4 s | 1.4 s | **18 ms** | **1469×** |
| S: | SATA HDD | 8,278,106 | 67 s | 4.8 s | **54 ms** | **1236×** |
| **ALL** | **Mixed** | **25,931,436** | **66 s** | **6.9 s** | **163 ms** | **407×** |

### HOT Interactive Search Percentile Latency (ALL drives, 25.9 M records)

**v0.5.4 historical (30 rounds, same hardware):**

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

**v0.5.66 re-bench ([`docs/benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt:573-707`](../../benchmarks/raw/2026-04-v0.5.66_full-benchmark-suite.txt), 30 rounds, same hardware, `--limit 100`):**

| Pattern                          | CLI e2e p50 | CLI e2e p95 | daemon-side |
|----------------------------------|------------:|------------:|------------:|
| `*` (full scan, top-100)         | **1 112 ms**|   1 163 ms  |    1 081 ms |
| `notepad.exe` (exact)            |     29.4 ms |     34.0 ms |         0 ms |
| `win*` (prefix)                  |     30.7 ms |     33.5 ms |         1 ms |
| `*.dbt` (ext_rare)               |     31.8 ms |     36.0 ms |         0 ms |
| `*.dll` (extension, 167 K match) |     68.6 ms |     74.8 ms |        42 ms |
| `config` (substring)             |     30.6 ms |     33.5 ms |         1 ms |
| `>.*\.(jpg\|png\|heic)$` (regex) |    135.3 ms |    148.4 ms |       108 ms |
| `*system32*` (in-path heavy)     |     30.4 ms |     33.2 ms |         0 ms |

**Daemon-side has barely moved (0–3 ms for targeted queries — same
as v0.5.4).**  What changed is the CLI cold-spawn floor: Phase 1's
thin-client shaved it from ~50 ms to ~28 ms, but per-process startup
now dominates.  The `*` fullscan at 1.1 s is a separate regression
from the v0.5.4 163 ms number and is tracked as Phase 5 target #2 in
`@/Users/rnio/Private/Github/UltraFastFileSearch/docs/research/cross-tool-benchmark-analysis.md` §7 (bounded-heap top-N).

Full scans sustain **~1.95 M records/second** end-to-end on v0.5.120
(23.3 M rows → CSV in 12.0 s across all 7 volumes; the 4-drive subset
runs 2.11 M rec/s — see the
[current canonical report](../../benchmarks/2026-06-v0.5.120-vs-everything.md);
v0.5.66 measured 1.72 M rec/s). Not 167 M/sec — that was an in-memory
scan rate without output materialisation; the end-to-end figure is the
disk-write-inclusive number.

### Bulk Retrieval (CSV, `--out-dir`, per-drive — v0.5.4 historical)

Current export performance is the **≈ 1.95 M rows/s** v0.5.120 figure
above (the same 8.3 M-row S: drive now exports in 3.83 s at
2.16 M rows/s vs 25.6 s here).  The tier sweep below predates the
export-pipeline rewrite and is kept for the tier-scaling shape only.

| Tier | Rows | Time | Rows/sec |
|------|-----:|-----:|---------:|
| 100 | 101 | 213 ms | 474/s |
| 1k | 1,001 | 202 ms | 5.0k/s |
| 10k | 10,001 | 323 ms | 31k/s |
| 100k | 100,001 | 1.4 s | 73k/s |
| 1M | 1,000,001 | 3.4 s | 292k/s |
| ALL (8.3M) | 8,278,106 | 25.6 s | **326k/s** |

Pipe-based output peaked at ~122k rows/s on v0.5.4.  Direct file write
(`--out-dir`) reached **323k rows/s** — a **2.6× speedup**.  The
direct-write advantage still holds on the current pipeline; the
absolute rates are the v0.5.120 numbers above.

### Scale Ceiling (100M records, 16 drives, 30 rounds)

| Total Records | `*` e2e p50 | targeted p50 | Status |
|--------------:|------------:|-------------:|--------|
| 25.9M | 161 ms | 9–10 ms | ✅ |
| 42.5M | 259 ms | 9–10 ms | ✅ |
| 59.0M | 471 ms | 10–12 ms | ✅ |
| 75.6M | 600 ms | 10–12 ms | ✅ |
| 92.2M | 670 ms | 11–14 ms | ✅ |
| **100.4M** | **808 ms** | **11–13 ms** | **✅** |
| >100M | — | — | ❌ OOM |

Targeted queries stay at **0–3 ms daemon-side** regardless of corpus size.

---

## Why UFFS Is Fast

### 1. Direct MFT Reading (15× vs Standard APIs)

Windows file enumeration (`FindFirstFile`/`FindNextFile`) requires ~2 syscalls per file. UFFS reads the MFT as a raw byte stream via a single volume handle — one `ReadFile` call processes ~1,000 files (1 MB ÷ 1 KB records), reducing syscall overhead by ~2,000× on a 2M-file drive.

### 2. Bitmap Skip (40–55% I/O Reduction)

The MFT contains records for deleted files. Typical utilization is 40–60%. UFFS reads `$MFT::$BITMAP` first (~250 KB), then trims read ranges to skip contiguous unused regions. On the S: drive (11.5 GB MFT, 45% utilization), this saves ~6.3 GB of disk reads.

### 3. IOCP Sliding Window (I/O + CPU Overlap)

I/O Completion Ports with a sliding window of concurrent reads. While buffer N is parsed, buffers N+1..N+K are already in flight. Window size is auto-tuned per drive type: NVMe=32 (deep queue), SSD=8 (NCQ), HDD=2–6 (minimize seeks).

### 4. Inline Parsing (Zero Intermediate Copies)

`SlidingIocpInline` parses each completed I/O buffer directly into the `MftIndex` as the IOCP completion arrives. No intermediate `Vec<ParsedRecord>`, no second pass, no double-buffering of record data.

### 5. Compact Memory Layout (224 Bytes/Record)

Hand-tuned `FileRecord` with bit-packed flags (17 booleans in one `u32`), inline first name/stream (no heap allocation for 95%+ of files), sentinel values instead of `Option<>`, contiguous names buffer with `(offset, length)` references, and 16-bit interned extension IDs.

### 6. mimalloc Global Allocator

Purpose-built for the millions-of-small-allocations workload. ~10–15% throughput improvement on NVMe where parsing is the bottleneck.

### 7. Extension Index (50–200× for `*.ext` Queries)

Interned 16-bit extension IDs during parsing, with an inverted index `ext_id → Vec<record_index>`. A `*.rs` query on 5K results takes 0.5ms instead of 100ms full-scan.

### 8. Zero-Allocation Case-Insensitive Matching

Byte-level inline comparison without allocating a lowercase copy — eliminates 2–8M heap allocations per search query across 26M records.

### 9. Leaf-Peeling Tree Metrics (O(n), No Recursion)

Array-based Kahn-style topological sort for treesize/descendants. O(n) time, O(n) space, no recursion, cache-friendly sequential access. Guaranteed stack safety on any tree depth.

### 10. LCN-Ordered Reads (HDD Only)

Read chunks sorted by physical disk offset (LCN order) to minimize head movement on fragmented HDDs. 20–30% improvement on HDDs; no effect on NVMe/SSD.

### 11. Daemon Architecture with Compact Cache

The daemon holds the full index in memory. First search auto-starts the daemon, which persists a serialized compact cache to disk. Subsequent daemon starts deserialize the cache (~5.7 s for 25.9 M records on v0.5.66, was 6.9 s on v0.5.4) instead of re-reading the MFT (~68.5 s on v0.5.66).  Once hot, **targeted searches return in 0–3 ms daemon-side / 17–96 ms CLI end-to-end** (v0.5.120 cross-tool capture, spawn floor 17–18 ms; 29–32 ms on v0.5.66; 9–13 ms e2e on v0.5.4 before the Phase 1+ thin-client); unfiltered `*` with `--limit 100` is **1 112 ms** CLI e2e on v0.5.66 (was 163 ms on v0.5.4 — tracked as Phase 5 target #2 bounded-heap top-N fix).

### 12. Trigram Index for Substring Queries

Three-character trigram index built during startup. Substring queries intersect trigram posting lists before scanning records, dramatically reducing the search space for patterns like `*config*`.

---

## C++ Reference Baseline (v0.4.106 historical — engineering validation, not public market benchmark)

This section captures a one-shot parity measurement from **v0.4.106**
between the legacy C++ implementation and the then-current Rust
engine.  It is **not** re-run per release: it was a correctness-and-
cold-path validation artefact, not a headline metric.  Per-drive
COLD on v0.5.66 has not been re-captured — the only current COLD
number is the **68.5 s aggregate for all 7 drives in parallel**
(`@/Users/rnio/Private/Github/UltraFastFileSearch/docs/architecture/engine/09-performance.md#per-drive-3-phase-results`,
flat ± 4 % vs the v0.5.4 66 s baseline).

UFFS keeps the earlier C++ implementation as a parity and regression baseline. This comparison is useful for validating parser correctness and understanding cold-path trade-offs, but it is not the headline market benchmark for the Rust engine.

The Rust engine intentionally does more work during COLD startup: compact index build, cache serialization, extension interning, tree metrics, and daemon-ready data structures. The relevant buyer-facing payoff is not the raw COLD number alone, but the combination of:

- full cold build from raw MFT
- warm restart from serialized cache
- hot in-memory queries once the daemon is ready

Public external comparisons should therefore use the current Rust engine and separate readiness, interactive top-N, bulk retrieval, and scale-ceiling workloads.

When comparing COLD timings, the comparison is **not apples-to-apples**:

| | UFFS (Rust) | C++ Reference |
|-|-------------|---------------|
| MFT read | ✅ | ✅ |
| Full path resolution (parent chain walk) | ✅ | ✅ |
| Compact index build (224 B/record) | ✅ | ❌ |
| Trigram index build | ✅ | ❌ |
| Compact cache serialization to disk | ✅ | ❌ |
| Daemon startup + IPC | ✅ | ❌ (direct) |
| Tree metrics (descendants, treesize) | ✅ | ❌ |
| Extension interning + inverted index | ✅ | ❌ |

UFFS does **significantly more work** during COLD startup because it builds persistent data
structures that make every subsequent search instant. The C++ tool re-reads the MFT on every
invocation. On v0.4.106 the extra Rust work made it 1.29× slower on cold total; on v0.5.66
the same methodology shows Rust now **2.6× faster** than C++ warm-disk total even while
doing that extra work (see parity table below).

### Parity Comparison (v0.5.66 re-run, COLD per-drive, 6 drives, sequential)

> **Re-measured 2026-04-21** against v0.5.66 using
> [`scripts/windows/cold-parity-per-drive.ps1`](../../../scripts/windows/cold-parity-per-drive.ps1)
> (earlier invocation with the per-drive-cold methodology preserved from v0.4.106).
> Sequence per drive: purge that drive's cache → stop daemon → run Rust COLD
> (daemon spawns, reads MFT, builds compact + trigram indexes, writes cache)
> → run C++ on the same drive with OS page cache now warmed by Rust's read.
> Raw log: [`docs/benchmarks/raw/2026-04-v0.5.66_cold-parity-per-drive.txt`](../../benchmarks/raw/2026-04-v0.5.66_cold-parity-per-drive.txt).
>
> **Drive G (15 k-record USB drive) is excluded** from the per-drive tables as it
> is too small to be representative — its timing is dominated by the ~0.7 s daemon
> cold-spawn floor and Windows USB device-open latency, not by MFT-read or index-build
> work. Included in the daemon warm-up record count below (which loads all attached
> drives) but not in the comparison numbers.
>
> The current v0.5.66 aggregate COLD (all 6 internal drives in parallel, not sequential
> per-drive) completes in **~68.5 s total** — see the 3-phase table in
> [`docs/architecture/engine/09-performance.md`](../../architecture/engine/09-performance.md).
> The per-drive breakdown below is kept for direct apples-to-apples continuity
> with the historical v0.4.106 snapshot.

| Drive | Records | C++ (warm disk) | Rust v0.5.66 (cold) | Ratio | Files/sec (Rust) |
|-------|--------:|----------------:|--------------------:|------:|-----------------:|
| C: | 3,672,016 | 49.26 s | 7.66 s | **0.16×** | 479,297/s |
| D: | 7,066,015 | 112.69 s | 27.56 s | **0.24×** | 256,394/s |
| E: | 2,929,524 | 74.02 s | 41.54 s | **0.56×** | 70,528/s |
| F: | 2,221,349 | 28.63 s | 5.56 s | **0.19×** | 399,185/s |
| M: | 1,908,810 | 44.28 s | 27.54 s | **0.62×** | 69,303/s |
| S: | 8,278,106 | 148.24 s | 67.57 s | **0.46×** | 122,509/s |
| **TOTAL (sequential)** | **26,075,820** | **457.15 s** | **177.39 s** | **0.39×** | **147,000/s** |

**v0.5.66 is 2.6× FASTER than the C++ reference on cold total wall-clock** despite doing
substantially more work per drive (compact-index build, trigram index, cache serialization,
daemon startup). This reverses the v0.4.106 historical snapshot where Rust was 1.29× slower
than C++ on the same methodology — the persistent data structures Rust builds during COLD
are now amortised fast enough that Rust wins outright even before counting any downstream
HOT queries.

#### Rust cold-path sub-phase breakdown (from `--profile` stderr)

| Drive | Records | Wall | AwaitReady | IPC | Daemon | CLI tax |
|-------|--------:|-----:|-----------:|----:|-------:|--------:|
| C: | 3,672,016 | 7.66 s | 7,103 ms | 26 ms | 24 ms | 27 ms |
| D: | 7,066,015 | 27.56 s | 27,105 ms | 45 ms | 44 ms | 29 ms |
| E: | 2,929,524 | 41.54 s | 41,109 ms | 18 ms | 16 ms | 30 ms |
| F: | 2,221,349 | 5.56 s | 5,102 ms | 43 ms | 42 ms | 29 ms |
| M: | 1,908,810 | 27.54 s | 27,107 ms | 27 ms | 26 ms | 24 ms |
| S: | 8,278,106 | 67.57 s | 67,115 ms | 49 ms | 47 ms | 25 ms |

Legend:

- **Wall** — `Stopwatch`-measured PowerShell-to-CLI-to-daemon round-trip (matches the v0.4.106 methodology).
- **AwaitReady** — daemon spawn + MFT read + compact index build + trigram index build + cache write. This is the true cold-path cost.
- **IPC** — client round-trip for the `*` `--limit 100` search.
- **Daemon** — daemon-side search duration (microseconds on cold, since the drive is already loaded before the search starts).
- **CLI tax** — `Wall - AwaitReady - IPC - Connect` (process spawn + output formatting).

On multi-million-record drives, **AwaitReady dominates 97-99% of wall-clock** — which is
exactly the point of the compact + trigram indexes: pay once up front, then serve every
subsequent query in ~1 ms daemon-side. The constant-cost IPC/CLI overheads (~25-30 ms
each) are invisible at this scale.

### Daemon-HOT steady-state comparison (v0.5.66; re-measured on v0.5.120)

> **v0.5.120 re-measurement** (10 rounds, file sink — full table in the
> [current canonical report](../../benchmarks/2026-06-v0.5.120-vs-everything.md)
> §vs the UFFS C++ reference, raw data in
> [`raw/2026-06-v0.5.120_cross-tool-summary.csv`](../../benchmarks/raw/2026-06-v0.5.120_cross-tool-summary.csv)):
> UFFS wins **5/5 full-scan cells** (4.9×–13.9× per drive, 6.6× on
> C+D+F+G combined) and **180×–3 447× on targeted queries** (e.g. C:
> exact 20 ms vs 3 640 ms; D: regex 26 ms vs 89 625 ms); the
> combined-drive regex cell **DNF'd** (> 120 s timeout, 131 s measured).
> The per-drive v0.5.66 walkthrough below is kept for its sub-phase
> breakdowns.
>
> After the initial cold load above, the Rust daemon holds all drives in memory and
> serves every subsequent query from the compact index + trigram postings. The C++
> reference has **no daemon** — every invocation re-reads all MFTs regardless of the
> `--drives=X` filter (that flag is an output filter, not a load-time filter; see
> [`scripts/windows/cross-tool-benchmark.rs:553`](../../../scripts/windows/cross-tool-benchmark.rs)).
> This table measures how that architectural difference plays out for interactive
> `*` queries on one drive at a time, **with both tools writing output to a file**
> (a scripting-style workflow). Raw log in the same [`docs/benchmarks/raw/2026-04-v0.5.66_cold-parity-per-drive.txt`](../../benchmarks/raw/2026-04-v0.5.66_cold-parity-per-drive.txt) file.

**Methodology.** Daemon pre-loaded with all 7 attached drives (warm-up from existing
cache: 0.2 s, 26,090,928 records across every drive including USB G). Per drive, one
round each of:

- Rust: `uffs.exe '*' --drive <X> --out <tmp> --columns Path --hide-system --hide-ads --profile`
- C++:  `uffs.com '*' --drives=<X> --columns=path --out=<tmp>` (re-reads all MFTs every invocation)

Both write to a temp file; row counts are read post-run from the file. Drive G (USB, 15 k
records) is excluded from the comparison for the same reason as above.

| Drive | C++ (MFT re-read + filter) | Rust (daemon HOT) | Speedup | Rust rows | C++ rows |
|-------|---------------------------:|------------------:|--------:|----------:|---------:|
| C: | 8,621 ms | 1,531 ms | **5.6×** | 3,454 | 3,513 |
| D: | 31,668 ms | 1,955 ms | **16.2×** | 4,756 | 7,066 |
| E: | 42,421 ms | 1,242 ms | **34.2×** | 2,914 | 2,928 |
| F: | 4,495 ms | 890 ms | **5.1×** | 2,121 | 2,082 |
| M: | 21,955 ms | 927 ms | **23.7×** | 1,902 | 1,909 |
| S: | 51,852 ms | 3,547 ms | **14.6×** | 8,249 | 8,278 |
| **TOTAL (sum of per-drive p50s)** | **161,012 ms** | **10,092 ms** | **16.0×** | — | — |

**C++ is 16.0× slower in the honest workflow comparison.** Every user-issued query
forces a full-MFT re-read of every drive because there is no persistent daemon. Rust's
daemon pays the cold cost *once* (the 177.4 s total above) and then serves every
subsequent query in ~1-4 s per drive regardless of query frequency. For interactive
use and scripting — the two dominant real-world workloads — this is the number that
actually matters.

> **Row-count caveat.** Rust and C++ do not interpret `*` identically — most visibly on
> drive D (4,756 vs 7,066 rows). The Rust invocation adds `--hide-system --hide-ads` to
> match Everything's default result-set semantics; C++ has no equivalent flag. Both tools
> are faithfully doing their default "find everything on drive X" workflow and writing it
> out; the timing comparison holds, but the row-count columns should not be read as
> cross-tool result-set equivalence validation. On the drives where both tools are
> closer to the same semantics (C, E, F, M, S), the row counts agree to within 1-2%.

#### Rust daemon-side vs CLI overhead breakdown

| Drive | Rust rows | Rust wall p50 | Rust daemon p50 | CLI overhead |
|-------|----------:|--------------:|----------------:|-------------:|
| C: | 3,454 | 1,531 ms | 1,013 ms | 518 ms |
| D: | 4,756 | 1,955 ms | 1,349 ms | 606 ms |
| E: | 2,914 | 1,242 ms | 800 ms | 442 ms |
| F: | 2,121 | 890 ms | 561 ms | 329 ms |
| M: | 1,902 | 927 ms | 666 ms | 261 ms |
| S: | 8,249 | 3,547 ms | 2,333 ms | 1,214 ms |

The daemon serves most drives' `*` queries in well under a second (≤ 1.4 s for ≤ 5 M
records matched). **CLI overhead accounts for 20-40% of wall-clock per invocation**
(~0.3-1.2 s for process spawn + IPC round-trip + stderr profile print), which matters
for scripting against thousands of queries: use the library crate or connect directly
to the daemon RPC socket to skip the Windows process-creation tax. For a human typing
a single interactive query, wall-clock ≤ 4 s is the relevant number.

After COLD, UFFS never needs to re-read the MFT — the daemon serves all subsequent
queries from memory in **0-3 ms daemon-side for targeted queries** (17–96 ms CLI
end-to-end on v0.5.120 with a 17–18 ms spawn floor; 29-32 ms on v0.5.66; 9-13 ms on
v0.5.4 before the post-Phase-1 thin-client), and **1,112 ms CLI e2e** for unfiltered
`*` with `--limit 100` (v0.5.66, regressed from 163 ms v0.5.4; Phase 5 fix pending —
not re-measured on v0.5.120, whose suite times full-scan *export*, not top-N).

---

## Benchmark Methodology

### 3-Phase Protocol

Every benchmark runs three caching levels per drive:

1. **COLD** — Kill daemon, delete all cache files, run `uffs "*" --profile --drive X --limit 100`
2. **WARM CACHE** — Kill daemon (cache files remain), run same command
3. **HOT** — Daemon still running, run same command

This isolates: (1) raw MFT read + full index build, (2) cache deserialization, (3) pure in-memory search.

### Profiling

Use `--profile` for full per-phase timing breakdown (client connect, daemon startup, search, IPC, per-drive cache/MFT/compact/trigram timing). Use `rust-script scripts\windows\profile.rs` for automated 3-phase profiling across all drives.

---

*Last Updated: 2026-06-11*
*UFFS Version: 0.5.120*
