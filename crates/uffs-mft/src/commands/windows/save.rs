// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Raw MFT save command handlers.
//!
//! These commands print human-readable progress to stdout, rebuild a few
//! `abs_path` bindings as the path is canonicalised, index a fixed-size
//! 65 536-entry `$UpCase` table, and intentionally keep `async` signatures so
//! all `cmd_*` handlers share one shape across the CLI dispatch table.  The
//! lint exemptions below capture those CLI-specific patterns; library code
//! never inherits them.
#![expect(
    clippy::print_stdout,
    reason = "intentional user-facing CLI save / upcase output"
)]
#![expect(
    clippy::float_arithmetic,
    clippy::default_numeric_fallback,
    reason = "byte / utilization calculations divide f64 helpers for human-readable percentage display"
)]
#![expect(
    clippy::min_ident_chars,
    clippy::shadow_reuse,
    reason = "short identifiers and sequential rebinding (e.g. `abs_path`) aid readability in CLI driver code"
)]
#![expect(
    clippy::too_many_lines,
    reason = "save commands run a configure -> read -> compute -> write -> print pipeline that is most readable inline"
)]
#![expect(
    clippy::unused_async,
    reason = "CLI dispatch (`commands/mod.rs`) awaits every `cmd_*` handler for uniform routing; the handler is sync but its signature must stay async"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "indexes into the fixed 65 536-entry `$UpCase` table; bounds are 16-bit constants in the spec"
)]
#![expect(
    clippy::items_after_statements,
    reason = "local `mod` items keep typed Win32 IOCTL structs adjacent to their sole call site for readability"
)]
#![expect(
    clippy::fn_params_excessive_bools,
    reason = "save commands take `--no-bitmap`, `--no-placeholders`, `--full`, `--json` flag bools mirroring CLI args; bundling into a struct duplicates the clap-derive layout"
)]

use std::path::Path;

use anyhow::{Context as _, Result};
use tracing::info;
use uffs_mft::{u64_to_f64, usize_to_u64};

use crate::display::{clean_path_for_display, format_bytes, format_duration, format_number_commas};

// ============================================================================
// Save/Load Raw MFT Commands
// ============================================================================

