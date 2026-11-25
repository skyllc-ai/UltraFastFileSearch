# UFFS benchmark methodology

**Audience:** engineers, benchmark skeptics, competitor authors, and anyone evaluating a specific number in a UFFS benchmark report and asking *"but under what rules?"*.

**Purpose:** the fairness-doctrine rules that govern every benchmark UFFS publishes. Any single number in any canonical report is traceable to the discipline described here. If the discipline fails, the report fails — and we'd rather not publish the report at all than fudge the discipline.

**Primary reference:** [`2026-06-v0.5.120-vs-everything.md`](2026-06-v0.5.120-vs-everything.md) is the current canonical competitive report (prior snapshots live in [`archive/`](archive/)). This methodology page is the companion — the "here are the rules we played by" document that readers can cite without reading the full report.

---

## The four principles

Every benchmark UFFS publishes adheres to these four rules. If any of the four is violated, the report has a bug — please open an issue.

1. **Separate workloads get separate numbers.** Cold start, warm restart, hot interactive query, and bulk export are four structurally different jobs. We measure each separately and publish each separately. We never average them into one headline "speed" number.
2. **Publish the failures.** Every regression UFFS has caused to itself is named, measured, root-caused, and published alongside the wins. Brand trust > headline number.
3. **Disclose the environment.** One test machine, specific drive topology, specific Windows version — all documented. Per-user numbers will vary; we don't hide that.
4. **Show the work.** Every number cites a raw log file, every log file is committed to the repo, every benchmark script is committed to the repo. If you can't reproduce our numbers on your hardware, something is wrong (or our methodology has a bug we want to hear about).

The sections below describe how each principle is operationalized.

---

## Workload separation — why one number is always a lie

### Cold vs warm vs hot vs bulk — four jobs, four numbers

| Workload | What happens | User experience | Tests |
|----------|--------------|-----------------|-------|
| **Cold** | No daemon, no cache. Raw MFT read, parse, compact index build, trigram + extension indexes built, cache written to disk. | First-time-user experience, or after a cache-dir wipe. | Daemon spawn + MFT read wall-clock time at 26 M records. |
| **Warm restart** | Daemon restart from existing cache file. No MFT read. | Every subsequent boot after the first. | Daemon restart wall-clock time. |
| **Hot interactive** | Daemon running, in-memory index. One targeted query (exact name, prefix, extension, substring, combined) returning tens to thousands of rows. | The "type-and-see" loop. | p50 / p95 latency over 30 rounds per pattern per drive. |
| **Hot bulk** | Daemon running, in-memory index. One full-scan `*` query exporting millions of rows to a CSV file. | Scripting, investigations, AI-agent loops. | p50 wall-clock over 10 rounds. |

**Why this matters for competitive benchmarks:**

- Everything keeps its own in-memory index continuously loaded. Its "cold" and "warm" are fuzzy concepts because it has no persisted serialized cache in the UFFS sense — the moment you install it, it indexes everything and keeps the index in RAM for as long as the process lives. Every Everything number we publish is fully hot.
- The UFFS C++ reference has no daemon, no persisted index. Every invocation re-reads every MFT. Every C++ number we publish is structurally "cold per invocation", even on warm disk where the OS page cache has the MFTs resident.
- UFFS Rust separates all four workloads explicitly. When we compare against Everything, we compare our *hot* numbers against Everything's equivalent hot path. When we compare against C++, we compare our *cold* numbers against C++'s equivalent re-read path. Never mismatched.

**Rule:** any benchmark that mixes a cold tool against a warm tool is rigged. We break out the cold/warm/hot rows explicitly and publish each one labeled.

### Interactive latency vs bulk throughput are not the same thing

Targeted queries (`notepad.exe`, `*.dll`, `win*`) and full-scan exports (`*` → CSV for 23 M rows) are different workload classes that want different measurement tools.

