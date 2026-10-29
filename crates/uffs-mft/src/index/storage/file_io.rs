// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! File-based persistence wrappers for `MftIndex` snapshots.
//!
//! Cache files are encrypted with AES-256-GCM when a platform key is
//! available. Legacy plaintext files (`UFFSIDX` magic) are auto-migrated
//! to encrypted format on first load.
//!
//! Since v0.4.22 the serialized bytes are zstd-compressed before encryption.
//! On load, the decompressor detects the zstd frame magic (`0xFD2FB528`) and
//! decompresses automatically; older uncompressed caches are still loaded
//! transparently.

use super::IndexHeader;
use crate::index::{MftIndex, usize_to_f64};

/// zstd frame magic bytes (little-endian `0xFD2FB528`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Default zstd compression level (3 = good balance of speed vs ratio).
const ZSTD_LEVEL: i32 = 3;

/// Returns `true` if `data` starts with the zstd frame magic.
fn is_zstd_compressed(data: &[u8]) -> bool {
    data.get(..4).is_some_and(|magic| magic == ZSTD_MAGIC)
}

impl MftIndex {
    /// Saves the index to a file.
    ///
    /// The serialized bytes are zstd-compressed and then encrypted with
    /// AES-256-GCM before writing. Encryption is mandatory — if the key
    /// is unavailable or encryption fails, an error is returned and **no
    /// data is written to disk**.
    ///
    /// # Errors
    ///
    /// Returns an error if compression, encryption, or file writing fails.
    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        volume_serial: u64,
        usn_journal_id: u64,
        next_usn: crate::usn::Usn,
    ) -> std::io::Result<()> {
        let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
        let t_save_total = std::time::Instant::now();

        let t_serialize = std::time::Instant::now();
        let serialized = self.serialize(volume_serial, usn_journal_id, next_usn);
        let serialize_ms = t_serialize.elapsed().as_millis();
        let uncompressed_len = serialized.len();

        // Compress with multi-threaded zstd before encryption
        let t_compress = std::time::Instant::now();
        let compressed = crate::cache::compress_zstd_mt(&serialized, ZSTD_LEVEL)?;
        let compress_ms = t_compress.elapsed().as_millis();
        let compressed_len = compressed.len();

        let key = uffs_security::keystore::get_cache_key().map_err(|err| {
            std::io::Error::other(format!("cannot save cache without encryption key: {err}"))
        })?;

        let t_encrypt = std::time::Instant::now();
        let data = uffs_security::crypto::encrypt_cache(&compressed, &key)?;
        let encrypt_ms = t_encrypt.elapsed().as_millis();

        let t_write = std::time::Instant::now();
        let result = crate::cache::atomic_write(path, &data);
        let write_ms = t_write.elapsed().as_millis();

        if profile {
            #[expect(
                clippy::float_arithmetic,
                reason = "display-only MB conversion for profiling"
            )]
            let mb = |bytes: usize| usize_to_f64(bytes) / (1_024.0_f64 * 1_024.0_f64);
            #[expect(clippy::float_arithmetic, reason = "display-only ratio for profiling")]
            let ratio = usize_to_f64(uncompressed_len) / usize_to_f64(compressed_len);
            let save_total_ms = t_save_total.elapsed().as_millis();
            tracing::debug!(
                target: "cache_profile",
                serialize_ms = %serialize_ms,
                uncomp_mb = %format_args!("{:.1}", mb(uncompressed_len)),
                compress_ms = %compress_ms,
                comp_mb = %format_args!("{:.1}", mb(compressed_len)),
                ratio = %format_args!("{ratio:.1}"),
                encrypt_ms = %encrypt_ms,
                write_ms = %write_ms,
                write_mb = %format_args!("{:.1}", mb(data.len())),
                total_ms = %save_total_ms,
                "mft_save"
            );
        }

        result
    }

    /// Loads an index from a file.
    ///
    /// Detects the file format automatically:
    /// - **`UFFSENC`**: decrypts with the platform key, then deserializes
    /// - **`UFFSIDX`** (legacy plaintext): deserializes directly, then re-saves
    ///   as encrypted (one-time auto-migration)
    /// - **Unknown**: returns an error
    ///
    /// After decryption, if the plaintext starts with the zstd frame magic
    /// (`0xFD2FB528`), it is decompressed before deserialization. Older
    /// uncompressed caches are loaded transparently.
    ///
    /// If decryption fails (wrong key / tampered), the corrupted file is
    /// deleted and an error is returned so the caller rebuilds from MFT.
    ///
    /// Set `UFFS_CACHE_PROFILE=1` to emit per-phase timing to stderr.
    ///
    /// # Errors
    ///
    /// Returns an error if file reading, decryption, or deserialization fails.
    pub fn load_from_file(
        path: &std::path::Path,
    ) -> Result<(Self, IndexHeader), Box<dyn core::error::Error>> {
        use uffs_security::crypto::{CacheFormat, decrypt_cache_in_place, detect_format};

        let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
        let t_total = std::time::Instant::now();

        let t0 = std::time::Instant::now();
        let raw = std::fs::read(path)?;
        let read_ms = t0.elapsed().as_millis();
        let raw_len = raw.len();

        let format = detect_format(&raw);

        let t1 = std::time::Instant::now();
        let decrypted = match format {
            CacheFormat::Encrypted => {
                let key = uffs_security::keystore::get_cache_key()
                    .map_err(|err| Box::new(err) as Box<dyn core::error::Error>)?;
                match decrypt_cache_in_place(raw, &key) {
                    Ok(pt) => pt,
                    Err(decrypt_err) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %decrypt_err,
                            "Cache decryption failed — deleting corrupted file"
                        );
                        let _rm_result = std::fs::remove_file(path);
                        return Err(Box::new(decrypt_err));
                    }
                }
            }
            CacheFormat::LegacyPlaintext => {
                tracing::info!(
                    path = %path.display(),
                    "Loading legacy plaintext cache (will re-encrypt on next save)"
                );
                raw
            }
            CacheFormat::Unknown => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown cache file format: {}", path.display()),
                )));
            }
        };
        let decrypt_ms = t1.elapsed().as_millis();

        // Decompress if zstd-compressed (backward compat: old caches skip this)
        let t_decompress = std::time::Instant::now();
        let compressed = is_zstd_compressed(&decrypted);
        let plaintext = if compressed {
            crate::cache::decompress_sized(&decrypted).map_err(|err| {
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("zstd decompression failed: {err}"),
                )) as Box<dyn core::error::Error>
            })?
        } else {
            decrypted
        };
        let decompress_ms = t_decompress.elapsed().as_millis();
        let plaintext_len = plaintext.len();

        let t2 = std::time::Instant::now();
        let (index, header) = Self::deserialize(&plaintext)?;
        let deser_ms = t2.elapsed().as_millis();

        let total_ms = t_total.elapsed().as_millis();

        if profile {
            #[expect(
                clippy::float_arithmetic,
                reason = "display-only MB conversion for profiling"
            )]
            let mb = |bytes: usize| usize_to_f64(bytes) / (1_024.0_f64 * 1_024.0_f64);
            tracing::debug!(
                target: "cache_profile",
                read_ms = %read_ms,
                raw_mb = %format_args!("{:.1}", mb(raw_len)),
                decrypt_ms = %decrypt_ms,
                compressed,
                decompress_ms = %decompress_ms,
                plain_mb = %format_args!("{:.1}", mb(plaintext_len)),
                deser_ms = %deser_ms,
                records = index.len(),
                total_ms = %total_ms,
                "mft_load"
            );
        }

        Ok((index, header))
    }
}