/// Save MFT bytes (or `$UpCase` table) to a file for offline analysis.
#[cfg(windows)]
#[expect(clippy::too_many_arguments, reason = "CLI command with many options")]
pub(crate) async fn cmd_save(
    drive: uffs_mft::platform::DriveLetter,
    output: &Path,
    compress: bool,
    compression_level: i32,
    raw_compat: bool,
    iocp_mode: bool,
    iocp_concurrency: usize,
    upcase: bool,
) -> Result<()> {
    use std::time::Instant;

    use uffs_mft::platform::{VolumeHandle, detect_drive_type};

    // $UpCase mode: save only the 128 KB uppercase mapping table.
    // Always saves as plain binary (64-byte header + raw table) — no
    // encryption.
    if upcase {
        return cmd_save_upcase(drive, output).await;
    }

    // IOCP mode uses a different code path
    if iocp_mode {
        return cmd_save_iocp(drive, output, compress, compression_level, iocp_concurrency).await;
    }

    use uffs_mft::{MftReader, SaveRawOptions};

    let start_time = Instant::now();

    info!(drive = %drive, "Reading raw MFT from drive");

    // Get volume info for display
    let handle = VolumeHandle::open(drive).with_context(|| format!("Failed to open {drive}:"))?;
    let vol_data = handle.volume_data();

    let drive_type = detect_drive_type(drive);
    let drive_type_str = drive_type.label();

    // Calculate metrics
    let record_count =
        vol_data.mft_valid_data_length / u64::from(vol_data.bytes_per_file_record_segment);

    // Fragmentation analysis
    let (extent_count, is_fragmented) =
        handle
            .get_mft_extents()
            .map_or((1_usize, false), |extents| {
                let count = extents.len();
                (count, count > 1)
            });

    // Bitmap analysis
    let (in_use_records, utilization) =
        handle.get_mft_bitmap().map_or((0_u64, 0.0_f64), |bitmap| {
            let in_use = usize_to_u64(bitmap.count_in_use());
            let pct = (u64_to_f64(in_use) / u64_to_f64(record_count)) * 100.0;
            (in_use, pct)
        });
    let free_records = record_count.saturating_sub(in_use_records);

    // Open reader and save
    let reader =
        MftReader::open(drive).with_context(|| format!("Failed to open drive {drive}:"))?;

    // Raw compat mode implies no compression
    let options = SaveRawOptions {
        compress: if raw_compat { false } else { compress },
        compression_level,
        volume_letter: drive,
        raw_compat,
        // `save` doesn't compute the root reserved-cluster adjustment;
        // `capture` is the path that bakes it into the header for offline
        // parity.
        reserved_allocated_bytes: 0,
    };

    let header = reader
        .save_raw_to_file(output, &options)
        .with_context(|| format!("Failed to save raw MFT to {}", output.display()))?;

    let elapsed = start_time.elapsed();

    // Get absolute path for display
    let abs_path = std::fs::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());
    let abs_path = clean_path_for_display(&abs_path);

    // Print formatted output
    println!("═══════════════════════════════════════════════════════════════");
    println!("                         MFT SAVED");
    println!("                    Drive: {drive}: ({drive_type_str})");
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("📁 MFT STRUCTURE");
    println!(
        "  Total records:        {}",
        format_number_commas(record_count)
    );
    println!(
        "  In-use records:       {}",
        format_number_commas(in_use_records)
    );
    println!(
        "  Free records:         {}",
        format_number_commas(free_records)
    );
    println!("  Utilization:          {utilization:.1}%");
    println!(
        "  Fragmentation:        {} extent(s) {}",
        extent_count,
        if is_fragmented { "⚠️" } else { "✅" }
    );
    println!();
    println!("💾 OUTPUT FILE");
    println!("  Path:                 {}", abs_path.display());
    println!(
        "  Original size:       {}",
        format_bytes(header.original_size)
    );
    if raw_compat {
        println!("  Format:               raw (compatible with other MFT tools)");
    } else if header.is_compressed() {
        println!(
            "  Compressed size:     {}",
            format_bytes(header.compressed_size)
        );
        #[expect(
            clippy::float_arithmetic,
            reason = "floating-point needed for compression ratio calculation"
        )]
        let ratio =
            u64_to_f64(header.compressed_size) / u64_to_f64(header.original_size) * 100.0_f64;
        println!("  Compression ratio:    {ratio:.1}%");
        #[expect(
            clippy::float_arithmetic,
            reason = "floating-point needed for savings calculation"
        )]
        let savings = 100.0_f64 - ratio;
        println!("  Space saved:          {savings:.1}%");
    } else {
        println!("  Compression:          none");
        println!("  Volume letter:        {}:", header.volume_letter);
    }
    println!();
    println!("⏱️  Completed in {}", format_duration(elapsed));

    Ok(())
}

