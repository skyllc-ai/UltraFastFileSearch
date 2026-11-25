# FAQ

Frequently asked questions about UFFS.

### Why is UFFS so fast?

UFFS reads the NTFS Master File Table (MFT) directly, bypassing the
Windows file enumeration APIs.  The MFT is a flat database of every file
on a drive.  Reading it sequentially is orders of magnitude faster than
walking the directory tree.

Once loaded, the MFT data is held in a compact in-memory index optimised
for search.  Queries run against this index in microseconds.

> **Deep dive:** [Concepts](concepts.md)

### Does UFFS work on macOS and Linux?

Yes, with offline MFT captures.  UFFS cannot read NTFS drives natively
on non-Windows platforms, but it can analyse MFT data exported from
a Windows machine.

> **Setup guide:** [Cache & Data Sources](cache-and-data.md)

### Does UFFS need Administrator privileges?

On Windows, **once** — reading the MFT requires elevated access, but
installing the Access Broker (`uffs-broker --install`) grants it a
single time, after which every search, daemon start/stop, and update
runs unelevated with no UAC prompt.  Without the broker you need an
Administrator terminal each time.  On macOS and Linux, no — UFFS reads
regular files (MFT captures).

> **Details:** [Installation §3](installation.md#3-platform-requirements)

### Why does the first search take so long?

The daemon is loading the MFT and building the in-memory index.  This
only happens once (~7 s from cache, or ~66 s cold for a large system).
Every subsequent search completes in ~200 ms end-to-end.
See [Performance](performance.md) for full benchmark data.

### How much memory does UFFS use?

It depends on the number of files.  Rough benchmarks:

| Files indexed | Daemon RSS |
|---------------|-----------|
| 1 million | ~300 MB |
| 5 million | ~1.2 GB |
| 25 million | ~4–6 GB |

The daemon retires after 2 hours idle, releasing all memory.

### Can I search file contents?

No.  UFFS searches file metadata — names, paths, sizes, timestamps,
and NTFS attributes.  It does not read file contents.

For content search, use tools like ripgrep (`rg`) or Windows Search.

### What is the difference between Size and SizeOnDisk?

**Size** is the logical file size — the number of data bytes.
**SizeOnDisk** is the allocated size — the actual disk space consumed,
rounded up to the cluster size (usually 4 KB).

A 1-byte file has Size=1 but SizeOnDisk=4096.

> **Full explanation:** [Concepts §1](concepts.md#1-size-vs-size-on-disk)

### What is Bulkiness?

Bulkiness = (SizeOnDisk / Size) × 100.  It measures allocation waste.

- **100** — perfectly efficient (file fills its clusters exactly)
- **500** — 5× more disk space than logical data (wasteful)
- **409600** — a 1-byte file using one 4 KB cluster

> **Guide:** [Concepts §3](concepts.md#3-bulkiness-waste-ratio)

### Does UFFS see recently created files?

The daemon loads the MFT at startup and holds it in memory.  Files
created after startup are not visible until you restart the daemon:

```bash
uffs --daemon restart
```

### Can UFFS delete or move files?

No.  UFFS is read-only — it searches and reports.  To act on results,
pipe the output to other tools:

```bash
uffs '*.bak' --columns Path --header false | xargs rm -v
```

### What is the `.iocp` file format?

The `.iocp` file is UFFS's serialised binary cache.  It stores the
parsed MFT index so the daemon can start quickly without re-parsing
the raw MFT data.

> **Details:** [Cache & Data Sources §3](cache-and-data.md#3-mft-file-formats)

### How do I update the index after file changes?

```bash
# Restart daemon (re-reads MFT or cache)
uffs --daemon restart

# Force re-parse (bypass .iocp cache)
uffs --daemon restart --no-cache
```

### Can multiple users share one daemon?

The daemon listens on a local IPC socket (Unix domain socket on
macOS/Linux, named pipe on Windows).  By default, it is scoped to the
current user.  Multiple CLI sessions, TUI instances, and MCP clients
from the same user share one daemon.

### What NTFS features does UFFS support?

| Feature | Supported | Notes |
|---------|-----------|-------|
| Filenames | ✓ | Long and short (8.3) names |
| Timestamps | ✓ | Created, modified, accessed |
| File sizes | ✓ | Logical and allocated |
| NTFS attributes | ✓ | All 19 boolean flags |
| Hard links | ✓ | With `--full` mode |
| Alternate Data Streams | ✓ | With `--full` mode |
| Compressed files | ✓ | Detected via attribute flag |
| Encrypted files | ✓ | Detected via attribute flag |
| Sparse files | ✓ | Detected via attribute flag |
| Reparse points / symlinks | ✓ | Detected via attribute flag |
| File contents | ✗ | Metadata only |
| ACLs / permissions | ✗ | Not read |

## See also

- [Getting Started](getting-started.md): your first search and everyday recipes
- [Troubleshooting](troubleshooting.md): common errors and their fixes
- [Glossary](glossary.md): the terms the chapters use