- Interactive latency wants **p50 and p95 over many rounds**, because human response-time perception is percentile-driven. A tool with median 30 ms and p95 800 ms feels worse than a tool with median 50 ms and p95 60 ms, even though the first has lower median.
- Bulk throughput wants **records-per-second sustained over the full run**. A tool that takes 2 s to start and then runs at 10 M rec/s is a bulk winner over a tool that starts instantly but sustains only 500 K rec/s.

Publishing one number for "speed" collapses both properties into the same metric and misleads every reader trying to match a tool to a workload. UFFS therefore publishes:

- **For interactive workloads:** p50 and p95 per pattern per drive, over 30 rounds.
- **For bulk workloads:** p50 wall-clock over 10 rounds, plus the derived rec/sec throughput.

Different tools win each. This is a feature of the benchmark, not a bug.

---

## Measurement discipline

### OS page cache handling — the direction matters

NTFS Master File Table reads are heavily cache-sensitive. The first read of an MFT that isn't in the OS page cache costs ~50-150 MB of real disk I/O; the second read of the same MFT from cache costs ~zero. Any benchmark that doesn't disclose cache state is untrustworthy in either direction.

**Our rule for the cold-start parity comparison:** Rust runs first, Rust reads the MFT from cold disk, *then* the C++ reference runs on the same drive with the OS page cache now warmed by Rust's just-finished read. C++ gets the advantage.

We do this deliberately. It's the opposite of the direction that would make UFFS look best. We deliberately let C++ serve its MFT reads from page cache while Rust pays the full cold-disk cost. We still win 2.6× total on cold wall-clock because Rust does strictly *more* work per drive (builds the compact index + trigram + extension indexes + writes a persistent cache, in addition to the MFT read), and does it faster than C++ completes the MFT read alone from the warm cache.

**When this assumption changes:** on machines with smaller RAM where the OS page cache gets evicted between drives, or on spinning-rust HDD setups where MFT re-read costs more even from cache, the *absolute* numbers change but the *direction* of the advantage stays the same. We publish the ratio so readers can reason about their own topology.

See the [cold-parity chart](charts/2026-04-v0.5.66/cold-parity-vs-cpp.svg) and [raw log](raw/2026-04-v0.5.66_cold-parity-per-drive.txt).

### Row counts — a comparison without them is rigged

A query that returns 0 rows, 100 rows, and 167 000 rows are three different workloads. UFFS publishes the row count for every benchmark cell so readers can tell at a glance whether a latency difference is "search is faster" or "output is smaller".

**The specific failure mode we guard against:** when UFFS and Everything have different *default filters*, UFFS appears to return fewer rows for `*`-style patterns because we hide NTFS system files and Alternate Data Streams by default (matching Everything's defaults *after* the user enables Everything's equivalent toggles). If we compared `uffs *` (filtered) against `es.exe *` (unfiltered) we would appear faster partly because we're returning fewer rows. We don't do that.

**Our rule:** where defaults diverge, we normalize to matching defaults. The canonical report's §TL;DR line *"23.4 M rows after `--hide-system --hide-ads`"* is explicit about this — the 26.09 M raw record count is not the same as the 23.4 M user-visible row count, and we publish both.

### 30-round statistics and interleaved re-bench for ambiguous cells

Every interactive-query cell is measured over **30 rounds**, consecutive, back-to-back, on a hot daemon with no restart between rounds. p50 and p95 are computed from the same 30-sample distribution.

**Cells where 30 rounds are not enough:** if a cell's StdDev exceeds ~10 % of its p50, or if two tools land within 5 % of each other (within measurement noise), we do a **100-round interleaved re-bench**. Interleaving matters: we alternate UFFS → ES → UFFS → ES for 100 pairs, so any transient disk-busyness tick that affects one round affects both tools equally.

Example: C:prefix in the 2026-04 canonical report landed UFFS 99 ms vs ES 97 ms on the 30-round pass (UFFS 1.02×, a loss by 2 %). StdDev was 11 % of p50 — borderline. We ran a 100-round interleaved re-bench: UFFS 94.5 ms vs ES 95.7 ms (UFFS 0.99×, a narrow win). **We published both numbers.** The 100-round number is authoritative; the 30-round number is the context ("30-round pass hit an unlucky disk-busyness tick").

