// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! AES-256-GCM authenticated encryption for cache files.
//!
//! # UFFSENC File Format
//!
//! ## Version 2 (current — writes always use this)
//!
//! ```text
//! Offset  Size    Field
//! ──────  ──────  ──────────────────────────────
//! 0       8       Magic: b"UFFSENC\0"
//! 8       2       Format version (u16 LE) = 2
//! 10      1       Algorithm ID: 0x01 = AES-256-GCM
//! 11      1       KDF ID (0x01=DPAPI, 0x02=Keychain, 0x03=SecretService, 0x04=HKDF)
//! 12      12      Nonce (96-bit, random per write)
//! 24      8       Plaintext length (u64 LE) — supports up to 16 EiB
//! 32      N       Ciphertext
//! 32+N    16      GCM Authentication Tag
//! ────────────────────────────────────────────────
//! Total overhead: 48 bytes
//! AAD: bytes 0..32 (header, included in GCM auth)
//! ```
//!
//! ## Version 1 (legacy — read-only support)
//!
//! Same layout but offset 24 has a 4-byte u32 plaintext length (header = 28
//! bytes). Supported for backward compatibility on read; never written.

use std::io;

use aes_gcm::aead::inout::InOutBuf;
use aes_gcm::{AeadInOut as _, Aes256Gcm, KeyInit as _, Tag};

// ────────────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────────────

/// Magic bytes identifying an encrypted UFFS cache file.
pub(crate) const ENCRYPTED_MAGIC: &[u8; 8] = b"UFFSENC\0";

/// Magic bytes identifying a legacy plaintext UFFS cache file.
pub(crate) const LEGACY_MAGIC: &[u8; 8] = b"UFFSIDX\0";

/// Current encryption format version (v2: u64 plaintext length).
pub(crate) const ENC_FORMAT_VERSION: u16 = 2;

/// Algorithm ID for AES-256-GCM.
pub(crate) const ALGO_AES_256_GCM: u8 = 0x01;

/// KDF ID: Windows DPAPI.
pub const KDF_DPAPI: u8 = 0x01;
/// KDF ID: macOS Keychain.
pub const KDF_KEYCHAIN: u8 = 0x02;
/// KDF ID: Linux Secret Service (D-Bus).
pub const KDF_SECRET_SERVICE: u8 = 0x03;
/// KDF ID: HKDF fallback (headless Linux).
pub const KDF_HKDF: u8 = 0x04;

/// Size of the UFFSENC v2 header (before ciphertext).
const HEADER_SIZE_V2: usize = 32;
/// Size of the legacy UFFSENC v1 header (before ciphertext).
const HEADER_SIZE_V1: usize = 28;
/// Size of the GCM authentication tag.
const TAG_SIZE: usize = 16;
/// Size of the AES-GCM nonce (96 bits).
const NONCE_SIZE: usize = 12;

// ────────────────────────────────────────────────────────────────────────────
// Format Detection
// ────────────────────────────────────────────────────────────────────────────

/// Detected cache file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheFormat {
    /// Encrypted with UFFSENC header.
    Encrypted,
    /// Legacy plaintext with UFFSIDX header.
    LegacyPlaintext,
    /// Unknown / unrecognised format.
    Unknown,
}

/// Detects the format of a cache file from its first bytes.
///
/// Requires at least 8 bytes to identify the magic.
#[must_use]
pub fn detect_format(data: &[u8]) -> CacheFormat {
    if let Some(magic) = data.get(..8) {
        if *magic == *ENCRYPTED_MAGIC {
            return CacheFormat::Encrypted;
        }
        if *magic == *LEGACY_MAGIC {
            return CacheFormat::LegacyPlaintext;
        }
    }
    CacheFormat::Unknown
}

// ────────────────────────────────────────────────────────────────────────────
// Encrypt
// ────────────────────────────────────────────────────────────────────────────

