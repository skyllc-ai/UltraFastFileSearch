// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! # uffs-mft: NTFS Master File Table Reading Library
//!
//! This crate provides high-performance direct MFT reading capabilities,
//! outputting data as Polars `DataFrame`s for efficient querying.
//!
//! ## Features
//!
//! - **Direct MFT Access**: Bypasses Windows file enumeration APIs for speed
//! - **Async I/O**: Uses tokio for high-throughput disk reading
//! - **Polars Integration**: Returns `DataFrame`s for powerful data
//!   manipulation
//! - **Parquet Persistence**: Save/load indexes in compressed Parquet format
//!
//! # Concurrency
//!
//! Predominantly a sync library + CLI binary.  Tokio is used only by
//! the daemon-embedded loaders (`commands::load`, `commands::windows`)
//! to schedule per-drive `spawn_blocking` MFT reads in parallel.  All
//! `std::fs::*` calls live in CLI command handlers or sync helpers
//! invoked from sync `fn main` (Phase 10f B3/B4 verdicts).  No
//! per-shard background tasks, no shared mutable state, no channels
//! at this layer — the daemon owns all of that and pulls
//! `DataFrame` snapshots from us.  See
//! `docs/architecture/code-quality/concurrency_policy.md` for the
//! workspace contract.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use uffs_mft::MftReader;
//!
//! fn main() -> Result<(), Box<dyn core::error::Error>> {
//!     // Read MFT from C: drive (requires admin privileges)
//!     let df = MftReader::open(crate::platform::DriveLetter::C)?.read_all()?;
//!
//!     println!("Found {} files", df.height());
//!
//!     // Save for later use
//!     MftReader::save_parquet(&df, "c_drive.parquet")?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ## `DataFrame` Schema
//!
//! The returned `DataFrame` has the following columns:
//!
//! | Column       | Type           | Description                    |
//! |--------------|----------------|--------------------------------|
//! | `frs`        | `UInt64`       | File Record Segment number     |
//! | `parent_frs` | `UInt64`       | Parent directory FRS           |
//! | `name`       | `String`       | File/directory name            |
//! | `size`       | `UInt64`       | File size in bytes             |
//! | `created`    | `Datetime[μs]` | Creation timestamp             |
//! | `modified`   | `Datetime[μs]` | Modification timestamp         |
//! | `accessed`   | `Datetime[μs]` | Access timestamp               |
//! | `flags`      | `UInt16`       | Bit-packed attributes          |
//!
//! ## API hygiene policy (Phase 3b §3.4 / §3.6 / §3.7)
//!
//! The vast majority of `pub struct` declarations in this crate fall
//! into one of three categories, each with a **uniform decision**:
//!
//! 1. **NTFS on-disk zerocopy types** (`#[repr(C, packed)]` +
//!    `#[derive(FromBytes, Immutable, KnownLayout)]`): `MultiSectorHeader`,
//!    `AttributeRecordHeader`, `ResidentAttributeData`,
//!    `NonResidentAttributeData`, `FileRecordSegmentHeader`, `IndexHeader`,
//!    `IndexRoot`, `StandardInformation`, `FileNameAttribute`,
//!    `ExtendedStandardInfo`, `AttributeListEntry`, `ReparsePointHeader`,
//!    `ReparseMountPointBuffer`, `NtfsBootSector`, IOCP capture headers, etc.
//!    The field layout **is** the NTFS specification (or the IOCP capture file
//!    contract); `pub` fields are non-negotiable.  `#[non_exhaustive]` would
//!    forbid the very `MyHeader { … }` literals that the zerocopy decoder
//!    helpers build by hand.  **Kept exhaustive.**
//!
//! 2. **Index / record DTOs** (`MftIndex`, `FileRecord`, `ChildInfo`,
//!    `LinkInfo`, `SizeInfo`, `StandardInfo`, `IndexStreamInfo`,
//!    `IndexNameRef`, `UsnApplyStats`, `IndexBuildTiming`, `ParseResult`,
//!    `ParsedColumns`, `ParsedRecord`, `ReadChunk`, `RawMftHeader`,
//!    `RawMftData`, etc.):  read-mostly value types consumed by `uffs-core` and
//!    `uffs-daemon`.  Hundreds of struct-literal construction sites;
//!    `#[non_exhaustive]` would require migrating each to a builder for
//!    marginal benefit while the crate is Polars-blocked from publishing.
//!    **Kept exhaustive.**
//!
//! 3. **Configuration option structs** (`LoadRawOptions`, `SaveRawOptions`,
//!    `IocpCaptureOptions`):  `pub` fields are config knobs with
//!    `Default::default()`; struct-literal construction is the natural API.
//!    **Kept exhaustive** for the same migration-cost reason; revisit when the
//!    crate publishes.
//!
//! The few `pub enum` declarations (`AttributeType`, `ReparseTag`,
//! `DriveType`, `FileFlags`, etc.) are **closed type-code enums**
//! defined by the NTFS specification — new variants only appear when
//! Microsoft extends NTFS — and **state-machine / dispatch enums**
//! that consumers exhaustively match.  Either category falls under
//! the playbook §3.6 "keep exhaustive" rule.  **Kept exhaustive.**
//!
//! No `pub trait` declarations live in this crate, so the
//! sealed-trait decision (§3.7) is **N/A**.
//!
//! # Environment
//!
//! Env vars read by this crate (registry:
//! `docs/architecture/code-quality/build_codegen_policy.md` §5, playbook
//! §1049-1056).  Several are dev / parity-debug knobs read via `env::var_os(…).
//! is_some()` (any set value enables; absence disables).
//!
//! | Env var | Type | Default | Notes |
//! |---|---|---|---|
//! | `CARGO_MANIFEST_DIR` | `path` | (set by Cargo) | Test-fixture path resolution.  CARGO semver class. |
//! | `CARGO_PKG_VERSION` | `string` | (set by Cargo) | Read via `env!()` for log preludes.  CARGO semver class. |
//! | `RUST_LOG` | `string` | `info` | `tracing-subscriber` filter directive used by the standalone `uffs_mft` binary.  STANDARD semver class (tracing convention). |
//! | `RUST_LOG_FILE` | `path` | (none) | Optional log-file path override for the standalone `uffs_mft` binary.  INTERNAL semver class. |
//! | `UFFS_LOG_DIR` | `path` | platform default | Log directory override for the standalone `uffs_mft` binary.  INTERNAL semver class. |
//! | `UFFS_CACHE_PROFILE` | `bool` (`env::var_os(…).is_some()`) | `false` (unset) | Emits per-phase cache I/O timings to stderr (`[CACHE_PROFILE]` prefix) from `cache`, `index/storage/{deserialize,file_io}`, and `reader/persistence`.  Dev / benchmark only.  INTERNAL semver class. |
//! | `UFFS_NO_JOURNAL_MAX_AGE_SECS` | `u64` (seconds) | `300` | Max age a cached index is served when the drive has **no active USN journal** (os error 1179); older caches trigger a full MFT rebuild instead of serving stale, in `reader::usn_apply`.  INTERNAL semver class. |
//! | `UFFS_MFT_TEST_DIR` | `path` | (none) | Optional test-fixture directory for the parallel-reader chaos-order harness.  Test-only.  INTERNAL semver class. |
//! | `UFFS_MFT_TEST_FILE` | `path` | (none) | Optional test-fixture file path for the parallel-reader chaos-order harness.  Test-only.  INTERNAL semver class. |
//! | `UFFS_PARITY_DEBUG` | `bool` | `false` | Enables verbose chaos-order parity debugging in the LIVE parser (`io::readers::parallel::to_index`).  INTERNAL semver class (dev only). |
//! | `UFFS_REBUILD_CHILDREN_ALWAYS` | `bool` (`env::var_os(…).is_some()`) | `false` (unset) | Forces unconditional children-rebuild from name graph in `index::tree::compute_tree_metrics_impl`; removes parse-order artifacts for validation runs.  INTERNAL semver class (dev only). |
//! | `UFFS_SINGLE_THREAD` | `bool` | `false` | Forces single-threaded reader in `reader::persistence` for parity debugging.  INTERNAL semver class (dev only). |
//! | `UFFS_SKIP_ORPHANS` | `bool` (`env::var_os(…).is_some()`) | `false` (unset) | Skips orphan-record sweep in `index::tree::compute_tree_metrics_impl` (only paths reachable from ROOT through visible FILE_NAME edges are aggregated).  INTERNAL semver class (dev only). |

