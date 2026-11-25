# Cache and Data Sources

UFFS reads NTFS data from three types of sources.  This page explains
each source, the caching pipeline, and how to set up offline MFT
analysis on non-Windows platforms.

## 1. Data Sources

The daemon does not pick one source over another: it loads **all** of
the sources it is given, together.

```
--mft-file <PATH>        The capture file(s) you name
--data-dir <DIR>         Plus one capture found in each drive_* folder
(Windows only)           Plus the live NTFS drives (all of them, or the
                         ones named with --drive)
```

On Windows, `--drive` limits both the live drives and the `drive_*`
folders of `--data-dir` to the letters you name.

### Source Selection Flags

| Flag | Description |
|------|-------------|
| `--mft-file <PATH>` | Load specific MFT capture file(s). Comma-separated for multiple (see the note below). |
| `--data-dir <DIR>` | Auto-discover MFT files in `drive_c/`, `drive_d/`, etc. |
| `--no-cache` | Bypass the cache; always re-parse the MFT data |

> A comma-separated `--mft-file` list works with `uffs --daemon start`
> and `uffs --daemon load`.  When a search starts the daemon for you,
> the list is passed on as a single path and fails, so start the daemon
> first:
>
> ```bash
> uffs --daemon start --mft-file ~/mft/C_mft.iocp,~/mft/D_mft.iocp
> ```

## 2. The Data Pipeline

```
MFT capture or            Index cache             In-memory index
live drive       parse    (<L>_index.uffs) load   (daemon)
  ──────────────────────▶  ──────────────────────▶  ┌──────────┐
    ~10-30 s                   ~2-5 s               │ Ready to │
    (first time only)          (later starts)       │  search  │
                                                    └──────────┘
```

**First run (COLD):** The MFT data is parsed, the index is built, and
the index is saved to the cache.  This takes 5–67 seconds per drive
depending on record count and media type (NVMe vs HDD).

**Subsequent runs (WARM CACHE):** The daemon loads the cached index,
applies the changes recorded in the USN journal since (live Windows
drives only), and rebuilds the search index, skipping the expensive MFT
parse.  Startup drops to 2–5 seconds per drive (~7 s total for 7 drives
/ 25.9M records).

The cache is used for **4 hours**; an older cache is ignored and the
MFT data is parsed again.

## 3. MFT File Formats

These are the capture formats UFFS reads.  None of them is the cache.

| Extension | Source | Description |
|-----------|--------|-------------|
| `.iocp` | `uffs-mft save --iocp` | MFT capture in the order Windows delivered the reads (UFFS-IOCP format) |
| `.bin` | `uffs-mft save` | MFT capture (zstd-compressed unless `--no-compress`) |
| `.raw` | `uffs-mft save --raw` | Bare MFT bytes, readable by other MFT tools |
| `.mft` | Third-party tools, or `uffs-mft save` | Standard raw MFT dump |
| `.uffs` | UFFS cache | A saved index (see [Cache Location](#5-cache-location)) |

When the daemon starts with `--data-dir`, it takes one file per
`drive_*` folder and prefers `.iocp` over `.bin` over `.mft` (it does
not pick `.raw` or `.uffs` files there; name them with `--mft-file`).
`uffs --daemon load --data-dir` prefers `.iocp`, then `.uffs`, `.bin`,
`.raw`, `.mft`.

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

The folder name decides which folders `--drive` selects.  The drive
letter the file is loaded as comes from the **first character of the
file name** (see [§6](#6-drive-letter-inference)), so start each file
name with its drive letter.

### Setting Up on macOS / Linux

1. On a Windows machine, capture the MFT for each drive:

   ```powershell
   # Option A: UFFS native (the uffs-mft tool; --raw = bare MFT bytes)
   uffs-mft save --drive C --output C_mft.bin --raw

   # Option B: Third-party (e.g. RawCopy, FTK Imager)
   # Produces a .mft file
   ```

   For a full-volume capture, its transfer and checks, engineers can
   follow [MFT capture, transfer and verify](../architecture/mft-capture-verify-runbook.md).

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

Every cache goes to one per-user cache directory, whatever the source
(live drive or capture file).  The files are encrypted and
zstd-compressed, and are named by drive letter:

| File | What it holds |
|------|---------------|
| `<L>_index.uffs` | The full index of drive `<L>` |
| `<L>_compact.uffs` | The compact search index of drive `<L>` |
| `<L>_usn.cursor` | Where the USN journal was last read (live drives) |

### Platform Cache Directories

| Platform | Directory |
|----------|-----------|
| Windows | `%LOCALAPPDATA%\uffs\cache\` |
| macOS | `~/Library/Caches/com.uffs/` |
| Linux | `$XDG_CACHE_HOME/uffs/` (default `~/.cache/uffs/`) |

Because the cache is keyed **only by drive letter**, a new capture of a
drive whose cache is less than 4 hours old is not read: the daemon
loads the old cache.  Load the new capture with `--no-cache`.

### Bypassing the Cache

`--no-cache` applies when a daemon **starts**.  On a search it takes
effect only if that search starts the daemon; a running daemon ignores
it.  So stop the daemon first:

```bash
uffs --daemon stop
uffs --daemon start --data-dir ~/uffs_data --no-cache
```

To load one more capture into a running daemon without its cache, use
`uffs --daemon load --mft-file <PATH> --no-cache`.  To delete a drive's
cache files for good, use `uffs --daemon forget <DRIVE>` (see
[Daemon](daemon.md)).

Use `--no-cache` when:

- You have updated your MFT captures with fresh data.
- You suspect cache corruption.
- You are benchmarking raw parse performance.

## 6. Drive Letter Inference

UFFS takes the drive letter of a capture file from the **first
character of its file name**:

| Filename | Inferred drive |
|----------|---------------|
| `C.bin` | C: |
| `C_mft.iocp` | C: |
| `D_mft.bin` | D: |
| `mft_c.iocp` | M: (the first character) |

There is no flag that overrides this.  If a file name does not start
with the drive letter, rename the file:

```bash
mv /path/to/unknown.bin /path/to/C_unknown.bin
```

## See also

- [Installation](installation.md): install UFFS and put it on your PATH
- [Daemon](daemon.md): the resident index server and how to manage it
- [Concepts](concepts.md): what each size, count and attribute means
- [Performance](performance.md): measured numbers and how to benchmark your machine
