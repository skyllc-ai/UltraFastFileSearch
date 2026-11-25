<!-- SPDX-License-Identifier: MPL-2.0 -->
<!-- Copyright (c) 2025-2026 SKY, LLC. -->

# NTFS Full-Volume Capture (`uffs-mft capture`)

> **Status:** In progress — P0–P2, P4, and P6 (offline) shipped: `sysinfo`; all
> metafile `save` targets incl. `$UsnJrnl` via `$Extend` traversal; the
> `capture` orchestrator/manifest incl. the compressed `$MFT`; and offline
> `metafile-info` with `$Boot`/`$Bitmap`/`$UsnJrnl` decoders (cross-platform,
> Mac-verified). Remaining: P3 (VSS `--volume-path`), P5 (`scripts/capture.rs`
> VSS/zip wrapper), and more P6 decoders (`$Secure`/`$Volume`/`$AttrDef`).
> **Owner:** UFFS core
> **Goal:** Capture *everything* needed to reconstitute a live Windows NTFS volume
> offline "as accurately as possible" — not just the namespace, but ACLs, the
> change journal, free-space map, and transaction log — into one verifiable,
> transferable bundle per drive. Future-proof: capture now, use later.
> **Runbook:** [capture, transfer and verify, step by step](mft-capture-verify-runbook.md).

---

## 1. Intent & scope

Today UFFS captures the MFT (`.iocp`, `.raw`, `.bin`, `.compressed.bin`) plus
derived listings (`cpp_*.txt`, `rust_*.txt`). That is an *Everything-class*
name/size/timestamp snapshot. This feature extends capture to a **complete
NTFS metafile set** so an offline machine can reconstitute the volume's
namespace **and** its security, temporal, and allocation state.

Non-goals: file *contents* (data clusters) and non-resident directory index
buffers — the namespace is rebuilt from each record's `$FILE_NAME` parent ref,
so index buffers and file data are never needed.

## 2. Capture model — two phases, no read-only mode

A VSS shadow copy is **read-only**, and write-protected volumes reject IOCP I/O
(see `reader/dataframe_read.rs`, `reader/index_read.rs`). Therefore the
authentic **live IOCP ingestion flow** and a **frozen consistent snapshot** are
mutually exclusive *sources* and must be captured in two phases. VSS **replaces**
the old "set drives read-only" step entirely.

| Phase | Source | Artifacts | Property |
|---|---|---|---|
| **Live** | live volume `\\.\X:` | `.iocp`, `cpp_x.txt`, `rust_x.txt` | authentic ingestion order (regression/debug) |
| **Frozen** | VSS shadow of `X:` | `$MFT` raw+compressed, `$Secure:$SDS`, `$UsnJrnl:$J`, `$Bitmap`, `$LogFile`, `$Boot`, `$UpCase`, `$AttrDef`, `$Volume`, `$MFTMirr`, `$BadClus`, `$Extend\*` | one crash-consistent instant |

Applies to **every** drive (each: live `.iocp` + its own shadow for the frozen
set). No drive is ever remounted read-only.

### 2.1 Best-effort by host capability

Capture **probes the host first** (`uffs-mft sysinfo`, §5) and does *what the
machine allows*, then **records exactly what it did**. The authoritative
client/server discriminator is the registry value
`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\InstallationType`
(`"Client"` | `"Server"`), corroborated by `GetProductInfo` / `EditionID`.

| Host | Shadow (frozen metafiles, Rust) | C++ golden on shadow | Live `.iocp` | Net |
|---|---|---|---|---|
| **Server** | ✅ Rust reads shadow device path | ✅ `diskshadow expose %s% Z:` → `uffs.com --drives=Z` (skew-free) | ✅ live volume | Full: consistent frozen set **and** skew-free parity |
| **Client** | ✅ Rust reads shadow device path | ❌ no easy raw-volume exposure → keep C++ **live** | ✅ live volume | Best-effort: frozen metafiles from shadow; parity baselines live (skew reconciled) |
| **No VSS / not elevated** | ❌ | ❌ | ✅ (if elevated) | Degraded: live `.iocp` + listings only; frozen set skipped, **and the skip is recorded** |

The manifest records, **per drive**, what was captured vs. skipped and *why*, so
an offline consumer never mistakes a capability gap for missing data.