// On docs.rs only: enable the `doc_cfg` rustdoc feature so cfg-gated items
// (`#[cfg(windows)]`, `#[cfg(feature = "...")]`, etc.) render with their
// cfg badge.  Gated behind `cfg(docsrs)` so local `cargo doc` never
// exercises the nightly-only feature.  Post-Rust-1.92 the `doc_auto_cfg`
// feature was merged into `doc_cfg` (rust-lang/rust#138907).
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(clippy::all, clippy::pedantic)]
#![expect(
    clippy::module_name_repetitions,
    reason = "re-exports use crate-prefixed names for clarity"
)]
// Windows-only because every `std::io::Error::new` site lives behind
// `#[cfg(windows)]` (IO-reader, bitmap, volume paths).  On non-Windows builds
// the lint never fires, and an unconditional `#[expect]` would itself trip
// `unfulfilled_lint_expectations` under `-D warnings`.
#![cfg_attr(
    windows,
    expect(
        clippy::std_instead_of_core,
        reason = "core::io::Error is not yet stable (feature `core_io`, tracking issue rust-lang/rust#154046). Our IO-reader, bitmap, and volume paths construct std::io::Error::new(ErrorKind::_, msg); swapping to core::io::ErrorKind alone would force a split std/core import at every site. Revisit once core::io::Error stabilizes."
    )
)]