/// Encrypts plaintext using AES-256-GCM and wraps it in the UFFSENC v2 format.
///
/// The v2 format uses a u64 plaintext length field, supporting payloads up to
/// 16 EiB (effectively unlimited for file-system index caches).
///
/// # Errors
///
/// Returns an error if encryption fails (should not happen with valid key).
pub fn encrypt_cache(plaintext: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>> {
    use rand::Rng as _;

    let plaintext_len = plaintext.len() as u64; // usize→u64 lossless on 64-bit

    // Generate random 96-bit nonce
    let mut nonce_bytes = [0_u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce_bytes);

    // KDF ID for the current platform
    #[cfg(target_os = "windows")]
    let kdf_id = KDF_DPAPI;
    #[cfg(target_os = "macos")]
    let kdf_id = KDF_KEYCHAIN;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let kdf_id = KDF_SECRET_SERVICE;

    // Build header (32 bytes for v2)
    let mut output = Vec::with_capacity(HEADER_SIZE_V2 + plaintext.len() + TAG_SIZE);
    output.extend_from_slice(ENCRYPTED_MAGIC); // 0..8
    output.extend_from_slice(&ENC_FORMAT_VERSION.to_le_bytes()); // 8..10
    output.push(ALGO_AES_256_GCM); // 10
    output.push(kdf_id); // 11
    output.extend_from_slice(&nonce_bytes); // 12..24
    output.extend_from_slice(&plaintext_len.to_le_bytes()); // 24..32  (u64)

    // AAD = header bytes 0..32
    let aad = output
        .get(..HEADER_SIZE_V2)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "header shorter than expected"))?
        .to_vec();

    // Append plaintext (will be encrypted in-place)
    let ciphertext_start = output.len();
    output.extend_from_slice(plaintext);

    // Encrypt in-place
    let cipher = Aes256Gcm::new(key.into());
    let tag = cipher
        .encrypt_inout_detached(
            (&nonce_bytes).into(),
            &aad,
            InOutBuf::from(output.get_mut(ciphertext_start..).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ciphertext offset out of bounds",
                )
            })?),
        )
        .map_err(|enc_err| io::Error::other(format!("AES-GCM encrypt failed: {enc_err}")))?;

    // Append 16-byte GCM tag
    output.extend_from_slice(&tag);

    Ok(output)
}

// ────────────────────────────────────────────────────────────────────────────
// Decrypt
// ────────────────────────────────────────────────────────────────────────────

