// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The decompressed size of a cache, carried next to its compressed bytes.
//!
//! Caches are compressed by a streaming, multi-threaded zstd encoder, which
//! does not record the decompressed size in the frame.  Decompressing such a
//! frame into a `Vec` grows it by doubling, so at the last step the old and
//! the new buffer both exist: for a 4.9 GB index, up to ~10 GB at once.
//!
//! After the data frame the writers append one zstd **skippable frame** that
//! holds the decompressed length.  Every zstd decoder skips skippable frames
//! (they are part of the zstd format), so a cache written this way still
//! decodes with any reader, including older UFFS builds; a reader that knows
//! the frame allocates the exact size up front.  A cache without the frame
//! (written by an older build) decodes as before.

use std::io;

/// Skippable-frame magic (`0x184D2A50..=0x184D2A5F` are reserved for
/// skippable frames; this one marks the UFFS size trailer).
const SKIPPABLE_MAGIC: u32 = 0x184D_2A5E;

/// Payload tag that identifies the trailer among skippable frames.
const TRAILER_TAG: &[u8; 4] = b"ULEN";

/// Payload length: the 4-byte tag plus the u64 decompressed length.
const PAYLOAD_LEN: u32 = 12;

/// Whole trailer: magic, payload length, payload.
const TRAILER_LEN: usize = 8 + 12;

/// Decompressed sizes beyond this are treated as no trailer at all, so a
/// damaged value can never request an absurd allocation.
const MAX_TRUSTED_LEN: u64 = 1 << 40;

/// A writer that counts the bytes passed through it.
#[derive(Debug)]
pub struct CountingWriter<W> {
    /// The wrapped writer.
    inner: W,
    /// Bytes written so far.
    written: u64,
}

impl<W> CountingWriter<W> {
    /// Wrap `inner`, starting the count at zero.
    pub const fn new(inner: W) -> Self {
        Self { inner, written: 0 }
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Unwrap the inner writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: io::Write> io::Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Append the size trailer for `decompressed_len` bytes to `compressed`.
pub fn append_size_trailer(compressed: &mut Vec<u8>, decompressed_len: u64) {
    compressed.reserve(TRAILER_LEN);
    compressed.extend_from_slice(&SKIPPABLE_MAGIC.to_le_bytes());
    compressed.extend_from_slice(&PAYLOAD_LEN.to_le_bytes());
    compressed.extend_from_slice(TRAILER_TAG);
    compressed.extend_from_slice(&decompressed_len.to_le_bytes());
}

/// The decompressed length recorded by [`append_size_trailer`], if
/// `compressed` ends with one.
#[must_use]
pub fn size_from_trailer(compressed: &[u8]) -> Option<usize> {
    let start = compressed.len().checked_sub(TRAILER_LEN)?;
    let trailer = compressed.get(start..)?;
    let field = |range: core::ops::Range<usize>| trailer.get(range);
    if field(0..4)? != SKIPPABLE_MAGIC.to_le_bytes()
        || field(4..8)? != PAYLOAD_LEN.to_le_bytes()
        || field(8..12)? != TRAILER_TAG
    {
        return None;
    }
    let len = u64::from_le_bytes(field(12..20)?.try_into().ok()?);
    if len > MAX_TRUSTED_LEN {
        return None;
    }
    usize::try_from(len).ok()
}

/// Decompress `compressed` into a buffer of exactly the size its trailer
/// records, or the growing way when it has none.
///
/// # Errors
///
/// Returns an error if the data does not decompress, or if it decompresses
/// to a different length than the trailer recorded.
pub fn decompress_sized(compressed: &[u8]) -> io::Result<Vec<u8>> {
    let Some(len) = size_from_trailer(compressed) else {
        return zstd::decode_all(compressed);
    };
    let mut decompressed = Vec::with_capacity(len);
    zstd::stream::copy_decode(compressed, &mut decompressed)?;
    if decompressed.len() != len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "cache decompressed to {} bytes, its size trailer says {len}",
                decompressed.len()
            ),
        ));
    }
    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{CountingWriter, append_size_trailer, decompress_sized, size_from_trailer};

    fn sample() -> Vec<u8> {
        (0..200_000_u32).flat_map(u32::to_le_bytes).collect()
    }

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut writer = CountingWriter::new(zstd::Encoder::new(Vec::new(), 3).unwrap());
        writer.write_all(data).unwrap();
        let written = writer.written();
        let mut compressed = writer.into_inner().finish().unwrap();
        append_size_trailer(&mut compressed, written);
        compressed
    }

    #[test]
    fn trailer_records_the_exact_size_and_decodes_sized() {
        let data = sample();
        let compressed = compress(&data);
        assert_eq!(size_from_trailer(&compressed), Some(data.len()));
        let decoded = decompress_sized(&compressed).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(
            decoded.capacity(),
            data.len(),
            "allocated once, at the exact size"
        );
    }

    #[test]
    fn any_zstd_reader_skips_the_trailer() {
        let data = sample();
        let compressed = compress(&data);
        assert_eq!(zstd::decode_all(compressed.as_slice()).unwrap(), data);
    }

    #[test]
    fn cache_without_a_trailer_still_decodes() {
        let data = sample();
        let plain = zstd::encode_all(data.as_slice(), 3).unwrap();
        assert_eq!(size_from_trailer(&plain), None);
        assert_eq!(decompress_sized(&plain).unwrap(), data);
    }

    #[test]
    fn a_trailer_that_disagrees_with_the_data_is_an_error() {
        let data = sample();
        let mut compressed = zstd::encode_all(data.as_slice(), 3).unwrap();
        append_size_trailer(&mut compressed, data.len() as u64 + 1);
        let err = decompress_sized(&compressed).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    }
}