extern crate alloc;

// Dev-dependencies used in tests but not by the library itself.
// Binary dependencies (used by src/main.rs only, or by #[cfg(windows)]
// modules). Listed here to prevent unused_crate_dependencies false positives on
// non-Windows.
// Linked for this crate's binary target(s), which call
// `uffs_version::handle_version!` in `main`; the library itself does not
// reference it (sanctioned unused-crate-dependencies marker).
use anyhow as _;
// ============================================================================
// Suppress unused crate warnings
// ============================================================================
// These dependencies are used by the uffs-mft binary (src/main.rs), not the
// library. Cargo doesn't support per-binary dependencies, so we suppress the
// warnings here. The binary uses these for CLI, logging, and async runtime.
// Platform-specific dependencies (used on Windows only)
#[cfg(not(windows))]
use bitflags as _;
use chrono as _;
use clap as _;
// Dev-dependencies (used in benchmarks and tests only)
#[cfg(test)]
use criterion as _;
use dirs_next as _;
#[cfg(test)]
use hex as _;
#[cfg(windows)]
use hex as _;
use hostname as _;
use indicatif as _;
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use rand as _;
#[cfg(test)]
use rand_chacha as _;
// Pipelining dependencies (used in io.rs PipelinedMftReader on Windows)
#[cfg(not(windows))]
use rayon as _;
// FxHash for fast hashing (used in io.rs on Windows)
#[cfg(not(windows))]
use rustc_hash as _;
// `serde_json` powers `--format json` for the Windows-only `info` / `drives`
// commands (src/commands/windows/info.rs); silence the library's view of it.
#[cfg(windows)]
use serde_json as _;
// `sha2` + `hex` power the Windows-only `capture` command's manifest /
// SHA256SUMS (src/commands/windows/capture.rs) and the chaos tests; silence the
// library's view.
#[cfg(any(test, windows))]
use sha2 as _;
use smallvec as _;
#[cfg(test)]
use tempfile as _;
#[cfg(not(windows))]
use thiserror as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;
#[cfg(not(windows))]
use uffs_polars as _;
use uffs_text as _;
use uffs_version as _;
#[cfg(windows)]
use windows as _;

