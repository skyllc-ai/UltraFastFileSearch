// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Compression, encryption, and atomic-write pipeline for MFT cache files.
//!
//! Extracted from `cache.rs` for file-size policy compliance.

use std::path::Path;

use super::{CountingWriter, append_size_trailer, atomic_write};
use crate::index::usize_to_f64;

/// Compresses `data` with zstd using multi-threaded mode.
///
/// Falls back to single-threaded if `multithread()` fails (e.g. on very
/// old builds without `zstdmt`).
///
/// # Errors
///
/// Returns an I/O error if compression fails.
pub fn compress_zstd_mt(data: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    let mut encoder = new_zstd_mt_encoder(level)?;
    std::io::Write::write_all(&mut encoder, data)?;
    encoder.finish()
}

/// Creates a multi-threaded zstd `Encoder` that compresses into a `Vec<u8>`.
///
/// Call `.finish()` on the returned encoder after writing all data to
/// obtain the compressed bytes.
///
/// # Errors
///
/// Returns an I/O error if encoder initialisation fails.
pub fn new_zstd_mt_encoder(level: i32) -> std::io::Result<zstd::Encoder<'static, Vec<u8>>> {
    new_zstd_encoder_with_workers(level, default_zstd_workers())
}

/// The zstd worker count for foreground cache writes: one per core, at most 8.
#[must_use]
pub fn default_zstd_workers() -> u32 {
    let workers_usize = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    u32::try_from(workers_usize).unwrap_or(8)
}

/// [`new_zstd_mt_encoder`] with an explicit worker count.
///
/// A background writer passes a small count so a cache save does not take
/// every core away from the user's own work.
///
/// # Errors
///
/// Returns an I/O error if encoder initialisation fails.
pub fn new_zstd_encoder_with_workers(
    level: i32,
    workers: u32,
) -> std::io::Result<zstd::Encoder<'static, Vec<u8>>> {
    let mut encoder = zstd::Encoder::new(Vec::new(), level)?;
    let _mt_result: Result<(), std::io::Error> = encoder.multithread(workers);
    Ok(encoder)
}

/// Streaming variant of [`compress_encrypt_write`]: the caller writes
/// serialized data directly into a zstd encoder via `write_fn`, avoiding
/// a contiguous intermediate buffer.
///
/// For a 7M-record drive, peak memory drops from ~1.3 GB to ~400 MB.
///
/// # Errors
///
/// Returns an I/O error if compression, encryption, or file writing fails.
pub fn compress_encrypt_write_streaming<F>(
    write_fn: F,
    path: &Path,
    zstd_level: i32,
    profile: bool,
    label: &str,
) -> std::io::Result<()>
where
    F: FnOnce(&mut CountingWriter<zstd::Encoder<'static, Vec<u8>>>) -> std::io::Result<()>,
{
    compress_encrypt_write_streaming_with_workers(
        write_fn,
        path,
        zstd_level,
        default_zstd_workers(),
        profile,
        label,
    )
}

/// [`compress_encrypt_write_streaming`] with an explicit zstd worker count.
///
/// # Errors
///
/// Returns an I/O error if compression, encryption, or file writing fails.
pub fn compress_encrypt_write_streaming_with_workers<F>(
    write_fn: F,
    path: &Path,
    zstd_level: i32,
    zstd_workers: u32,
    profile: bool,
    label: &str,
) -> std::io::Result<()>
where
    F: FnOnce(&mut CountingWriter<zstd::Encoder<'static, Vec<u8>>>) -> std::io::Result<()>,
{
    let t_total = std::time::Instant::now();

    let t_compress = std::time::Instant::now();
    let mut writer = CountingWriter::new(new_zstd_encoder_with_workers(zstd_level, zstd_workers)?);
    write_fn(&mut writer)?;
    let decompressed_len = writer.written();
    let mut compressed = writer.into_inner().finish()?;
    // The streaming encoder does not record the decompressed size; the
    // trailer does, so the loader can allocate it exactly.
    append_size_trailer(&mut compressed, decompressed_len);
    let compress_ms = t_compress.elapsed().as_millis();
    let compressed_len = compressed.len();

    let key = uffs_security::keystore::get_cache_key()
        .map_err(|err| std::io::Error::other(format!("key unavailable: {err}")))?;
    let t_encrypt = std::time::Instant::now();
    let encrypted = uffs_security::crypto::encrypt_cache(&compressed, &key)?;
    let encrypt_ms = t_encrypt.elapsed().as_millis();
    drop(compressed);

    let t_write = std::time::Instant::now();
    atomic_write(path, &encrypted)?;
    let write_ms = t_write.elapsed().as_millis();

    if profile {
        #[expect(
            clippy::float_arithmetic,
            reason = "display-only MB conversion for profiling"
        )]
        let mb = |bytes: usize| usize_to_f64(bytes) / (1_024.0_f64 * 1_024.0_f64);
        let total_ms = t_total.elapsed().as_millis();
        tracing::debug!(
            target: "cache_profile",
            label,
            compress_ms = %compress_ms,
            comp_mb = %format_args!("{:.1}", mb(compressed_len)),
            encrypt_ms = %encrypt_ms,
            write_ms = %write_ms,
            write_mb = %format_args!("{:.1}", mb(encrypted.len())),
            total_ms = %total_ms,
            "bg_streaming_compress_encrypt_write"
        );
    }

    Ok(())
}

