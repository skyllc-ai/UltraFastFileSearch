![UFFS — Ultra Fast File Search](../../assets/brand/uffs-wordmark.png)

# UFFS User Manual

UFFS (Ultra Fast File Search) finds files on NTFS drives by reading each
drive's Master File Table directly and keeping it in memory, so a search
over millions of files answers in milliseconds. This manual covers the
command line `uffs`, the background daemon, the terminal UI and the MCP
server for AI agents. It describes the latest release; the changelog at
the root of the repository lists what changed in each one.

Read the chapters in this order; each one builds on the ones before it.

1. [Installation](installation.md): WinGet, the release zips, the one-line
   installer, building from source, and putting UFFS on your PATH.
2. [Getting Started](getting-started.md): your first search, what the
   output means, narrowing results, and a gallery of everyday recipes.
3. [Concepts](concepts.md): size and size on disk, tree size, bulkiness,
   descendants, file types, attributes and timestamps, and how UFFS reads
   the MFT.
4. [CLI Overview](cli-overview.md): the shape of a `uffs` command, every
   flag at a glance, and the management commands.
5. [Search Modes](search-modes.md): glob, literal and regex patterns,
   path-aware patterns, case, whole words and drive scoping.
6. [Filters](filters.md): size, date, attribute, extension, type, path,
   tree-size and length filters, and how to combine them.
7. [Sorting](sorting.md): every sort column, direction, multi-tier sorts
   and deterministic ordering.
8. [Output Formats](output-formats.md): CSV, JSON and table output,
   choosing columns, and scripting with the results.
9. [Aggregation](aggregation.md): summaries instead of rows, with presets,
   custom specs, duplicates and recipes.
10. [Daemon](daemon.md): the resident index server, how it starts, stays
    resident and retires, and how to manage it.
11. [Cache & Data Sources](cache-and-data.md): live drives, MFT captures and
    the index cache, and offline analysis on macOS and Linux.
12. [TUI Search Box](tui-search-box.md): the terminal UI, its patterns,
    toggles and keys.
13. [MCP Server](mcp.md): connecting Claude, Cursor, Windsurf and other AI
    agents to UFFS through the Model Context Protocol.
14. [Updating](updating.md): `uffs --update`, pinning a version, rolling
    back, and the health check.
15. [Uninstalling](uninstall.md): `uffs --uninstall`, the dry run, and what
    is removed.
16. [Advanced Diagnostics](advanced-diagnostics.md): profiling,
    benchmarking, logging and the environment variables.
17. [Performance](performance.md): measured numbers per drive, query
    latency, bulk export throughput and how to benchmark your own machine.
18. [Troubleshooting](troubleshooting.md): common errors and their fixes.
19. [FAQ](faq.md): short answers to the questions people ask first.
20. [Glossary](glossary.md): MFT, FRS, cluster, tree size and the other
    terms the chapters use.