// ============================================================================
// Module declarations
// ============================================================================

pub mod discovery;
// Phase 3: error, flags, ntfs have zero external module-path use;
// downstream callers go through the flat `uffs_mft::{MftError, Result,
// FileFlags, SECTOR_SIZE, AttributeIterator, ...}` re-exports below.
pub(crate) mod error;
pub(crate) mod flags;
pub mod index;
pub mod raw;
pub mod raw_iocp;
pub(crate) mod tree_metrics;

// Cross-platform modules (NTFS structures and parsing)
pub(crate) mod ntfs; // NTFS structure definitions - cross-platform
pub mod parse; // MFT record parsing - cross-platform

// I/O operations module
// Available on all platforms for offline MFT processing (chaos mode, testing)
// Live MFT access (via HANDLE) is still Windows-only and gated per-function
pub mod io;

// Platform module needed by io module
// Available on all platforms (with Windows-specific HANDLE types cfg-gated
// internally)
pub mod platform;

pub mod usn;

// Read-order optimization for bulk content-read jobs: resolves FRS to
// on-disk physical location (LCN). The byte-parsing core is pure and
// cross-platform (testable without Windows); only its `VolumeHandle`-
// driven public entry point is cfg-gated internally, matching that
// type's own gating below.
pub mod lcn_resolve;

pub mod frs;

pub mod cache;

/// Minimal `ustar` archive writer + byte splitter for capture bundles.
pub mod archive;

/// Canonical CSV parity comparison for the capture verification flow.
pub mod parity;

mod reader;

// WI-7.1 — pathological-name parity corpus (Tier 1 decoder pins + Tier 2
// offline-capture-vs-golden). Crate-internal so it can reach the `pub(crate)`
// instrumented decoder; test-only.
#[cfg(test)]
#[path = "parity_tests.rs"]
mod parity_tests;

// ============================================================================
// Public API re-exports
// ============================================================================