/// Compresses, encrypts, and atomically writes serialized cache bytes to disk.
///
/// Designed to be called from a background thread — all work is self-contained.
/// Profile output is emitted to stderr if `profile` is true.
///
/// # Errors
///
/// Returns an I/O error if any step fails. Since this is typically called
/// from a background thread, callers should log but not propagate errors.
pub fn compress_encrypt_write(
    serialized: Vec<u8>,
    path: &Path,
    zstd_level: i32,
    profile: bool,
    label: &str,
) -> std::io::Result<()> {
    let t_total = std::time::Instant::now();

    let uncompressed_len = serialized.len();
    let t_compress = std::time::Instant::now();
    let mut compressed = compress_zstd_mt(&serialized, zstd_level)?;
    append_size_trailer(&mut compressed, uncompressed_len as u64);
    let compress_ms = t_compress.elapsed().as_millis();
    let compressed_len = compressed.len();
    drop(serialized);

    let key = uffs_security::keystore::get_cache_key()
        .map_err(|err| std::io::Error::other(format!("key unavailable: {err}")))?;
    let t_encrypt = std::time::Instant::now();
    let encrypted = uffs_security::crypto::encrypt_cache(&compressed, &key)?;
    let encrypt_ms = t_encrypt.elapsed().as_millis();
    drop(compressed);

    let t_write = std::time::Instant::now();
    atomic_write(path, &encrypted)?;
    let write_ms = t_write.elapsed().as_millis();

    if profile {
        #[expect(
            clippy::float_arithmetic,
            reason = "display-only MB conversion for profiling"
        )]
        let mb = |bytes: usize| usize_to_f64(bytes) / (1_024.0_f64 * 1_024.0_f64);
        #[expect(clippy::float_arithmetic, reason = "display-only ratio for profiling")]
        let ratio = usize_to_f64(uncompressed_len) / usize_to_f64(compressed_len);
        let total_ms = t_total.elapsed().as_millis();
        tracing::debug!(
            target: "cache_profile",
            label,
            compress_ms = %compress_ms,
            uncomp_mb = %format_args!("{:.1}", mb(uncompressed_len)),
            comp_mb = %format_args!("{:.1}", mb(compressed_len)),
            ratio = %format_args!("{ratio:.1}"),
            encrypt_ms = %encrypt_ms,
            write_ms = %write_ms,
            write_mb = %format_args!("{:.1}", mb(encrypted.len())),
            total_ms = %total_ms,
            "bg_compress_encrypt_write"
        );
    }

    Ok(())
}