/// Creates an `InvalidData` I/O error with the given message.
fn bad_data(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Decrypts an UFFSENC-formatted buffer, returning the original plaintext.
///
/// Copies `data` once and decrypts the copy in place; a caller that owns
/// the buffer should use [`decrypt_cache_in_place`] and skip that copy.
///
/// # Errors
///
/// See [`decrypt_cache_in_place`].
pub fn decrypt_cache(data: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>> {
    decrypt_cache_in_place(data.to_vec(), key)
}

/// Decrypts an UFFSENC-formatted buffer inside its own allocation and returns
/// that allocation holding only the plaintext.
///
/// Supports both v1 (u32 length, 28-byte header) and v2 (u64 length, 32-byte
/// header) formats. Validates the header, algorithm ID, and GCM auth tag
/// before any plaintext is returned.
///
/// The ciphertext is decrypted where it lies, then moved to the front of the
/// buffer and the header and tag cut off — no second buffer, so loading a
/// multi-GB cache does not hold the ciphertext and the plaintext at once.
///
/// # Errors
///
/// Returns an error if:
/// - The data is too short or has wrong magic
/// - The algorithm or version is unsupported
/// - GCM authentication fails (tampered data or wrong key)
pub fn decrypt_cache_in_place(mut data: Vec<u8>, key: &[u8; 32]) -> io::Result<Vec<u8>> {
    let envelope = parse_envelope(&data)?;
    let payload_end = envelope.header_size + envelope.payload_len;

    let tag_bytes: [u8; TAG_SIZE] = data
        .get(payload_end..payload_end + TAG_SIZE)
        .ok_or_else(|| bad_data("tag OOB"))?
        .try_into()
        .map_err(|_e| bad_data("tag size mismatch"))?;
    let auth_tag: &Tag = (&tag_bytes).into();

    let (header, rest) = data.split_at_mut(envelope.header_size);
    let ciphertext = rest
        .get_mut(..envelope.payload_len)
        .ok_or_else(|| bad_data("ciphertext OOB"))?;
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt_inout_detached(
            (&envelope.nonce).into(),
            header,
            ciphertext.into(),
            auth_tag,
        )
        .map_err(|_e| bad_data("AES-GCM authentication failed (wrong key or tampered data)"))?;

    data.copy_within(envelope.header_size..payload_end, 0);
    data.truncate(envelope.payload_len);
    Ok(data)
}

/// The parts of an UFFSENC header that decryption needs.
struct Envelope {
    /// Header length in bytes (also the AAD length): 28 for v1, 32 for v2.
    header_size: usize,
    /// Ciphertext (= plaintext) length in bytes.
    payload_len: usize,
    /// The 96-bit GCM nonce.
    nonce: [u8; NONCE_SIZE],
}

/// Validate an UFFSENC header and check `data` holds the whole payload and tag.
fn parse_envelope(data: &[u8]) -> io::Result<Envelope> {
    // Minimum size: smallest header (v1=28) + tag (16) = 44 bytes
    if data.len() < HEADER_SIZE_V1 + TAG_SIZE {
        return Err(bad_data(format!(
            "encrypted cache too short: {} bytes (min {})",
            data.len(),
            HEADER_SIZE_V1 + TAG_SIZE
        )));
    }

    if data.get(..8) != Some(ENCRYPTED_MAGIC.as_slice()) {
        return Err(bad_data("not an encrypted UFFS cache file (bad magic)"));
    }

    let algo = data
        .get(10)
        .copied()
        .ok_or_else(|| bad_data("missing algorithm byte"))?;
    if algo != ALGO_AES_256_GCM {
        return Err(bad_data(format!("unsupported algorithm: 0x{algo:02x}")));
    }

    // Version → header size + payload length
    let ver_bytes: [u8; 2] = data
        .get(8..10)
        .ok_or_else(|| bad_data("missing version"))?
        .try_into()
        .map_err(|_e| bad_data("invalid version"))?;
    let (header_size, payload_len) = match u16::from_le_bytes(ver_bytes) {
        1 => {
            let len_buf: [u8; 4] = data
                .get(24..28)
                .ok_or_else(|| bad_data("missing v1 length"))?
                .try_into()
                .map_err(|_e| bad_data("invalid v1 length"))?;
            (HEADER_SIZE_V1, u32::from_le_bytes(len_buf) as usize) // u32→usize lossless on 64-bit
        }
        2 => {
            if data.len() < HEADER_SIZE_V2 + TAG_SIZE {
                return Err(bad_data(format!(
                    "v2 cache too short: {} bytes (min {})",
                    data.len(),
                    HEADER_SIZE_V2 + TAG_SIZE
                )));
            }
            let len_buf: [u8; 8] = data
                .get(24..32)
                .ok_or_else(|| bad_data("missing v2 length"))?
                .try_into()
                .map_err(|_e| bad_data("invalid v2 length"))?;
            let len64 = u64::from_le_bytes(len_buf);
            let len = usize::try_from(len64).map_err(|_e| {
                bad_data(format!("plaintext length {len64} exceeds platform usize"))
            })?;
            (HEADER_SIZE_V2, len)
        }
        ver => return Err(bad_data(format!("unsupported format version: {ver}"))),
    };

    let nonce: [u8; NONCE_SIZE] = data
        .get(12..24)
        .ok_or_else(|| bad_data("data too short for nonce"))?
        .try_into()
        .map_err(|_e| bad_data("invalid nonce"))?;

    let expected = header_size
        .checked_add(payload_len)
        .and_then(|len| len.checked_add(TAG_SIZE))
        .ok_or_else(|| bad_data("payload length overflows"))?;
    if data.len() < expected {
        return Err(bad_data(format!(
            "encrypted cache truncated: have {} bytes, expected {expected}",
            data.len()
        )));
    }

    Ok(Envelope {
        header_size,
        payload_len,
        nonce,
    })
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// S2.3.5: encrypt → decrypt round-trip, various sizes.
    #[test]
    fn round_trip_empty() {
        let key = [0x42_u8; 32];
        let plaintext = b"";
        let encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_1_byte() {
        let key = [0xAB_u8; 32];
        let plaintext = b"X";
        let encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_1mb() {
        let key = [0xCD_u8; 32];
        let plaintext = vec![0x55_u8; 1024 * 1024];
        let encrypted = encrypt_cache(&plaintext, &key).expect("encrypt");
        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    /// In-place decryption hands back the plaintext in the buffer it was
    /// given — no second allocation of the payload's size.
    #[test]
    fn in_place_round_trip_reuses_the_buffer() {
        let key = [0xCD_u8; 32];
        let plaintext: Vec<u8> = (0..=255_u8).cycle().take(64 * 1024).collect();
        let encrypted = encrypt_cache(&plaintext, &key).expect("encrypt");
        let capacity = encrypted.capacity();
        let decrypted = decrypt_cache_in_place(encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
        assert_eq!(
            decrypted.capacity(),
            capacity,
            "the same allocation must be reused"
        );
    }

    /// Tampering is still caught before any plaintext is returned.
    #[test]
    fn in_place_rejects_tampered_ciphertext() {
        let key = [0x11_u8; 32];
        let mut encrypted = encrypt_cache(b"hello world", &key).expect("encrypt");
        if let Some(byte) = encrypted.get_mut(HEADER_SIZE_V2) {
            *byte ^= 0xFF;
        }
        let err = decrypt_cache_in_place(encrypted, &key).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    }

    /// A header claiming a length that cannot be added up is rejected, not
    /// wrapped around into a small bound.
    #[test]
    fn overflowing_payload_length_is_rejected() {
        let key = [0x22_u8; 32];
        let mut encrypted = encrypt_cache(b"x", &key).expect("encrypt");
        if let Some(len) = encrypted.get_mut(24..32) {
            len.copy_from_slice(&u64::MAX.to_le_bytes());
        }
        let err = decrypt_cache_in_place(encrypted, &key).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    }

    /// S2.3.6: tampered ciphertext → decrypt fails.
    #[test]
    fn tampered_ciphertext() {
        let key = [0x11_u8; 32];
        let plaintext = b"hello world";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Flip a byte in the ciphertext region (v2 header = 32 bytes)
        if let Some(byte) = encrypted.get_mut(HEADER_SIZE_V2) {
            *byte ^= 0xFF;
        }
        decrypt_cache(&encrypted, &key).unwrap_err();
    }

    /// S2.3.7: tampered header → decrypt fails (AAD mismatch).
    #[test]
    fn tampered_header_nonce() {
        let key = [0x22_u8; 32];
        let plaintext = b"hello world";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Flip a nonce byte
        if let Some(byte) = encrypted.get_mut(14) {
            *byte ^= 0xFF;
        }
        decrypt_cache(&encrypted, &key).unwrap_err();
    }

    #[test]
    fn tampered_header_algo() {
        let key = [0x33_u8; 32];
        let plaintext = b"hello world";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Change algo ID
        if let Some(byte) = encrypted.get_mut(10) {
            *byte = 0xFF;
        }
        decrypt_cache(&encrypted, &key).unwrap_err();
    }

    /// S2.3.8: truncated file → decrypt fails.
    #[test]
    fn truncated_file() {
        let key = [0x44_u8; 32];
        let plaintext = b"hello world";
        let encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Truncate to just the header (v2 = 32 bytes)
        let header_only = encrypted.get(..HEADER_SIZE_V2).expect("header slice");
        decrypt_cache(header_only, &key).unwrap_err();
        // Truncate mid-ciphertext
        let partial = encrypted.get(..HEADER_SIZE_V2 + 5).expect("partial slice");
        decrypt_cache(partial, &key).unwrap_err();
    }

    /// v2 format round-trip with large payload (validates u64 length field).
    #[test]
    fn round_trip_v2_header_format() {
        let key = [0xEE_u8; 32];
        let plaintext = vec![0xAB_u8; 5_000_000]; // 5 MB
        let encrypted = encrypt_cache(&plaintext, &key).expect("encrypt");

        // Verify v2 header: version = 2, header size = 32
        assert_eq!(encrypted.get(8..10), Some([2_u8, 0].as_slice()));
        // Verify u64 length at offset 24..32
        let len_bytes: [u8; 8] = encrypted
            .get(24..32)
            .expect("len slice")
            .try_into()
            .unwrap();
        assert_eq!(u64::from_le_bytes(len_bytes), 5_000_000);

        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    /// v1 format backward compatibility: hand-craft a v1 header and verify
    /// decrypt still works.
    #[test]
    fn decrypt_v1_backward_compat() {
        let key_bytes = [0x77_u8; 32];
        let plaintext = b"v1 payload data";
        let nonce_bytes = [0x01_u8; 12];

        // Build a v1 header manually (28 bytes)
        let mut v1_data = Vec::new();
        v1_data.extend_from_slice(ENCRYPTED_MAGIC); // 0..8
        v1_data.extend_from_slice(&1_u16.to_le_bytes()); // 8..10 version=1
        v1_data.push(ALGO_AES_256_GCM); // 10
        v1_data.push(KDF_DPAPI); // 11
        v1_data.extend_from_slice(&nonce_bytes); // 12..24
        let payload_len: u32 = plaintext.len().try_into().expect("test payload fits u32");
        v1_data.extend_from_slice(&payload_len.to_le_bytes()); // 24..28 (u32)

        let aad = v1_data.clone();
        let ciphertext_start = v1_data.len();
        v1_data.extend_from_slice(plaintext);

        let cipher = Aes256Gcm::new((&key_bytes).into());
        let ct_region = v1_data.get_mut(ciphertext_start..).expect("ct region");
        let tag = cipher
            .encrypt_inout_detached((&nonce_bytes).into(), &aad, ct_region.into())
            .expect("encrypt");
        v1_data.extend_from_slice(&tag);

        // decrypt_cache should handle v1 format
        let decrypted = decrypt_cache(&v1_data, &key_bytes).expect("decrypt v1");
        assert_eq!(decrypted, plaintext);
    }

    /// S2.3.9: legacy UFFSIDX magic → `detect_format` returns
    /// `LegacyPlaintext`.
    #[test]
    fn detect_legacy() {
        let mut data = vec![0_u8; 64];
        data.get_mut(..8)
            .expect("data slice")
            .copy_from_slice(LEGACY_MAGIC);
        assert_eq!(detect_format(&data), CacheFormat::LegacyPlaintext);
    }

    #[test]
    fn detect_encrypted() {
        let mut data = vec![0_u8; 64];
        data.get_mut(..8)
            .expect("data slice")
            .copy_from_slice(ENCRYPTED_MAGIC);
        assert_eq!(detect_format(&data), CacheFormat::Encrypted);
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(detect_format(b"RANDOM"), CacheFormat::Unknown);
        assert_eq!(detect_format(b""), CacheFormat::Unknown);
    }

    /// Wrong key → decrypt fails.
    #[test]
    fn wrong_key() {
        let key1 = [0x11_u8; 32];
        let key2 = [0x22_u8; 32];
        let plaintext = b"secret data";
        let encrypted = encrypt_cache(plaintext, &key1).expect("encrypt");
        decrypt_cache(&encrypted, &key2).unwrap_err();
    }

    // ────────────────────────────────────────────────────────────────────────
    // S2.5 Performance baselines
    // Run: `cargo test -p uffs-security --release -- --ignored --nocapture`
    // ────────────────────────────────────────────────────────────────────────

    /// Bytes per mebibyte.
    const BYTES_PER_MB: usize = 1_024 * 1_024;

    /// Measures wall-clock throughput over `runs` iterations, returning MB/s.
    ///
    /// Runs the closure once as a warm-up before timing.
    fn measure_throughput_mb_s(payload_bytes: usize, runs: u32, workload: impl Fn()) -> usize {
        // Warm-up
        workload();
        let start = std::time::Instant::now();
        for _ in 0..runs {
            workload();
        }
        let elapsed_us = start.elapsed().as_micros().max(1);
        // MB/s = (payload_bytes * runs * 1_000_000) / (BYTES_PER_MB *
        // elapsed_us)
        let payload_u128 = payload_bytes as u128;
        let mib_u128 = BYTES_PER_MB as u128;
        let numerator = payload_u128 * u128::from(runs) * 1_000_000_u128;
        let denominator = mib_u128 * elapsed_us;
        // Throughput in MB/s fits comfortably in usize on 64-bit targets.
        usize::try_from(numerator / denominator).unwrap_or(usize::MAX)
    }

    /// Minimum acceptable throughput (MB/s). Below this the test fails,
    /// printing the measured value so the regression is visible.
    const MIN_THROUGHPUT_MB_S: usize = 50;

    /// S2.5.1: encrypt throughput baseline.
    ///
    /// Panics if throughput drops below [`MIN_THROUGHPUT_MB_S`], printing
    /// the measured value for triage.
    #[test]
    #[ignore = "S2.5 perf baseline — run with --ignored"]
    fn baseline_encrypt_throughput() {
        let key = [0xBE_u8; 32];
        let runs = 5_u32;

        for size_mb in [100_usize, 500_usize] {
            let payload = vec![0xAA_u8; size_mb * BYTES_PER_MB];
            let mb_s = measure_throughput_mb_s(payload.len(), runs, || {
                drop(encrypt_cache(&payload, &key));
            });
            assert!(
                mb_s >= MIN_THROUGHPUT_MB_S,
                "encrypt {size_mb} MB: {mb_s} MB/s — below floor of {MIN_THROUGHPUT_MB_S} MB/s",
            );
        }
    }

    /// S2.5.2: decrypt throughput baseline.
    ///
    /// Panics if throughput drops below [`MIN_THROUGHPUT_MB_S`], printing
    /// the measured value for triage.
    #[test]
    #[ignore = "S2.5 perf baseline — run with --ignored"]
    fn baseline_decrypt_throughput() {
        let key = [0xBF_u8; 32];
        let runs = 5_u32;

        for size_mb in [100_usize, 500_usize] {
            let payload = vec![0xBB_u8; size_mb * BYTES_PER_MB];
            let encrypted = encrypt_cache(&payload, &key).expect("encrypt setup");
            let mb_s = measure_throughput_mb_s(encrypted.len(), runs, || {
                drop(decrypt_cache(&encrypted, &key));
            });
            assert!(
                mb_s >= MIN_THROUGHPUT_MB_S,
                "decrypt {size_mb} MB: {mb_s} MB/s — below floor of {MIN_THROUGHPUT_MB_S} MB/s",
            );
        }
    }

    // ── Mutation-kill tests (2026-08-17 nightly cargo-mutants run) ──
    // Each test below pins one boundary the mutants run proved
    // untested: it asserts the SPECIFIC error (or success) that
    // distinguishes the real operator from its surviving mutation,
    // not merely "some error happened".

    /// Build a synthetic UFFSENC prefix (magic + version + algorithm)
    /// zero-padded to `total_len` — indexing-free by construction so the
    /// test lane's `indexing_slicing` deny holds even in tests.
    fn synthetic_header(version: u16, total_len: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(total_len);
        data.extend_from_slice(ENCRYPTED_MAGIC.as_slice()); // 0..8
        data.extend_from_slice(&version.to_le_bytes()); // 8..10
        data.push(ALGO_AES_256_GCM); // 10
        data.resize(total_len, 0);
        data
    }

    /// Kills `<` → `==`/`<=` at the v1 minimum-size gate (192:19): a
    /// buffer of EXACTLY the 44-byte minimum must get PAST the gate
    /// (failing later, at GCM auth — not with "too short").
    #[test]
    fn v1_exact_minimum_size_passes_the_size_gate() {
        // v1 length field (24..28) stays 0 → expected = 28 + 0 + 16 = 44.
        let data = synthetic_header(1, HEADER_SIZE_V1 + TAG_SIZE);
        let err = decrypt_cache(&data, &[0_u8; 32]).expect_err("garbage tag must fail GCM auth");
        let msg = err.to_string();
        assert!(
            !msg.contains("too short"),
            "a 44-byte buffer must pass the minimum-size gate, got: {msg}"
        );
    }

    /// Kills `+` → `-` inside the v1 minimum (192:36): an under-minimum
    /// buffer must be rejected BY THE SIZE GATE, with its message —
    /// not stumble into a later header-parse error.
    #[test]
    fn v1_under_minimum_is_rejected_by_the_size_gate() {
        let data = synthetic_header(1, 20);
        let err = decrypt_cache(&data, &[0_u8; 32]).expect_err("20 bytes is under the minimum");
        assert!(
            err.to_string().contains("too short"),
            "the size gate must reject it, got: {err}"
        );
    }

    /// Kills `+` → `-` in the v2 minimum (228:44): an under-minimum v2
    /// buffer must be rejected by the v2 size gate with its message.
    #[test]
    fn v2_under_minimum_is_rejected_by_the_v2_size_gate() {
        let data = synthetic_header(2, HEADER_SIZE_V2 + TAG_SIZE - 2);
        let err = decrypt_cache(&data, &[0_u8; 32]).expect_err("46 bytes is under the v2 minimum");
        assert!(
            err.to_string().contains("v2 cache too short"),
            "the v2 size gate must reject it, got: {err}"
        );
    }

    /// Kills `+` → `-` in `expected` (255:46) — a one-byte-truncated
    /// real ciphertext must be rejected as TRUNCATED, not slip through
    /// a shrunken expectation into a GCM failure.
    #[test]
    fn one_byte_truncation_is_reported_as_truncated() {
        let key = [0x77_u8; 32];
        let mut encrypted = encrypt_cache(b"boundary", &key).expect("encrypt");
        let _last = encrypted.pop();
        let err = decrypt_cache(&encrypted, &key).expect_err("truncated input must fail");
        assert!(
            err.to_string().contains("truncated"),
            "must fail the expected-size check, got: {err}"
        );
    }

    /// Kills `<` → `>` in the truncation check (256:19): TRAILING bytes
    /// after a valid ciphertext are tolerated (the slices are exact),
    /// so decryption must still succeed.
    #[test]
    fn trailing_garbage_after_valid_ciphertext_still_decrypts() {
        let key = [0x33_u8; 32];
        let plaintext = b"suffix tolerant";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        encrypted.extend_from_slice(&[0xEE; 7]);
        let decrypted = decrypt_cache(&encrypted, &key).expect("trailing bytes are not an error");
        assert_eq!(decrypted, plaintext);
    }
}