// Re-export cache types.  `load_or_build_dataframe_cached` is Windows-only
// (depends on the `uffs_polars::DataFrame` build) and gated below.
#[cfg(windows)]
pub use cache::load_or_build_dataframe_cached;
pub use cache::{
    CacheStatus, FileLock, INDEX_TTL_SECONDS, LockKind, MultiDriveCacheStatus, atomic_write,
    cache_age_seconds, cache_dir, cache_file_path, cache_lock_path, check_cache_status,
    check_multi_drive_cache, cleanup_expired_cache, compress_encrypt_write, compress_zstd_mt,
    create_secure_dir, is_cache_fresh, load_cached_index, migrate_legacy_cache,
    remove_all_cached_indices, remove_cached_index, save_to_cache, save_to_cache_background,
    secure_cache_dir, secure_remove, set_file_permissions_owner_only, with_file_lock,
};
pub use error::{MftError, Result};
pub use flags::FileFlags;
// Re-export FRS newtypes — typed alternatives to raw `u64` FRS values.
pub use frs::{Frs, ParentFrs};
// Re-export lean index types
pub use index::{
    ChildInfo, FileRecord, IndexBuildTiming, IndexNameRef, IndexStreamInfo, LinkInfo, MftIndex,
    NO_ENTRY, ROOT_FRS, SizeInfo, StandardInfo, UsnApplyStats, bytes_to_mb_f64, f64_to_u64,
    f64_to_usize, frs_to_usize, len_to_u16, len_to_u32, micros_to_i64, millis_to_u64, nanos_to_u64,
    nonneg_to_u64, u32_as_usize, u32_to_f64, u64_to_f64, usize_to_f64, usize_to_u64,
};
// Re-export I/O types for advanced usage.  Public-API anchors that have
// no current external consumers but are part of the documented surface are
// kept pub.  The Windows-only reader structs are pub(crate) at their
// definition site and have no `crate::<reader>` consumers (only
// `crate::io::readers::<reader>` paths), so no crate-root re-export is
// needed for them.
#[cfg(windows)]
pub use io::{
    AlignedBuffer, ExtensionAttributes, MftExtentMap, MftRecordMerger, ParseResult, ParsedColumns,
    ParsedRecord, ReadChunk, StdInfoParse, apply_fixup, generate_read_chunks, parse_record_full,
    parse_record_zero_alloc,
};
pub use lcn_resolve::{RecordOutcome, first_data_lcn};
#[cfg(windows)]
pub use lcn_resolve::{for_each_record, resolve_frs_to_lcn};
// Re-export NTFS constants and types (pure Rust data structures,
// cross-platform)
pub use ntfs::SECTOR_SIZE;
pub use ntfs::{
    AttributeIterator, AttributeListEntry, AttributeRecordHeader, AttributeRef, AttributeType,
    DataRun, DataRunIter, ExtendedStandardInfo, FileNameAttribute, FileRecordSegmentHeader,
    IndexHeader, IndexRoot, MultiSectorHeader, NameInfo, NonResidentAttributeData, NtfsBootSector,
    ReparseMountPointBuffer, ReparsePointHeader, ReparseTag, ResidentAttributeData,
    StandardInformation, StreamInfo, apply_usa_fixup, data_runs_iter_from_attribute,
    extract_data_runs_from_attribute, fixup_file_record, parse_data_runs, parse_data_runs_iter,
};
// Caller's effective uid (Unix-only) — daemon-management uses it to decide
// whether managing the *running* daemon needs elevation (owner comparison).
#[cfg(unix)]
pub use platform::current_euid;
// Elevation check — cross-platform public API (Windows: UAC token check;
// Unix: geteuid() == 0).  Exported unconditionally so uffs-cli and
// uffs-daemon can gate mutating daemon commands on all targets.
pub use platform::is_elevated;
// Re-export platform types
// Core types (DriveType, MftBitmap, MftExtent) are pure data — available on all
// platforms Windows-specific types and functions (VolumeHandle,
// detect_ntfs_drives, etc.) only on Windows
pub use platform::{DriveType, MftBitmap, MftExtent, SystemMemory, query_system_memory};
pub use platform::{Lcn, registered_broker_handle_count};
// External-API anchors with cross-crate consumers.  Other Windows-only
// platform items (NtfsVolumeData, detect_drive_type, infer_drive_from_path,
// is_volume_read_only) are pub(crate) and consumed only via
// `crate::platform::*` paths, so no crate-root re-export is needed.
#[cfg(windows)]
pub use platform::{VolumeHandle, detect_ntfs_drives, register_broker_handle};
pub use raw::{
    LoadRawOptions, RawMftData, RawMftHeader, SaveRawOptions, load_raw_mft, load_raw_mft_header,
    save_raw_mft,
};
pub use raw_iocp::{
    CapturedChunk, IocpCaptureData, IocpCaptureHeader, IocpCaptureOptions, IocpCaptureWriter,
    is_iocp_capture, load_iocp_capture, load_iocp_to_index,
};
pub use reader::{
    BenchmarkResult, DriveCharacteristics, DriveReadResult, MftProgress, MftReadMode, MftReader,
    MftStats, MultiDriveMftReader, PhaseTimings,
};
// Re-export USN Journal types
pub use usn::{
    ChangeType, FileChange, UsnJournalInfo, UsnRecord, aggregate_changes, query_usn_journal,
    read_usn_journal, reason,
};