## 3. Artifact catalog

| FRS | Metafile / stream | File | ~Size (1 TB) | Irreducible because |
|---|---|---|---|---|
| 0 | `$MFT` | `x_mft.iocp` (live) + `x_mft.compressed.bin` (frozen) | 464 M | namespace + tree metrics; `.iocp` also carries ingestion order + `reserved_allocated_bytes` |
| 9 | `$Secure:$SDS` | `x_secure.sds` | few–tens M | ACLs/owner/DACL+SACL; MFT holds only a `SecurityId` index. `$SDH`/`$SII` rebuildable from `$SDS` |
| 11 | `$Extend\$UsnJrnl:$J` (+`$Max`) | `x_usn.jrnl` | 32 M–1 G | change flow / temporal dimension |
| 6 | `$Bitmap` | `x_bitmap.bin` | ~32 M | authoritative free-space (approx. from dataruns only) |
| 2 | `$LogFile` | `x_logfile.bin` | 64–256 M | crash-consistent metadata replay |
| 7 | `$Boot` | `x_boot.bin` | 8 K | geometry + volume serial (identity, offsets) |
| 10 | `$UpCase` | `x_upcase.bin` | 128 K | exact case-folding for offline name collation (`--upcase` exists) |
| 4 | `$AttrDef` | `x_attrdef.bin` | 2.5 K | generic attribute-type interpretation |
| 3 | `$Volume` | `x_volume.bin` | tiny | label, NTFS version, dirty flag |
| 1 | `$MFTMirr` | `x_mftmirr.bin` | 4 K | corruption cross-check |
| 8 | `$BadClus` | `x_badclus.bin` | ~0 (sparse) | bad-cluster map |
| 11 | `$Extend\$ObjId`,`$Quota`,`$Reparse` | `x_extend_*.bin` | KB–MB | link tracking, quotas, reparse index |

Frozen additions total ≈ 0.2–1.5 GB (dominated by `$UsnJrnl` + `$LogFile`).

## 4. VSS orchestration (rust-script wrapper)

