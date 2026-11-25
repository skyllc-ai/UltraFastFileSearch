# Glossary

Key terms used throughout the UFFS documentation.

---

| Term | Definition |
|------|-----------|
| **ADS** | Alternate Data Stream — NTFS feature allowing multiple data streams per file. Hidden from Explorer by default. Use `--full` to index them. |
| **Allocated size** | The actual disk space consumed by a file, rounded up to the nearest cluster boundary. Also called "size on disk". See [Concepts §1](concepts.md#1-size-vs-size-on-disk). |
| **Bulkiness** | The ratio of allocated size to logical size, expressed as a percentage. A measure of allocation waste. 100 = perfectly efficient. See [Concepts §3](concepts.md#3-bulkiness-waste-ratio). |
| **Cluster** | The smallest unit of disk allocation in NTFS. Typically 4 KB (4096 bytes). Files smaller than one cluster still consume one full cluster. |
| **Compact index** | UFFS's in-memory representation of the MFT — a struct-of-arrays layout optimised for search and aggregation. |
| **Daemon** | The UFFS background process that holds the MFT index in memory and serves search queries over IPC. See [Daemon](daemon.md). |
| **DataFrame** | A Polars columnar data structure. UFFS uses DataFrames internally for query execution. |
| **Descendants** | The total number of files and subdirectories inside a directory (recursive count). See [Concepts §4](concepts.md#4-descendants-child-count). |
| **Extension record** | An MFT record that continues the attributes of another record. Used when a file has many hard links or ADS entries. Indexed in `--full` mode. |
| **FRS** | File Reference Segment — the MFT record number uniquely identifying a file or directory on a drive. Also called "file reference number". |
| **Hard link** | An NTFS feature allowing multiple directory entries to point to the same file data. The file has one FRS but multiple names and parent directories. |
| **Idle retirement** | The daemon's automatic shutdown after being idle for a configurable period (default: 2 hours). |
| **.iocp** | UFFS's serialised binary cache format. Stores a parsed MFT index for fast daemon startup. See [Cache & Data Sources](cache-and-data.md). |
| **IPC** | Inter-Process Communication — the local socket (Unix domain socket on macOS/Linux, named pipe on Windows) used between CLI and daemon. |
| **Logical size** | The actual data size of a file in bytes, independent of disk allocation. The number you see in `Size` columns. |
| **MCP** | Model Context Protocol — an open standard for AI agent tool integration. UFFS exposes an MCP server for filesystem search. See [MCP](mcp.md). |
| **MFT** | Master File Table — the central metadata database of an NTFS volume. Contains one record per file/directory with name, size, timestamps, and attributes. |
| **MFT bitmap** | A bitmap indicating which MFT records are in use. UFFS uses this to skip free records (faster reads). Disabled with `--no-bitmap`. |
| **NDJSON** | Newline-Delimited JSON — one JSON object per line. UFFS's `--format json` output format. |
| **Parquet** | Apache Parquet — a columnar file format. UFFS can export the MFT to Parquet (daemon-managed, or via the `uffs-mft` tool) for external analysis. |
| **Path resolution** | The process of reconstructing full file paths from MFT data. The MFT stores only filenames and parent FRS numbers, not full paths. UFFS's `FastPathResolver` walks the parent chain. |
| **Reparse point** | An NTFS feature for symlinks, junctions, and volume mount points. Detected via the reparse attribute flag. |
| **SoA** | Struct of Arrays — a data layout where each field is stored in a separate contiguous array. UFFS uses SoA for the compact index, enabling SIMD-friendly scans. |
| **Treesize** | The recursive logical size of a directory subtree — the sum of sizes of all files in the directory and its descendants. See [Concepts §2](concepts.md#2-tree-size-and-tree-allocated). |
| **Tree allocated** | Same as treesize but for allocated (on-disk) sizes. |
