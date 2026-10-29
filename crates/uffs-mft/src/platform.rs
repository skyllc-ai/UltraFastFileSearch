// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Platform-specific implementations for Windows.
//!
//! This module provides Windows API wrappers for:
//! - Volume handle management
//! - NTFS volume data retrieval
//! - Privilege checking
//!
//! Some types (`MftExtent`, `MftBitmap`, `DriveType`) are available on all
//! platforms for testing and offline MFT processing.
//!
//! # Safety
//!
//! This module uses Windows FFI and requires careful handling of raw handles.

mod bitmap;
/// Drive-letter newtype — validated Windows drive identifier (`A..=Z`).
///
/// Phase 4 sub-phase 5b: replaces 117 raw `char` parameters across the
/// workspace with a `#[repr(transparent)]` newtype that canonicalises
/// case and rejects non-ASCII-letter input at the parse boundary.
pub mod drive_letter;
/// Drive-prefix splitting for search patterns.
///
/// The single canonical parser shared by the CLI parse layer and the
/// daemon dispatch safety net, so both agree on what a leading `X:`
/// means.
pub mod drive_pattern;
mod extents;
/// Logical Cluster Number newtype — signed cluster identifier used
/// by `FSCTL_GET_RETRIEVAL_POINTERS` and the on-disk data-run decoder.
///
/// Phase 4 sub-phase 5d: replaces raw `i64` LCN fields on `MftExtent`
/// and `DataRun` with a [`Lcn`] newtype that enforces sparse / hole
/// detection through [`Lcn::is_hole`] / [`Lcn::is_zero`] instead of
/// open-coded `< 0` / `== 0` checks at every call site.
pub mod lcn;
/// NTFS metafile capture ($Boot, ...) from a live volume.
pub mod metafile;
/// Offline decoders for captured metafiles ($Boot geometry, $Bitmap free
/// space).
pub mod metafile_decode;
/// Live-volume NTFS metafile readers ($Boot, $DATA streams, $UsnJrnl).
pub mod metafile_read;
mod physical;
/// Native Windows process introspection for the self-update detector.
///
/// Image path + pid enumeration — keeps the `unsafe` FFI out of `uffs-cli`.
#[cfg(windows)]
pub mod process;
mod system;
/// `$UpCase` table reading from live NTFS volume.
pub mod upcase;
#[cfg(windows)]
mod volume;

pub use bitmap::MftBitmap;
pub use drive_letter::{DriveLetter, DriveLetterError};
pub use drive_pattern::split_drive_prefix;
pub use extents::MftExtent;
pub use lcn::Lcn;
// Export DriveType unconditionally (needed for tests), but Windows-specific
// functions only on Windows
pub use physical::{PhysicalDrive, physical_drives};
pub use system::DriveType;
// Caller's effective uid — Unix-only; lets daemon-management compare the
// caller against the running daemon's owner (its PID-file uid).
#[cfg(unix)]
pub use system::current_euid;
// Unix: POSIX W_OK writability probe — lets `uffs --uninstall` flag a
// root-owned binary root before it tries to delete it.
#[cfg(unix)]
pub use system::dir_user_writable;
// Elevation check — available on all platforms (Windows: UAC token check;
// Unix: geteuid() == 0).  Both the daemon CLI gate and uffs-daemon use this.
pub use system::is_elevated;
// is_volume_read_only — Windows-only (non-Windows stub was removed because
// every caller in this crate is #[cfg(windows)]-gated).  Consumed by the
// uffs-mft bin (commands/windows/incremental.rs) via the external-style
// `uffs_mft::platform::is_volume_read_only` path, so must be pub.
#[cfg(windows)]
pub use system::is_volume_read_only;
#[cfg(windows)]
pub(crate) use system::u32_size_of;
// System memory query — available on all platforms
pub use system::{SystemMemory, query_system_memory};
// Windows-specific public API surface — consumed cross-crate (uffs-daemon),
// by the uffs-mft bin (commands/) via `uffs_mft::platform::*` external-style
// paths, and as platform utility helpers (infer_drive_from_path,
// volume_root_path are stable public API restored from the Phase 2.5
// demotion in commit 1529cb162).
#[cfg(windows)]
pub use system::{
    detect_boot_drive, detect_drive_type, detect_ntfs_drives, infer_drive_from_path, is_boot_drive,
    volume_root_path,
};
#[cfg(windows)]
pub(crate) use volume::read_handle_at;
// Cross-platform broker-adoption count so `--status` can report the mode on
// any target: the real registry count on Windows, `0` (no broker) elsewhere.
#[cfg(windows)]
pub use volume::registered_broker_handle_count;
// Crate-internal: the USN journal open (FU-2b) and `$MFT` extent read (FU-3)
// adopt the same broker volume handle the MFT read uses.
#[cfg(windows)]
pub(crate) use volume::try_adopt_broker_handle;
#[cfg(windows)]
pub(crate) use volume::{
    IOCP_WAIT_COMPLETION_DEADLINE, IOCP_WAIT_POLL_INTERVAL_MS, WAIT_TIMEOUT_ERROR_CODE,
    classify_wait_error_code, wait_deadline_exceeded,
};
// Both VolumeHandle and NtfsVolumeData are part of the public API —
// `VolumeHandle::volume_data()` returns `&NtfsVolumeData` so the latter
// must be at least as public as the former.
#[cfg(windows)]
pub use volume::{NtfsVolumeData, VolumeHandle, register_broker_handle};