`scripts/capture.rs` (rust-script, deps via the `//! ```cargo` block like
`verify_parity.rs`). **No PowerShell.**

1. **Create shadow** via WMI `Win32_ShadowCopy::Create(Volume, "ClientAccessible")`
   using the **`wmi` crate** (encapsulates COM; works on Win client *and* server).
   Note: `vssadmin create` is Server-only; `diskshadow` may be absent on client —
   WMI is the portable path.
2. Read back `DeviceObject` → `\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopyN`.
3. **Frozen phase:** `uffs-mft capture --volume-path <device> --frozen --out <dir>`.
4. **Live phase:** `uffs-mft capture --drive X --live --out <dir>` (`.iocp` +
   optional cpp/rust baselines).
5. **Delete shadow** (WMI `Win32_ShadowCopy.Delete` / `Win32_ShadowCopy` instance).
6. **Package** (§7).

Requires elevation (shadow create + raw volume read). The Access Broker path
still applies for the non-elevated daemon, but capture is an admin operation.

## 5. CLI surface

Home = the crate (tested, lint-covered, broker-aware, reuses datarun/fixup/format
code). New per-metafile `save` targets + one orchestrating `capture` subcommand:

```
# host + drive environment probe — runs FIRST, gates best-effort, records the fact
uffs-mft sysinfo --out capture_host.txt [--json]   # also embedded in manifest.json

# individual targets (compose the frozen set; each writes a self-describing header)
uffs-mft save --secure   --volume-path <dev> -o x_secure.sds
uffs-mft save --usn      --drive X            -o x_usn.jrnl
uffs-mft save --bitmap   --volume-path <dev> -o x_bitmap.bin
uffs-mft save --logfile  --volume-path <dev> -o x_logfile.bin
uffs-mft save --boot | --attrdef | --volume | --mftmirr | --badclus | --upcase

# orchestrator (the entry point the rust-script calls)
uffs-mft capture --drive X --out ~/uffs_data \
    [--all] [--live] [--frozen] [--volume-path <dev>] \
    [--iocp] [--forensic] [--zip] [--split-size 1GiB]
```

`VolumeHandle::open` gains a `--volume-path <device>` mode so the frozen phase
reads the shadow device directly (offset math from the shadow's own `$Boot`).

## 6. On-disk layout + manifest

```
~/uffs_data/drive_x/
  x_mft.iocp                 x_secure.sds        x_boot.bin
  x_mft.compressed.bin       x_usn.jrnl          x_upcase.bin
  x_bitmap.bin               x_logfile.bin       x_attrdef.bin  …
  cpp_x.txt  rust_x.txt      *.log
  capture_host.txt           manifest.json         SHA256SUMS
```

`capture_host.txt` — human-readable descriptor of the machine the capture ran on:

```text
UFFS Capture Host Report
  Captured:      2026-07-05T18:22:04Z (America/Los_Angeles, UTC-07:00)
  Tool:          uffs-mft 0.5.x
  Host:          MACHINE-NAME  (workgroup/domain)
  OS:            Windows 11 Pro 24H2  build 10.0.26100  x64
  InstallType:   Client            <-- gates VSS/C++-on-shadow strategy
  Elevated:      yes
  VSS service:   running (shadow create: permitted)
  CPU / RAM:     24 cores / 64 GB
  Capture mode:  best-effort(client) = live .iocp + shadow(frozen, Rust) ; C++ golden live

  Drives (NTFS):
    C:  NVMe   931 GB (312 GB free)  serial 0x….  MFT 4547 MB / 5.03M recs / 28 extents
        iocp:✅  frozen(shadow):✅  cpp/rust:live  usn:✅ secure:✅ bitmap:✅ logfile:✅
    D:  SSD    465 GB (…)            …
        iocp:✅  frozen(shadow):✅  …
    S:  HDD   11.0 TB (…)            …
        iocp:✅  frozen(shadow):✅  …
```

`manifest.json` (self-describing, drives offline `load` + verification):

```json
{
  "schema": 1,
  "drive": "C",
  "captured_at": "2026-07-05T18:22:04Z",
  "tool_version": "uffs-mft 0.5.x",
  "host": {
    "machine": "MACHINE-NAME", "os": "Windows 11 Pro 24H2",
    "build": "10.0.26100", "arch": "x64",
    "installation_type": "Client", "edition_id": "Professional",
    "elevated": true, "vss": { "service": "running", "create_permitted": true },
    "cpu_cores": 24, "ram_bytes": 68719476736,
    "timezone": "America/Los_Angeles", "utc_offset_minutes": -420,
    "capture_mode": "best-effort(client)"
  },
  "volume": { "serial": "0x….", "ntfs_version": "3.1", "media_type": "NVMe",
              "bytes_per_cluster": 4096, "mft_record_size": 1024 },
  "vss": { "used": true, "device": "\\\\?\\GLOBALROOT\\Device\\…ShadowCopy7",
           "created_at": "…", "snapshot_id": "{GUID}" },
  "phases": { "live": ["x_mft.iocp","cpp_x.txt","rust_x.txt"],
              "frozen": ["x_mft.compressed.bin","x_secure.sds","x_usn.jrnl","…"] },
  "artifacts": [ { "file":"x_mft.iocp","frs":0,"stream":"$MFT",
                   "bytes":486539264,"sha256":"…","compression":"zstd" }, … ]
}
```

Each binary artifact keeps a small typed header (magic / version / volume-serial /
timestamp / drive), consistent with the existing `.iocp` and `$UpCase` headers.

## 7. Packaging: `--zip` + 1 GiB split (best practice)

- **No double-compression:** ZIP entry method = `store` for already-compressed
  (`.iocp`, `.compressed.bin`), `deflate` for text baselines + `$Bitmap` +
  `$LogFile`.
- **Single archive, then raw-split** into fixed parts — *not* spanned ZIP
  (Explorer can't extract `.z01`). Output: `drive_x.zip.001 … NNN`.
- **`--split-size` default 1 GiB.** Reassemble: `copy /b drive_x.zip.* drive_x.zip`
  (Win) or `cat drive_x.zip.* > drive_x.zip` (Mac/Linux).
- **`SHA256SUMS`** covers the whole zip **and** every part → verify each chunk
  post-transfer, then re-verify after reassembly.
- Implemented in the rust-script via the `zip` + `sha2` crates.

## 8. Offline symmetry

Every capture target gains a matching `uffs-mft load` path (as `save`↔`load`,
`save_iocp`↔`load_iocp_capture` already do), so the offline machine reconstitutes
from the same crate that created the artifacts. `verify_parity.rs --regenerate`
continues to regenerate the Rust listing from `x_mft.iocp`/`.compressed.bin` and
compare against the `cpp_x.txt` golden. `$Secure`/`$UsnJrnl`/`$Bitmap`/`$LogFile`
loaders are additive (namespace path unchanged).

## 9. Crate placement

- Metafile readers → `uffs-mft::ntfs::metafiles` (new module) reusing
  `VolumeHandle`, datarun resolution, and fixup code. No new `unsafe`.
- Capture file formats → `uffs-mft::raw` (alongside `raw_iocp.rs`).
- `capture` subcommand → `uffs-mft::commands::windows::capture`.
- Orchestration + VSS + packaging → `scripts/capture.rs` (rust-script).

## 10. Testing

- Fixture/golden round-trip per artifact (`save` → `load` → structural equal).
- Header (de)serialization unit tests (as `raw_iocp` has).
- Manifest schema + SHA256SUMS verification test.
- Split/reassemble round-trip test (byte-identical).
- Live-only paths `#[ignore]` (elevated Windows), fixtures elsewhere.

## 11. Phasing

0. ✅ **P0 — `uffs-mft sysinfo`** (host probe): OS + `InstallationType`
   (client/server), elevation, VSS availability, per-drive media type
   (HDD/SSD/NVMe) + geometry, and every mounted volume (type/format/size/used%).
   Emits `capture_host.txt` (+ `--json`); cross-platform (non-Windows = real host
   facts, no capture). *(commit 56b09cec0)*
1. ✅ **P1 — metafile `save` targets** (`$Boot`, `$Bitmap`, `$Secure:$SDS`,
   `$AttrDef`, `$MFTMirr`, `$Volume`, `$BadClus`; `$UpCase` pre-existing) on a
   generic data-run stream reader. `uffs-mft metafile --kind <k>`.
2. ✅ **P2 — `$LogFile`** (FRS 2) **+ `$UsnJrnl:$J` via `$Extend` traversal**
   (`$INDEX_ROOT` + `$INDEX_ALLOCATION` INDX-block parse to resolve the FRS).
4. ✅ **P4 — `capture` orchestrator**: `uffs-mft capture --drive C --out <dir>`
   writes the compressed `$MFT` + all metafiles + `manifest.json` (per-artifact
   SHA-256) + `SHA256SUMS` into `drive_<x>/`. `MetafileHeader` +
   `load_metafile_from_file` provide the offline read side (P6 partial).
3. ⏳ **P3 — `--volume-path` (shadow device) support in `VolumeHandle`** (read a
   VSS snapshot device directly) — not yet.
5. ⏳ **P5 — `scripts/capture.rs`: WMI VSS create/delete + `--zip`/split + hashes**
   — not yet.
6. ◑ **P6 — offline read side** — shipped: `uffs-mft metafile-info <file>` +
   `load_metafile_from_file`, and cross-platform decoders `parse_boot`
   (geometry), `parse_bitmap` (free space), `parse_usn` (change journal), all
   unit-tested + Mac-verified. Remaining: `$Secure`/`$Volume`/`$AttrDef` parsers
   and `verify_parity` wiring.

## 12. Open questions

- `$UsnJrnl`: dump the **raw `$J` stream** (lossless, replayable) vs. parsed USN
  records (smaller, queryable). Proposal: raw `$J` now, parser later.
- Cross-volume **atomic** shadow *set* (VSS COM) vs. per-volume shadows.
  Proposal: per-volume (independent drives don't need cross-vol atomicity).
- `$Secure`: capture `$SDS` only vs. `$SDS`+`$SDH`+`$SII`. Proposal: `$SDS` only
  (indexes rebuildable); revisit if offline rebuild proves costly.
- Subcommand naming: `uffs-mft sysinfo` (host+drive capture-time environment) is
  distinct from the daemon's runtime `uffs_status`/`status` (health/uptime). Keep
  the names separate to avoid confusion. Proposal: `sysinfo`.