/// Save MFT in IOCP capture mode.
///
/// This mode reads MFT using IOCP and saves chunks in the order they complete,
/// capturing the non-deterministic I/O ordering for realistic testing.
#[cfg(windows)]
async fn cmd_save_iocp(
    drive: uffs_mft::platform::DriveLetter,
    output: &Path,
    compress: bool,
    compression_level: i32,
    concurrency: usize,
) -> Result<()> {
    use std::time::Instant;

    use uffs_mft::platform::{VolumeHandle, detect_drive_type};
    use uffs_mft::{IocpCaptureOptions, MftReader};

    let start_time = Instant::now();

    info!(drive = %drive, concurrency, "Reading MFT with IOCP capture mode");

    // Get volume info for display
    let handle = VolumeHandle::open(drive).with_context(|| format!("Failed to open {drive}:"))?;
    let vol_data = handle.volume_data();

    let drive_type = detect_drive_type(drive);
    let drive_type_str = drive_type.label();

    // Calculate metrics
    let record_count =
        vol_data.mft_valid_data_length / u64::from(vol_data.bytes_per_file_record_segment);

    println!("═══════════════════════════════════════════════════════════════");
    println!("                    MFT IOCP CAPTURE");
    println!("                    Drive: {drive}: ({drive_type_str})");
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("📊 MFT INFO");
    println!(
        "  Total records:        {}",
        format_number_commas(record_count)
    );
    println!("  IOCP concurrency:     {concurrency}");
    println!();
    println!("⏳ Reading with IOCP and capturing chunk order...");

    // Create capture options
    let reserved_alloc = vol_data.reserved_allocated_bytes();
    let options = IocpCaptureOptions {
        compress,
        compression_level,
        volume_letter: drive,
        // IOCP concurrency is bounded by the CLI parser (max 255); the
        // saturating `try_from` fallback is unreachable for valid CLI
        // input and replaces the previous truncating `as u8` cast.
        concurrency: u8::try_from(concurrency).unwrap_or(u8::MAX),
        reserved_allocated_bytes: reserved_alloc,
    };

    // Open reader and save with IOCP capture
    let reader =
        MftReader::open(drive).with_context(|| format!("Failed to open drive {drive}:"))?;

    let header = reader
        .save_iocp_capture(output, &options)
        .with_context(|| format!("Failed to save IOCP capture to {}", output.display()))?;

    let elapsed = start_time.elapsed();

    // Get absolute path for display
    let abs_path = std::fs::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());
    let abs_path = clean_path_for_display(&abs_path);

    println!();
    println!("💾 OUTPUT FILE");
    println!("  Path:                 {}", abs_path.display());
    println!("  Format:               UFFS-IOCP (chunk order preserved)");
    println!(
        "  Chunks captured:      {}",
        format_number_commas(u64::from(header.chunk_count))
    );
    println!(
        "  Total records:        {}",
        format_number_commas(header.total_records)
    );
    println!(
        "  Data size:            {}",
        format_bytes(header.total_data_size)
    );
    if header.is_compressed() {
        println!("  Compression:          zstd (level {compression_level})");
    } else {
        println!("  Compression:          none");
    }
    println!();
    println!("⏱️  Completed in {}", format_duration(elapsed));

    Ok(())
}

// ============================================================================
// $UpCase Table Save
// ============================================================================

