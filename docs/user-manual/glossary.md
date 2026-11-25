# Glossary

The terms the chapters of this manual rely on, in alphabetical order. Each
definition is short; the link after it leads to the chapter that explains
the term in full.

| Term | Definition |
|------|------------|
| **8.3 name** | The short DOS-style name (`PROGRA~1`) NTFS can keep beside a file's long name. The MFT holds both. |
| **Access Broker** | `uffs-broker`, a small Windows service (LocalSystem) installed once with `uffs-broker --install`. It opens the drives for the daemon, so searches never need Administrator or a UAC prompt. See [Installation](installation.md). |
| **ADS** | Alternate Data Stream: an extra, named stream of data attached to an NTFS file, hidden from Explorer. UFFS lists each one as `file:stream`; `--hide-ads` leaves them out. See [Concepts](concepts.md). |
| **Aggregation** | A summary computed in the daemon over every matching file (counts, sizes, groups, histograms, duplicates) instead of a list of rows. See [Aggregation](aggregation.md). |
| **Allocated size** | The disk space a file occupies, rounded up to whole clusters; Explorer's "size on disk". The column and sort key is `size_on_disk`. See [Concepts](concepts.md#1-size-vs-size-on-disk). |
| **Bulkiness** | Allocated size divided by logical size, as a percentage: 100 means no wasted space, more means slack in partly used clusters, less means compression. Directories use their tree totals. See [Concepts](concepts.md#3-bulkiness-waste-ratio). |
| **Capture** | A copy of a drive's MFT saved to a file (`.bin`, `.iocp`, `.raw`, `.mft`), so it can be searched later or on macOS and Linux. See [Cache & Data Sources](cache-and-data.md). |
| **Cluster** | The smallest unit NTFS allocates, usually 4 KB. A file always occupies whole clusters. |
| **Compact index** | The daemon's in-memory form of a drive's MFT, laid out for fast scanning; what a search actually reads. |
| **Daemon** | `uffsd`, the background process that holds each drive's index in memory and answers searches. It starts on the first search, one per user. See [Daemon](daemon.md). |
| **Descendants** | For a directory, the number of entries in its whole subtree plus one for the directory itself (an empty directory has 1); for a file, 0. See [Concepts](concepts.md#4-descendants). |
| **Elevated** | Running as Administrator (Windows) or root. Reading a live MFT needs it, unless the Access Broker is installed. |
| **Extension record** | A second MFT record that holds the overflow of a file with many names or streams. UFFS always merges it into the file's main record. |
| **FRS** | File Record Segment number: a file's record number in the MFT, unique per drive. |
| **Glob** | A pattern with `*` (any run of characters) and `?` (one character), such as `*.pdf`; `\|` separates alternatives. See [Search Modes](search-modes.md). |
| **Hard link** | One of several names (possibly in different folders) for the same file data. The file has one FRS and several names. |
| **Idle retirement** | The daemon exits by itself after 2 hours without a search (6 hours once a TUI, GUI or MCP client has connected), unless it is resident. See [Daemon](daemon.md). |
| **Index cache** | The encrypted per-drive file (`C_index.uffs`) the daemon writes after reading a drive, so the next start loads it in seconds instead of re-reading the MFT. See [Cache & Data Sources](cache-and-data.md). |
| **.iocp** | A capture format: a drive's MFT saved in the order the reads completed (`uffs-mft save`). It is a source, not the cache. See [Cache & Data Sources](cache-and-data.md). |
| **IPC** | The local connection between `uffs` and the daemon: a named pipe on Windows, a Unix socket on macOS and Linux. |
| **Logical size** | The number of bytes in a file, independent of how much disk space it takes. The `size` column. |
| **MAX_PATH** | The 260-character path limit of older Windows programs; `--min-path-length` finds paths near it. See [Concepts](concepts.md). |
| **MCP** | Model Context Protocol, the open standard AI agents use to call tools. `uffsmcp` serves UFFS search over it. See [MCP Server](mcp.md). |
| **Memory tier** | How much of a drive's index the daemon keeps in RAM: `Warm` (searchable), `Parked` (after 30 minutes idle), `Cold` (after 24 hours), and `Hot`, which only `uffs --daemon preload` sets. See [Daemon](daemon.md#memory-tiers-and-why-you-never-see-hot). |
| **MFT** | Master File Table: the table at the heart of every NTFS drive, one record per file and directory with its names, sizes, timestamps and attributes. UFFS reads it directly. See [Concepts](concepts.md). |
| **MFT bitmap** | The bitmap that marks which MFT records are in use, so a reader can skip the free ones. |
| **NDJSON** | Newline-delimited JSON, one object per line: the output of `--format json`. See [Output Formats](output-formats.md). |
| **NTFS** | The Windows file system UFFS reads. Other file systems (FAT, exFAT, ReFS) have no MFT and are not searched. |
| **Parquet** | A columnar file format; `uffs-mft` can export a drive's MFT to it for analysis in other tools. |
| **Path resolution** | Rebuilding a file's full path from the MFT, which stores only each name and its parent's record number. |
| **Preset** | A named, ready-made aggregation such as `overview`, `by_type` or `duplicates`. See [Aggregation](aggregation.md). |
| **Regex** | A regular expression, written after a `>` (`>.*\.log$`); it is matched against file names. See [Search Modes](search-modes.md). |
| **Reparse point** | An NTFS marker that turns an entry into a symbolic link, junction or mount point. The `reparse` attribute. |
| **Resident** | A daemon that starts at login and never retires: `uffs --daemon resident on`. See [Daemon](daemon.md). |
| **Smart case** | `--smart-case`: a search is case-sensitive only when the pattern contains a capital letter. See [Search Modes](search-modes.md). |
| **Snapshot** | A baseline copy of a drive's MFT saved with `uffs --snapshot`, so a later `uffs --diff` can show what was deleted since. See [CLI Overview](cli-overview.md). |
| **Tree allocated** | For a directory, the allocated size of everything in its subtree. See [Concepts](concepts.md#2-tree-size-and-tree-allocated). |
| **Tree size** | For a directory, the logical size of everything in its subtree. See [Concepts](concepts.md#2-tree-size-and-tree-allocated). |
| **uffsd** | The daemon's program. `uffs` starts it when it is not running. |
| **uffsmcp** | The MCP server's program, started by an AI host or with `uffs --mcp start`. See [MCP Server](mcp.md). |
| **USN journal** | NTFS's change journal. The daemon reads it to keep a live drive's index current without re-reading the MFT. |
| **Watchdog** | `uffs-watchdog`, which restarts a resident daemon on Windows after a crash or an installer. See [Daemon](daemon.md#the-watchdog-surviving-crashes-and-installers). |
| **WinGet** | The Windows package manager; `winget install SkyLLC.UFFS` installs UFFS. See [Installation](installation.md). |
