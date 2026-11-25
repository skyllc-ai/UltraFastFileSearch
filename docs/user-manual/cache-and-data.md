# Cache and Data Sources

UFFS reads NTFS data from three types of sources.  This page explains
each source, the caching pipeline, and how to set up offline MFT
analysis on non-Windows platforms.

## 1. Data Source Priority

When the daemon starts, it looks for data in this order:

```
1. --mft-file <PATH>        Explicit raw MFT file(s)
2. --data-dir <DIR>          Auto-discover MFT files in drive_* dirs
3. (Windows only)            Live NTFS MFT from detected drives
```

### Source Selection Flags

| Flag | Description |
|------|-------------|
| `--mft-file <PATH>` | Load specific raw MFT file(s). Comma-separated for multiple. |
| `--data-dir <DIR>` | Auto-discover MFT files in `drive_c/`, `drive_d/`, etc. |
| `--no-cache` | Bypass cache; always re-parse raw MFT data |

## 2. The Data Pipeline

```
Raw MFT data              Cached index            In-memory index
 (.bin, .mft)    parse     (.iocp)      deserial.    (daemon)
  ──────────────────────▶  ──────────────────────▶  ┌──────────┐
    ~10-30 s                   ~2-5 s               │ Ready to │
    (first time only)          (every start)        │  search   │
                                                    └──────────┘
```

**First run (COLD):** Raw MFT data is parsed, the index is built, and a
`.iocp` cache file is written.  This takes 5–67 seconds per drive
depending on record count and media type (NVMe vs HDD).

**Subsequent runs (WARM CACHE):** The daemon loads the `.iocp` cache
directly, skipping the expensive MFT parse step.  Startup drops to
2–5 seconds per drive (~7 s total for 7 drives / 25.9M records).

## 3. MFT File Formats

UFFS accepts three raw MFT file formats:

| Extension | Source | Description |
|-----------|--------|-------------|
| `.iocp` | UFFS cache | Serialized binary index — fastest to load |
| `.bin` | `uffs-mft save` | Raw MFT byte dump |
| `.mft` | Third-party tools | Standard raw MFT dump |

When auto-discovering files in `--data-dir`, UFFS prefers `.iocp` over
`.bin` over `.mft`.

## 4. Data Directory Layout

The `--data-dir` flag expects a directory with subdirectories named
`drive_<letter>/`, each containing one MFT capture:

```
~/uffs_data/
├── drive_c/
│   └── C_mft.iocp          (or C.bin, C.mft)
├── drive_d/
│   └── D_mft.iocp
├── drive_e/
│   └── E_mft.iocp
└── drive_f/
    └── F_mft.bin
```

The drive letter is inferred from the subdirectory name (case-
insensitive).  The filename inside can be anything — only the directory
name matters.

### Setting Up on macOS / Linux

1. On a Windows machine, capture the MFT for each drive:

   ```powershell
   # Option A: UFFS native (the uffs-mft tool; --raw = bare MFT bytes)
   uffs-mft save --drive C --output C_mft.bin --raw

   # Option B: Third-party (e.g. RawCopy, FTK Imager)
   # Produces a .mft file
   ```

2. Copy the files to your Mac/Linux machine and organise them:

   ```bash
   mkdir -p ~/uffs_data/drive_c ~/uffs_data/drive_d
   cp C_mft.bin ~/uffs_data/drive_c/
   cp D_mft.bin ~/uffs_data/drive_d/
   ```

3. Run your first search — the cache is built automatically:

   ```bash
   uffs '*.txt' --data-dir ~/uffs_data
   ```

## 5. Cache Location

Cache files (`.iocp`) are written alongside the source MFT file by
default.  The daemon looks for a cached version before parsing the
raw source.

| Input | Cache file written to |
|-------|-----------------------|
| `~/uffs_data/drive_c/C.bin` | `~/uffs_data/drive_c/C.iocp` |
| `/tmp/mft/D_mft.mft` | `/tmp/mft/D_mft.iocp` |
| (Windows live C:) | Platform cache directory |

### Platform Cache Directories (Windows Live Drives)

| Platform | Directory |
|----------|-----------|
| Windows | `%LOCALAPPDATA%\uffs\cache\` |

### Bypassing the Cache

```bash
# Force re-parse (ignore existing .iocp files)
uffs '*.dll' --no-cache --data-dir ~/uffs_data

# Also available on daemon start
uffs --daemon start --data-dir ~/uffs_data --no-cache
```

Use `--no-cache` when:

- You have updated your MFT captures with fresh data.
- You suspect cache corruption.
- You are benchmarking raw parse performance.

## 6. Drive Letter Inference

When using `--mft-file`, UFFS infers the drive letter from the
filename:

| Filename | Inferred drive |
|----------|---------------|
| `C.bin` | C: |
| `C_mft.iocp` | C: |
| `D_mft.bin` | D: |
| `drive_e.mft` | E: |

If inference fails, use `--drive` or `--drives` to specify explicitly:

```bash
uffs '*' --mft-file /path/to/unknown.bin --drive C
```

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Daemon](daemon.md): the resident index server and how to manage it
- [Concepts](concepts.md): what each size, count and attribute means
- [Performance](performance.md): measured numbers and how to benchmark your machine