We do not cherry-pick. If the re-bench confirms the original number, we publish that and mark the cell "re-bench confirmed". If the re-bench changes the number, we publish both.

---

## Publishing discipline — what we show that's ugly

### We publish regressions

Every UFFS benchmark report has a **§Known regressions** section listing every workload where the current version is measurably slower than a prior version. As of the 2026-04 report, two such regressions are published:

1. **`*` full-scan top-100 hot regression.** v0.5.4 measured 163 ms (all drives, `--limit 100`, n=30). v0.5.66 measures 1 112 ms CLI end-to-end / 1 081 ms daemon-side (same hardware, same methodology, n=30). The regression landed during the Phase 2 top-N sort rewrite and has a bounded-heap fix tracked as Phase 5 target #2.
2. **`--sort path` regression.** `*.dll --sort path_only` regressed from ~60 ms projected to 221 ms measured on a 167 K-row C: drive. Root-caused to duplicate path resolution in the hot path; fix in progress.

Both are named in the canonical report. The raw log data shows them. Our charts would show them if we included the regressing cells — we chose not to because the charts are the *headline* view; the regression section is the *evidence*.

**Why we publish these at all:** because anyone running UFFS v0.5.66 against their v0.5.4 baseline will notice, and a benchmark report that hid them is a benchmark report nobody will trust the next time. Brand trust is an asset with a decade-long payoff. Headline numbers are an asset with a month-long payoff. We optimize for the longer horizon.

### What we explicitly don't claim

UFFS benchmark reports never make these claims, no matter how supportive the data looks:

- **"Fastest Windows file search."** Not universally true. Everything with its own in-memory index fully warm is faster than UFFS on some specific desktop-interactive scenarios (small result sets, `--sort modified` on a single drive, typical "type-and-see" usage on a laptop). We win on the documented patterns in the canonical report; we don't extrapolate to all patterns on all hardware.
- **"Universal winner."** Different tools win different workloads by design. Everything is the right tool for single-drive laptops doing quick desktop lookups. UFFS is the right tool for multi-drive workstations, scripting, bulk export, aggregation workloads, and AI-agent integration. We say this in the canonical report's §Why Everything is still a great product section.
- **"Verified on your hardware."** The numbers in any canonical report come from one test machine. Absolute latencies will drift with CPU, NVMe-vs-HDD ratio, filesystem fullness, Windows Defender configuration, and a dozen other factors. The *ratios* between tools tend to be more stable on any given machine than the absolute latencies — but we publish both so readers can reason about both.

### Test-environment transparency

Every canonical report documents:

- **CPU** (e.g. AMD Ryzen 9 3900XT)
- **RAM** (e.g. 64 GB)
- **OS and build** (e.g. Windows 11 Pro 24H2)
- **Drive topology** (e.g. 7 NTFS volumes, 3 NVMe + 3 SATA HDD + 1 USB stick, specific record counts per drive)
- **Binary versions** (e.g. UFFS Rust v0.5.66 `2ff76fb45`, UFFS C++ reference commit, Everything GUI engine 1.4.1.1032 + ES CLI 1.1.0.30 — the pinned versions live in [`scripts/windows/competitors.toml`](../../scripts/windows/competitors.toml). Note: `es.exe` is a thin IPC wrapper; the GUI daemon version determines search behaviour and must be documented alongside the CLI version.)
- **Elevation state** (both tools run elevated — UFFS needs it for raw MFT read; Everything recommends it for volume enumeration)

This isn't optional boilerplate. When a reader sees a surprising number, the first debugging step is "did they run on the same-ish hardware I have?". Disclosure is what makes that reasoning possible.

---

## Reproducibility

### Raw logs live in-tree

Every canonical report cites raw PowerShell capture logs for every benchmark cell. Those logs are committed, verbatim, to [`raw/`](raw/). The policy ([`raw/README.md`](raw/README.md)):

