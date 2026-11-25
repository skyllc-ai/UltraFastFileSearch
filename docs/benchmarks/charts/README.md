# UFFS benchmark charts

Version-dated, shareable SVG charts generated from the canonical benchmark reports. Designed for:

- **Embedding in the hub + canonical report** (renders natively on GitHub, works in light + dark mode because of the explicit white-card design).
- **Screenshotting for social posts** (HN, Reddit, Twitter/X, LinkedIn). The white-card design is readable on any platform theme and crops cleanly at 2:1 or 16:9.
- **Click-through verification** — every chart cites the raw log file in [`../raw/`](../raw/) that produced its numbers, so readers can audit each bar.

## Layout

```
charts/
├── README.md                  <- this file
├── 2026-06-v0.5.120/          <- current canonical snapshot
│   ├── head-to-head-vs-everything.svg     (suite-generated: 30/30 vs Everything, median 0.36×)
│   ├── daemon-hot-vs-cpp.svg              (suite-generated: daemon HOT vs per-invocation MFT re-read)
│   ├── full-scan-throughput.svg           (suite-generated: 4.8 s / 10.2 M rows / 2.11 M rec/s)
│   ├── cold-parity-vs-cpp.svg             (v0.5.66 capture, carried forward — not re-measured)
│   └── memory-scales-linearly.svg         (v0.5.66 capture, carried forward — not re-measured)
└── 2026-04-v0.5.66/           <- prior snapshot (hand-written era, brand-restyled)
    ├── head-to-head-vs-everything.svg     (§Head-to-head 1: 12/12 vs Everything)
    ├── cold-parity-vs-cpp.svg             (§Head-to-head 2: cold-start parity)
    ├── daemon-hot-vs-cpp.svg              (§Head-to-head 2: daemon HOT steady-state)
    ├── memory-scales-linearly.svg         (§Scale: linear RSS growth)
    └── full-scan-throughput.svg           (§Scale: 13.6 s / 23.4 M / 1.72 M rec/s)
```

## Design system

Charts follow the **UFFS brand kit** (`docs/dev/architecture/brand-kit/STYLE_GUIDE.md`) — rust orange on charcoal, deliberately *not* the cool blue of every other file-search tool:

- **Charcoal card background** `#0F0D0B` with a subtle `#1E1B18` 1 px border — explicit colors, no `currentColor` / theme-inheritance games; the chart looks identical on dark HN, light Twitter, any IDE preview.
- **Light-on-charcoal typography**: Cream `#F2EDE8` titles and body labels, Sand `#9A8D82` subtitles, axes, and footers.
- **Accent colors** are fixed per concept across every chart:
  - UFFS Rust: `#CE422B` (Rust Orange — the brand anchor)
  - Everything: `#9A8D82` (Sand, muted)
  - UFFS C++ reference: `#DEA584` (Paper Tan)
  - Winning ratio callouts: `#F7B26B` (Ember)
  - Architectural ceiling warnings: `#B03618` (Crimson) borders with Ember text
- **Font:** Inter, falling back to the system stack (`-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, ...`) — renders cleanly on macOS, Windows, Linux, and in browsers.
- **ViewBox-based responsive sizing** so GitHub / Markdown renderers can scale them to any container width without pixelating.

## When to update

When a new canonical benchmark report is cut (e.g. v0.5.70 supersedes v0.5.66):

1. Create a new dated directory: `charts/YYYY-MM-vX.Y.Z/`.
2. Re-produce the same five charts with the new numbers using the same design system.
3. Update the hub README and the new canonical report to embed the new chart paths.
4. Leave the old directory untouched — prior charts live forever, same archive discipline as [`../archive/`](../archive/) and [`../raw/`](../raw/).

## Regeneration

**Since v0.5.120+ the bench suite generates the competition charts automatically.** Every `just bench-suite` run writes three SVGs into `bundle/charts/` straight from that run's `cross-tool-summary.csv` (see `crates/uffs-bench/src/charts.rs`):

- `head-to-head-vs-everything.svg` — UFFS vs Everything p50 per (drive, pattern) cell
- `daemon-hot-vs-cpp.svg` — UFFS daemon HOT vs the C++ reference's per-invocation MFT re-read
- `full-scan-throughput.svg` — UFFS-only `*` → CSV export (rows + sustained rec/s)

All are styled per the **UFFS brand kit** (`docs/dev/architecture/brand-kit/STYLE_GUIDE.md`): Charcoal card, Cream/Sand typography, Rust Orange `#CE422B` UFFS bars, Sand competitor bars, Ember win callouts. Promotion = copy the charts out of the bundle into a new dated directory here.

The 2026-04 set predates generation and was hand-written; its **numbers are frozen** (same data, same raw-log citations) but the look was restyled in-place to the brand palette — the original white-card/blue design predated the brand kit. Two charts of that era (cold-parity, memory-scaling) came from **internal engineering tools** — production-diff parity checks and memory-footprint work — not competition benchmarks, so the suite does not re-measure them. They served their purpose but remain interesting data points, so each new snapshot directory **carries them forward verbatim** (the chart itself states the version it was captured on).

No external dependencies, no renderer to install, no chart-library version drift. The text of the SVG **is** the source of truth; there's no `.png` fallback to keep in sync.
