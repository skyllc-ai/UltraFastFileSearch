// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Low-level I/O operations for MFT reading.
//!
//! This module provides efficient disk I/O for reading MFT records:
//! - Aligned buffer management for direct I/O
//! - Sector-aligned reads
//! - Multi-sector fixup (Update Sequence Array)
//! - Fragmented MFT support via extent mapping
//! - Chunk planning and reader implementations tuned by drive type
//!
//! Available on all platforms for offline MFT processing (chaos mode, testing).
//! Live MFT access via HANDLE is Windows-only and gated per-function.

pub use crate::ntfs::SECTOR_SIZE;
// `SECTOR_SIZE_U64` is the matching u64 alias used by the I/O readers.
// Gated to `cfg(windows)` to match its only consumers (the Windows-only
// reader implementations under `io::readers::*` +
// `reader::persistence_capture`).
#[cfg(windows)]
pub(crate) use crate::ntfs::SECTOR_SIZE_U64;

mod aligned_buffer;
mod chunking;
mod extent_map;
mod fixup;
mod merger;
// `pub(crate)` so the instrumented name decoder (`parser::unified::
// decode_name_u16`, WI-4.1) is reachable from the sibling `parse/` and
// `usn/` modules that decode NTFS names — one decoder, one lossy tally.
pub(crate) mod parser;
// readers module available on all platforms (contains ChaosMftReader for
// offline MFT)
pub mod readers;

pub use aligned_buffer::AlignedBuffer;
pub use chunking::{ReadChunk, generate_precise_read_chunks, generate_read_chunks};
pub use extent_map::{MftExtentMap, PlannedRead};
pub use fixup::apply_fixup;
pub use merger::MftRecordMerger;
#[expect(
    deprecated,
    reason = "re-exporting deprecated API for backward compatibility"
)]
pub use parser::parse_record_to_fragment;
pub use parser::{
    ExtensionAttributes, ParseResult, ParsedColumns, ParsedRecord, StdInfoParse,
    add_missing_parent_placeholders_to_vec, create_placeholder_record, parse_record,
    parse_record_full, parse_record_zero_alloc, process_record,
};
#[cfg(windows)]
pub(crate) use readers::{IoCompletionPort, MftRecordReader, OverlappedRead};
// Re-export Windows-specific readers (require HANDLE).  All public
// readers were Phase 2.5-demoted in commit 1529cb162 — restored here to
// preserve their public API contracts.  IoCompletionPort / OverlappedRead
// stay pub(crate) (FFI primitives).  MftRecordReader is pub(crate) (was
// always so).
#[cfg(windows)]
pub use readers::{
    IocpMftReader, MultiVolumeIoOp, MultiVolumeIocpReader, ParallelMftReader, PipelinedMftReader,
    PrefetchMftReader, StreamingMftReader, VolumeState, prepare_volume_state,
};