/// Non-Windows: there is no Access Broker, so no adopted handles.
#[cfg(not(windows))]
#[must_use]
pub const fn registered_broker_handle_count() -> usize {
    0
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn volume_root_path_works() {
        // [`DriveLetter`] canonicalises to uppercase at construction
        // time, so a single mapping covers both former cases.
        assert_eq!(volume_root_path(DriveLetter::C), PathBuf::from("C:\\"));
        assert_eq!(volume_root_path(DriveLetter::D), PathBuf::from("D:\\"));
    }

    #[test]
    fn is_elevated_works() {
        // Just verify it doesn't panic.  Bind with `_unused` so the must_use
        // result is consumed without us losing the annotation; the prefix
        // signals "intentionally unused" without triggering
        // `let_underscore_must_use`.
        let _unused = is_elevated();
    }

    #[test]
    fn nvme_optimal_settings() {
        let drive_type = DriveType::Nvme;

        assert_eq!(drive_type.optimal_concurrency(), 32);
        assert_eq!(drive_type.optimal_io_size(), 4 * 1024 * 1024);
        assert_eq!(drive_type.optimal_chunk_size(), 4 * 1024 * 1024);
        assert_eq!(drive_type.prefetch_buffers(), 8);
        assert!(drive_type.is_high_performance());
        assert!(drive_type.benefits_from_parallel_parsing());
    }

    #[test]
    fn ssd_optimal_settings() {
        let drive_type = DriveType::Ssd;

        assert_eq!(drive_type.optimal_concurrency(), 8);
        assert_eq!(drive_type.optimal_io_size(), 2 * 1024 * 1024);
        assert_eq!(drive_type.optimal_chunk_size(), 2 * 1024 * 1024);
        assert_eq!(drive_type.prefetch_buffers(), 4);
        assert!(drive_type.is_high_performance());
        assert!(!drive_type.benefits_from_parallel_parsing());
    }

    #[test]
    fn hdd_optimal_settings() {
        let drive_type = DriveType::Hdd;

        assert_eq!(drive_type.optimal_concurrency(), 4);
        assert_eq!(drive_type.optimal_io_size(), 1024 * 1024);
        assert_eq!(drive_type.optimal_chunk_size(), 1024 * 1024);
        assert_eq!(drive_type.prefetch_buffers(), 2);
        assert!(!drive_type.is_high_performance());
        assert!(!drive_type.benefits_from_parallel_parsing());
    }

    #[test]
    fn hdd_extent_aware_concurrency() {
        assert_eq!(DriveType::optimal_concurrency_for_hdd(62), 2);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(100), 2);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(51), 2);

        assert_eq!(DriveType::optimal_concurrency_for_hdd(50), 4);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(28), 4);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(21), 4);

        assert_eq!(DriveType::optimal_concurrency_for_hdd(20), 6);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(19), 6);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(17), 6);
        assert_eq!(DriveType::optimal_concurrency_for_hdd(1), 6);
    }

    #[test]
    fn unknown_optimal_settings() {
        let drive_type = DriveType::Unknown;

        assert_eq!(drive_type.optimal_concurrency(), 4);
        assert_eq!(drive_type.optimal_io_size(), 1024 * 1024);
        assert_eq!(drive_type.optimal_chunk_size(), 1024 * 1024);
        assert_eq!(drive_type.prefetch_buffers(), 2);
        assert!(!drive_type.is_high_performance());
        assert!(!drive_type.benefits_from_parallel_parsing());
    }

    #[test]
    fn removable_and_virtual_take_conservative_hdd_profile() {
        // Removable (USB / SD / MMC) and Virtual (VHD / RAM-backed): the bus or
        // the opaque backing medium is the bottleneck, so both mirror the HDD
        // profile — small chunks, few buffers, low concurrency, and never
        // high-performance or parallel-parse-friendly.
        for drive_type in [DriveType::Removable, DriveType::Virtual] {
            assert_eq!(
                drive_type.optimal_concurrency(),
                DriveType::Hdd.optimal_concurrency()
            );
            assert_eq!(
                drive_type.optimal_io_size(),
                DriveType::Hdd.optimal_io_size()
            );
            assert_eq!(drive_type.optimal_chunk_size(), 1024 * 1024);
            assert_eq!(drive_type.prefetch_buffers(), 2);
            assert!(!drive_type.is_high_performance());
            assert!(!drive_type.benefits_from_parallel_parsing());
        }
    }

    #[test]
    fn optimal_settings_are_reasonable() {
        let nvme = DriveType::Nvme;
        let ssd = DriveType::Ssd;
        let hdd = DriveType::Hdd;

        assert!(nvme.optimal_concurrency() > ssd.optimal_concurrency());
        assert!(ssd.optimal_concurrency() > hdd.optimal_concurrency());

        assert!(nvme.optimal_io_size() > ssd.optimal_io_size());
        assert!(ssd.optimal_io_size() >= hdd.optimal_io_size());

        assert!(nvme.prefetch_buffers() > ssd.prefetch_buffers());
        assert!(ssd.prefetch_buffers() >= hdd.prefetch_buffers());
    }
}