- **Verbatim.** Banner lines, working-directory prefix, occasional typing corrections — all preserved. No post-processing.
- **Read-only at the file level.** If a re-run produces different numbers, those numbers land in a new file. Existing files are never edited.
- **Append-only at the directory level.** Old captures live forever, by version and date.

This is the same discipline as [`archive/`](archive/) for old canonical reports. Primary sources survive; secondary presentations (reports, charts) evolve.

### Scripts live in-tree

Two benchmark scripts cover every canonical-report workload:

- [`scripts/windows/cross-tool-benchmark.rs`](../../scripts/windows/cross-tool-benchmark.rs) — the UFFS Rust vs UFFS C++ vs Everything p50/p95 harness (interactive workloads).
- [`scripts/windows/cold-parity-per-drive.ps1`](../../scripts/windows/cold-parity-per-drive.ps1) — the per-drive Rust-cold vs C++-warm-disk parity harness.

Both are runnable with one command from an elevated PowerShell in the repo root after `cargo build --release`. See the canonical report's §Reproducing this benchmark section for exact invocations.

**Rule:** if you can't reproduce a published number from these scripts on your hardware within reasonable tolerance (±10 % on absolute latency, ±20 % on cold wall-clock), we want to know. Open an issue. Benchmark reports that can't be replicated are benchmark reports with bugs.

### Archive and no-backfill

When a new canonical report supersedes the current one:

1. The new report is written against the latest binary and committed as `YYYY-MM-vX.Y.Z-<scope>.md`.
2. The previous canonical report is moved to [`archive/`](archive/) without edits.
3. New charts land in a new `charts/YYYY-MM-vX.Y.Z/` directory.
4. New raw logs land in [`raw/`](raw/) with their dated filenames.

**No backfill.** We do not retroactively construct archive entries for versions that weren't captured under the current methodology. Reconstructing a pre-v0.5.66 competitive report from 2026 with today's scientific standards would produce numbers that mix today's rigor with yesterday's informal measurements — the worst of both worlds. The first canonical snapshot is v0.5.66 (2026-04). Earlier version numbers appear in docs as historical context ("v0.5.4 measured 163 ms") but do not get standalone archive files.

---

## Known limitations of this methodology

For full transparency, the current methodology has shortcomings we're open about:

- **Single test machine.** We don't have a cross-hardware matrix. Volunteers running the same scripts on different machines would improve the external validity of every published number. See the reproduction scripts above.
- **Windows-only.** All benchmarks run on Windows. macOS and Linux are supported for *offline* MFT analysis (reading a captured MFT file), but competitive benchmarking against Everything / Mac Spotlight / locate isn't yet methodologically scoped. When that changes it'll land in a new canonical report with a separate scope line.
- **No WizFile yet.** WizFile is a relevant competitor for some workloads but has no automated-comparison flag (`--out` equivalent). When the harness can wrap it, WizFile results will appear in a future report.
- **Single C++ reference version.** We benchmark against *our own* legacy C++ reference, not against other MFT-reading C++ projects (SearchMyFiles, NTFS Search, etc.). The scope is deliberately narrow — *"UFFS vs the tool that started this project"* — so the comparison tells a single clean story about the Rust rewrite's gains.

These are not excuses; they're the backlog. Every canonical report re-evaluates whether any of them has been closed.

---

## Where to go next

- **The current numbers:** [`2026-06-v0.5.120-vs-everything.md`](2026-06-v0.5.120-vs-everything.md).
- **The shareable charts:** [`charts/2026-06-v0.5.120/`](charts/2026-06-v0.5.120/).
- **The raw logs that back every number:** [`raw/`](raw/).
- **The hub overview:** [`README.md`](README.md).
- **Internal engineering-detail source:** `docs/research/cross-tool-benchmark-analysis.md` — the forensic version of this document, currently kept internal. If you're reviewing a specific cell and suspect a methodology bug, contact the project and we'll share the relevant engineering-internal context for that cell.

If you find an error in any of the above — a misleading chart, a number that doesn't reconcile to the raw log, a methodology gap we haven't addressed here — please open an issue. Methodology documents that get corrected over time are worth more than methodology documents that claim to be final.