/// Save the NTFS `$UpCase` table (128 KB) from a live volume.
///
/// Reads the table via MFT FRS 10 data runs, wraps it with a
/// 64-byte [`UpcaseHeader`], and writes atomically (no encryption —
/// `$UpCase` is public NTFS specification data).
#[cfg(windows)]
#[expect(clippy::print_stdout, reason = "intentional user-facing CLI output")]
async fn cmd_save_upcase(drive: uffs_mft::platform::DriveLetter, output: &Path) -> Result<()> {
    use std::time::Instant;

    use uffs_mft::platform::VolumeHandle;
    use uffs_mft::platform::upcase::{self, UpcaseHeader};

    let start = Instant::now();

    println!("═══════════════════════════════════════════════════════════════");
    println!("  Reading $UpCase table from {drive}: via raw volume I/O");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Read the table from the live volume.
    let table = upcase::read_upcase_table(drive)
        .with_context(|| format!("Failed to read $UpCase from {drive}:"))?;

    // Get volume metadata for the header.
    let handle = VolumeHandle::open(drive)
        .with_context(|| format!("Failed to open {drive}: for metadata"))?;
    let vol = handle.volume_data();

    // Compute CRC-32 of the raw table bytes.
    let raw_bytes: &[u8] = bytemuck::cast_slice(table.as_ref());
    let table_crc32 = upcase::crc32_table(raw_bytes);

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let header = UpcaseHeader {
        ntfs_major: vol.ntfs_major_version,
        ntfs_minor: vol.ntfs_minor_version,
        volume_serial: vol.volume_serial_number,
        table_crc32,
        timestamp,
        drive,
    };

    // Atomic write (header + raw table, no encryption).
    upcase::save_upcase_to_file(output, &header, &table)
        .with_context(|| format!("Failed to save $UpCase to {}", output.display()))?;

    let abs_path = std::fs::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());
    let abs_path = clean_path_for_display(&abs_path);
    let elapsed = start.elapsed();

    println!("💾 $UpCase table saved");
    println!(
        "  Size:   {} ({} entries)",
        format_bytes(usize_to_u64(upcase::UPCASE_SIZE_BYTES)),
        format_number_commas(65_536)
    );
    println!("  NTFS:   {}.{}", header.ntfs_major, header.ntfs_minor);
    println!("  Serial: 0x{:016X}", header.volume_serial);
    println!("  CRC-32: 0x{:08X}", header.table_crc32);
    println!("  Path:   {}", abs_path.display());
    println!("  Time:   {}", format_duration(elapsed));

    // Quick sanity checks.
    println!();
    println!("  Sanity: 'a' → 0x{:04X} (A=0x0041)", table[0x61]);
    println!("          'z' → 0x{:04X} (Z=0x005A)", table[0x7A]);
    println!("          ü   → 0x{:04X} (Ü=0x00DC)", table[0x00FC]);
    println!("          é   → 0x{:04X} (É=0x00C9)", table[0x00E9]);

    Ok(())
}

// ============================================================================
// NTFS Metafile Capture ($Boot, ...)
// ============================================================================

/// Save an NTFS metafile ($Boot, ...) to a file for offline analysis.
#[cfg(windows)]
pub(crate) async fn cmd_metafile(
    drive: uffs_mft::platform::DriveLetter,
    kind: crate::cli::MetafileKind,
    output: &Path,
) -> Result<()> {
    use uffs_mft::platform::VolumeHandle;
    use uffs_mft::platform::metafile::{self, MetafileHeader, MetafileKind};

    let lib_kind = match kind {
        crate::cli::MetafileKind::Boot => MetafileKind::Boot,
        crate::cli::MetafileKind::Bitmap => MetafileKind::Bitmap,
        crate::cli::MetafileKind::Secure => MetafileKind::Secure,
        crate::cli::MetafileKind::AttrDef => MetafileKind::AttrDef,
        crate::cli::MetafileKind::MftMirr => MetafileKind::MftMirr,
        crate::cli::MetafileKind::Volume => MetafileKind::Volume,
        crate::cli::MetafileKind::BadClus => MetafileKind::BadClus,
        crate::cli::MetafileKind::LogFile => MetafileKind::LogFile,
        crate::cli::MetafileKind::UsnJrnl => MetafileKind::UsnJrnl,
    };

    // Volume serial for the header.
    let handle = VolumeHandle::open(drive).with_context(|| format!("Failed to open {drive}:"))?;
    let vol = handle.volume_data();

    let data = metafile::read_metafile(drive, lib_kind)
        .with_context(|| format!("Failed to read {} from {drive}:", lib_kind.name()))?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let header = MetafileHeader {
        kind: lib_kind,
        drive,
        volume_serial: vol.volume_serial_number,
        timestamp,
        data_size: usize_to_u64(data.len()),
    };
    metafile::save_metafile_to_file(output, &header, &data)
        .with_context(|| format!("Failed to save {} to {}", lib_kind.name(), output.display()))?;

    let abs_path = std::fs::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());
    let abs_path = clean_path_for_display(&abs_path);

    println!("💾 {} saved", lib_kind.name());
    println!("  Drive:  {drive}:");
    println!("  Size:   {}", format_bytes(usize_to_u64(data.len())));
    println!("  Serial: 0x{:016X}", vol.volume_serial_number);
    println!("  Path:   {}", abs_path.display());

    Ok(())
}
