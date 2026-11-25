# UFFS Benchmark Archive

Every canonical benchmark report lives in this directory once superseded by a newer version. **Archived reports are never retroactively edited, re-measured, or pruned.** They are frozen-in-time primary sources.

## Why archive at all?

Benchmarks age. The numbers in any given report are true for one binary version, one test machine, one OS revision, one filesystem state. Six months later, some numbers have improved, some have regressed, some methodology tightened. All of that is interesting — but only if the old numbers are still here to compare against.

Three uses for this archive:

1. **Credibility over time.** *"Here's what we claimed in 2026-Q1, here's what we claimed in 2026-Q3, here's the regression we caught between them and here's when we fixed it."* That story only works if both reports survive verbatim.
2. **Honest regression tracking.** When a number gets worse between versions, the archived prior number is the forensic evidence that it *was* better once. The canonical report flags the regression; the archive holds the proof.
3. **Methodology evolution.** The benchmark methodology itself improves — more rounds, better isolation, new size classes, new patterns. Archived reports document what the methodology looked like at the time, so apples-to-apples comparison across eras is traceable.

## Naming convention

```
archive/YYYY-MM-vX.Y.Z-<scope>.md
```

Examples:
- `2026-04-v0.5.66-vs-everything-and-cpp.md` — the April 2026 canonical report, head-to-head against Everything and UFFS C++.
- `2026-Q3-v0.6.0-phase-5-regression-fixes.md` *(hypothetical)* — the snapshot after Phase 5 closes the known regressions noted in the 2026-04 report.

**Date-first so the filesystem sorts chronologically.** **Version next** so the binary measured is unambiguous. **Scope last** so readers can skim the list and see what each report covers.

## What goes in each archive entry

Exactly the same template as the current canonical report:

1. **Headline + test environment** at the top (what, when, what machine, what binaries).
2. **TL;DR with 3-5 numbers** — the claims the report is making.
3. **Methodology block** — cold vs warm vs hot separation, interactive vs bulk, the fairness promise.
4. **Head-to-head comparison tables** — one per competitor / reference point, with p50 + p95 + row counts.
5. **Scale ceiling** — memory curves, bulk export, aggregations at the tested scale.
6. **Known regressions** — published openly, with root cause and fix-in-progress notes.
7. **Test environment details + reproduce instructions** — exact scripts and raw log paths.
8. **References back to product docs + competitor sources.**

See [`2026-04-v0.5.66-vs-everything-and-cpp.md`](2026-04-v0.5.66-vs-everything-and-cpp.md) for the first example.

## The no-backfill rule

We do **not** retroactively construct archive entries for versions that weren't captured under the same methodology at their own release time. Attempting to reconstruct a v0.5.4 competitive benchmark *from memory* in 2026-04 would produce numbers that mix today's scientific standards with yesterday's informal measurements — the worst of both worlds.

The first canonical snapshot is **v0.5.66 (2026-04)**. Earlier version numbers in docs (v0.5.4, v0.4.106) are referenced where relevant as historical context, but they do not get a standalone archive file.

## Workflow when a new canonical report lands

1. The new report is written against the latest binary, tested, and committed as `docs/benchmarks/YYYY-MM-vX.Y.Z-<scope>.md`.
2. The previous canonical report is `git mv`'d into `archive/` **without edits** (same filename).
3. `docs/benchmarks/README.md` updates the *Current canonical report* section to point at the new report.
4. `docs/benchmarks/README.md` archive section gets a new bullet naming the freshly archived prior report.
5. The main `README.md` proof-strip numbers are refreshed if they moved. Stale numbers in deep-dive docs get cross-checked.
