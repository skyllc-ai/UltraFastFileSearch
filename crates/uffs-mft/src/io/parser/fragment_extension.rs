// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Legacy extension-record helper for direct-to-fragment parsing.
//! Extracts names and streams from extension records into a fragment.
//!
//! # Hardening (WI-5.2)
//! This module parses **untrusted on-disk bytes**. Every offset/length derived
//! from those bytes is combined with `checked_add`/`checked_mul` (or
//! `saturating_*` where overflow is provably unreachable) and every slice into
//! `data` goes through `.get()` / the `rd_u*` helpers — never `data[a..b]`
//! indexing. The daemon builds with `panic = "abort"`, so a single parser panic
//! on a malformed record would be a whole-process denial of service.
//! `arithmetic_side_effects` is enabled module-wide as a regression guard: any
//! new raw `+`/`*` on a byte-derived value is a compile error here.
#![warn(clippy::arithmetic_side_effects)]

use core::mem::size_of;

use smallvec::SmallVec;
use zerocopy::FromBytes as _;

use crate::index::{
    ChildInfo, IndexNameRef, IndexStreamInfo, LinkInfo, NO_ENTRY, SizeInfo, frs_to_usize,
    len_to_u16, len_to_u32, u32_as_usize,
};
use crate::ntfs::{
    AttributeRecordHeader, AttributeType, FileNameAttribute, FileRecordSegmentHeader,
};

/// Parses an extension record and adds its names/streams to the base record in
/// a fragment.
///
/// This is the parallel-parsing variant of `parse_extension_to_index`.
///
/// # Arguments
///
/// * `data` - The raw extension record data (after fixup)
/// * `base_frs` - The FRS of the base record this extension belongs to
/// * `fragment` - The MFT index fragment to update
///
/// # Returns
///
/// `true` if any names/streams were added, `false` otherwise.
#[deprecated(
    since = "0.2.203",
    note = "Use parse_record_full() + MftRecordMerger instead"
)]
#[expect(
    clippy::too_many_lines,
    reason = "monolithic extension parser for performance: nested FileName + Data \
              attribute parsing with resident/non-resident branches"
)]
pub(super) fn parse_extension_to_fragment(
    data: &[u8],
    base_frs: u64,
    fragment: &mut crate::index::MftIndexFragment,
) -> bool {
    if data.len() < size_of::<FileRecordSegmentHeader>() {
        return false;
    }

    let Ok((header, _)) = FileRecordSegmentHeader::read_from_prefix(data) else {
        return false;
    };

    // Parse attributes to find $FILE_NAME and $DATA
    let mut offset = usize::from(header.first_attribute_offset);
    let max_offset = core::cmp::min(u32_as_usize(header.bytes_in_use), data.len());

    // Collect names and streams from extension record
    let mut names: SmallVec<[(String, u64); 4]> = SmallVec::new();
    let mut streams: SmallVec<[(String, u64, u64); 4]> = SmallVec::new();

    while offset
        .checked_add(size_of::<AttributeRecordHeader>())
        .is_some_and(|end| end <= max_offset)
    {
        let Some(attr_slice) = data.get(offset..) else {
            break;
        };
        let Ok((attr_header, _)) = AttributeRecordHeader::read_from_prefix(attr_slice) else {
            break;
        };

        if attr_header.type_code == AttributeType::END_MARKER {
            break;
        }

        // `offset + length` can overflow on a crafted `length`; checked_add →
        // break.
        let Some(attr_end) = offset.checked_add(u32_as_usize(attr_header.length)) else {
            break;
        };
        if attr_header.length == 0 || attr_end > max_offset {
            break;
        }

        match AttributeType::from_u32(attr_header.type_code) {
            Some(AttributeType::FileName) if attr_header.is_non_resident == 0 => {
                let value_offset = usize::from(rd_u16(data, offset.saturating_add(20)));
                // `offset + value_offset` is byte-derived; checked, then
                // re-validated by the `.get()` below.
                if let Some(fn_offset) = offset.checked_add(value_offset)
                    && let Some(fn_slice) = fn_offset
                        .checked_add(size_of::<FileNameAttribute>())
                        .filter(|end| *end <= data.len())
                        .and_then(|_| data.get(fn_offset..))
                {
                    let Ok((fn_attr, _)) = FileNameAttribute::read_from_prefix(fn_slice) else {
                        break;
                    };

                    if fn_attr.file_name_namespace != 2 {
                        let name_len = usize::from(fn_attr.file_name_length);
                        let name_start = fn_offset.saturating_add(size_of::<FileNameAttribute>());
                        // `name_len * 2` (UTF-16) overflows on a crafted length
                        // → checked_mul/checked_add,
                        // then `.get()` bounds the slice.
                        if let Some(name_bytes) = name_len
                            .checked_mul(2)
                            .and_then(|byte_len| name_start.checked_add(byte_len))
                            .and_then(|end| data.get(name_start..end))
                        {
                            let name_u16: SmallVec<[u16; 64]> = name_bytes
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|pair| u16::from_le_bytes(*pair))
                                .collect();
                            let name = crate::io::parser::unified::decode_name_u16(&name_u16).0;
                            let parent_frs = fn_attr.parent_directory & 0x0000_FFFF_FFFF_FFFF;
                            names.push((name, parent_frs));
                        }
                    }
                }
            }
            Some(AttributeType::Data) => {
                // legacy-output parity: Only primary attributes (LowestVCN ==
                // 0) count as streams. Continuation extents
                // (LowestVCN > 0) are skipped.
                // See ntfs_index_load.hpp:358
                let is_primary = if attr_header.is_non_resident == 0 {
                    true // Resident attributes are always primary
                } else {
                    // Mirror the original guarded read: the 8-byte `LowestVCN`
                    // field must be fully present, else treat as non-primary
                    // ("can't verify, skip to be safe").
                    offset
                        .checked_add(16)
                        .filter(|nr| nr.saturating_add(8) <= data.len())
                        .is_some_and(|nr_offset| rd_u64(data, nr_offset).cast_signed() == 0)
                };

                if !is_primary {
                    // Skip continuation extents - they don't count as new
                    // streams
                    offset = offset.saturating_add(u32_as_usize(attr_header.length));
                    continue;
                }

                let name_len = usize::from(attr_header.name_length);
                if name_len > 0 {
                    let (size, allocated) = if attr_header.is_non_resident != 0 {
                        // `rd_u*` are individually bounds-safe (return 0 OOB);
                        // the `nr + 48 <= len` guard
                        // preserves the original "all fields
                        // present" semantics. `nr` via checked_add.
                        offset
                            .checked_add(16)
                            .filter(|nr| nr.saturating_add(48) <= data.len())
                            .map_or((0, 0), |nr_offset| {
                                let allocated =
                                    rd_u64(data, nr_offset.saturating_add(24)).cast_signed();
                                let size = rd_u64(data, nr_offset.saturating_add(32)).cast_signed();
                                (
                                    size.max(0).cast_unsigned(),
                                    allocated.max(0).cast_unsigned(),
                                )
                            })
                    } else {
                        // `rd_u32` is bounds-safe (returns 0 OOB); the `+ 4 <=
                        // len` guard preserves the
                        // original "field present" semantics.
                        offset
                            .checked_add(16)
                            .filter(|len_off| len_off.saturating_add(4) <= data.len())
                            .map_or((0, 0), |len_offset| {
                                (u64::from(rd_u32(data, len_offset)), 0)
                            })
                    };

                    let name_offset = offset.saturating_add(usize::from(attr_header.name_offset));
                    // `name_len * 2` (UTF-16) overflows on a crafted length →
                    // checked_mul/checked_add, then `.get()` bounds the slice.
                    if let Some(name_bytes) = name_len
                        .checked_mul(2)
                        .and_then(|byte_len| name_offset.checked_add(byte_len))
                        .and_then(|end| data.get(name_offset..end))
                    {
                        let name_u16: SmallVec<[u16; 64]> = name_bytes
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| u16::from_le_bytes(*pair))
                            .collect();
                        let stream_name = crate::io::parser::unified::decode_name_u16(&name_u16).0;
                        // ALL named $DATA streams create regular
                        // stream entries.  Internal ones are filtered from
                        // output by is_internal_windows_stream in the output
                        // layer.
                        streams.push((stream_name, size, allocated));
                    }
                }
            }
            _ => {}
        }

        // Disk-derived advance; saturate so a crafted length can't overflow.
        offset = offset.saturating_add(u32_as_usize(attr_header.length));
    }

    if names.is_empty() && streams.is_empty() {
        return false;
    }

    merge_extension_into_fragment(fragment, base_frs, &names, &streams)
}

/// Merge parsed extension-record names and streams into the fragment index.
///
/// Called after the attribute loop has collected `names` and `streams` from
/// the extension record.  Handles link/stream interning, chain threading,
/// parent-child relationships, and the edge case where the primary
/// `$FILE_NAME` lives only in an extension record.
#[expect(
    clippy::indexing_slicing,
    reason = "link_indices/stream_indices are valid indices into fragment vectors (just pushed); \
              chain loops iterate 0..len-1 with i+1 < len"
)]
fn merge_extension_into_fragment(
    fragment: &mut crate::index::MftIndexFragment,
    base_frs: u64,
    names: &[(String, u64)],
    streams: &[(String, u64, u64)],
) -> bool {
    // Add names to the fragment
    let mut link_indices: Vec<u32> = Vec::with_capacity(names.len());
    for (name, parent_frs) in names {
        let name_offset = len_to_u32(fragment.names.len());
        fragment.names.extend_from_slice(name.as_bytes());
        let name_len = name.len();
        let is_ascii = name.is_ascii();
        let link_ext_id = fragment.intern_extension(name);
        let name_ref = IndexNameRef::new(name_offset, len_to_u16(name_len), is_ascii, link_ext_id);

        let link_idx = len_to_u32(fragment.links.len());
        fragment.links.push(LinkInfo {
            next_entry: NO_ENTRY,
            name: name_ref,
            _pad0: [0; 4],
            // Typed `ParentFrs` slot — lift parser-local raw `u64`.
            parent_frs: crate::frs::ParentFrs::new(*parent_frs),
        });
        link_indices.push(link_idx);
    }

    // Add streams to the fragment
    let mut stream_indices: Vec<u32> = Vec::with_capacity(streams.len());
    for (stream_name, size, allocated) in streams {
        let name_offset = len_to_u32(fragment.names.len());
        fragment.names.extend_from_slice(stream_name.as_bytes());
        let name_len = stream_name.len();
        let is_ascii = stream_name.is_ascii();
        let stream_ext_id = fragment.intern_extension(stream_name);
        let name_ref =
            IndexNameRef::new(name_offset, len_to_u16(name_len), is_ascii, stream_ext_id);

        let stream_idx = len_to_u32(fragment.streams.len());
        fragment.streams.push(IndexStreamInfo {
            size: SizeInfo {
                length: *size,
                allocated: *allocated,
            },
            next_entry: NO_ENTRY,
            name: name_ref,
            flags: 8_u8 << 2_u8,
            _pad0: [0; 3],
        });
        stream_indices.push(stream_idx);
    }

    // Ensure parent directories exist.  Boundary: lift parser-local raw
    // `u64` to typed `Frs` at the typed-API call site.
    for (_, parent_frs) in names {
        if *parent_frs != base_frs && *parent_frs != 0 {
            fragment.get_or_create(crate::frs::Frs::new(*parent_frs));
            // ^ side effect: ensures parent placeholder exists
        }
    }

    // Chain new links together first (before getting record reference)
    for pair in link_indices.windows(2) {
        if let [current, next] = *pair {
            fragment.links[u32_as_usize(current)].next_entry = next;
        }
    }
    for pair in stream_indices.windows(2) {
        if let [current, next] = *pair {
            fragment.streams[u32_as_usize(current)].next_entry = next;
        }
    }

    // Get the first_name.next_entry, first_stream.next_entry, and first_name
    // validity before we start modifying things.  Lift parser-local raw `u64`
    // to typed `Frs` once for all the typed-API call sites in this function.
    let base_frs_typed = crate::frs::Frs::new(base_frs);
    let record = fragment.get_or_create(base_frs_typed);
    let first_name_valid = record.first_name.name.is_valid();
    let first_name_next = record.first_name.next_entry;
    let first_stream_next = record.first_stream.next_entry;

    // Find the end of the current link chain
    let link_chain_end = find_chain_end(first_name_next, |i| fragment.links[i].next_entry);
    let stream_chain_end = find_chain_end(first_stream_next, |i| fragment.streams[i].next_entry);

    // Attach new links
    attach_links(
        fragment,
        base_frs,
        &link_indices,
        first_name_valid,
        link_chain_end,
    );

    // Attach new streams
    if !stream_indices.is_empty() {
        if let Some(end_idx) = stream_chain_end {
            fragment.streams[u32_as_usize(end_idx)].next_entry = stream_indices[0];
        } else {
            let rec_for_stream = fragment.get_or_create(base_frs_typed);
            rec_for_stream.first_stream.next_entry = stream_indices[0];
        }
        let rec_for_count = fragment.get_or_create(base_frs_typed);
        // Bounded internal counters; saturate.
        let stream_added = len_to_u16(stream_indices.len());
        rec_for_count.stream_count = rec_for_count.stream_count.saturating_add(stream_added);
        rec_for_count.total_stream_count = rec_for_count
            .total_stream_count
            .saturating_add(stream_added);
    }

    // Build parent-child relationships
    build_parent_child_entries(fragment, base_frs, names);

    !names.is_empty() || !streams.is_empty()
}

/// Walk a `next_entry` chain and return the last index (or `None` if empty).
fn find_chain_end(start: u32, get_next: impl Fn(usize) -> u32) -> Option<u32> {
    if start == NO_ENTRY {
        return None;
    }
    let mut idx = start;
    while get_next(u32_as_usize(idx)) != NO_ENTRY {
        idx = get_next(u32_as_usize(idx));
    }
    Some(idx)
}

/// Attach extension-record link indices to the base record.
#[expect(
    clippy::indexing_slicing,
    clippy::missing_asserts_for_indexing,
    reason = "link_indices are valid indices into fragment.links (just pushed by caller); \
              early return guards link_indices.is_empty()"
)]
fn attach_links(
    fragment: &mut crate::index::MftIndexFragment,
    base_frs: u64,
    link_indices: &[u32],
    first_name_valid: bool,
    link_chain_end: Option<u32>,
) {
    if link_indices.is_empty() {
        return;
    }

    // Boundary: lift parser-local raw `u64` to typed `Frs` once for all
    // the typed-API call sites in this function.
    let base_frs_typed = crate::frs::Frs::new(base_frs);
    if first_name_valid {
        // Base record already has a name — chain extension names as additional
        // hard links
        if let Some(end_idx) = link_chain_end {
            fragment.links[u32_as_usize(end_idx)].next_entry = link_indices[0];
        } else {
            let rec_for_link_chain = fragment.get_or_create(base_frs_typed);
            rec_for_link_chain.first_name.next_entry = link_indices[0];
        }
        let rec_for_link_count = fragment.get_or_create(base_frs_typed);
        // Bounded internal counter; saturate.
        rec_for_link_count.name_count = rec_for_link_count
            .name_count
            .saturating_add(len_to_u16(link_indices.len()));
    } else {
        // Copy the first extension name directly into first_name
        // This matches established behavior (ntfs_index.hpp lines 559-567)
        let first_link_name = fragment.links[u32_as_usize(link_indices[0])].name;
        let first_link_parent = fragment.links[u32_as_usize(link_indices[0])].parent_frs;
        let rec_for_first_name = fragment.get_or_create(base_frs_typed);
        rec_for_first_name.first_name.name = first_link_name;
        rec_for_first_name.first_name.parent_frs = first_link_parent;

        // Chain remaining links (if any) to first_name.next_entry
        if link_indices.len() > 1 {
            let rec_for_extra_links = fragment.get_or_create(base_frs_typed);
            rec_for_extra_links.first_name.next_entry = link_indices[1];
            // Bounded internal counter; saturate.
            rec_for_extra_links.name_count = rec_for_extra_links
                .name_count
                .saturating_add(len_to_u16(link_indices.len().saturating_sub(1)));
        }
    }
}

/// Build parent→child entries for names added from extension records.
#[expect(
    clippy::indexing_slicing,
    reason = "frs_to_idx indices are bounds-checked by resize; parent_idx validated by get_or_create"
)]
fn build_parent_child_entries(
    fragment: &mut crate::index::MftIndexFragment,
    base_frs: u64,
    names: &[(String, u64)],
) {
    // Boundary: lift parser-local raw `u64` to typed `Frs` once for the
    // typed-API and `ChildInfo` writes below.
    let base_frs_typed = crate::frs::Frs::new(base_frs);
    let record = fragment.get_or_create(base_frs_typed);
    let existing_name_count = record.name_count;

    for (name_idx, (_, parent_frs)) in names.iter().enumerate() {
        let p_frs = *parent_frs;
        if p_frs == base_frs || p_frs == u64::from(NO_ENTRY) {
            continue;
        }

        // Ensure parent exists in fragment
        let parent_idx = {
            let p_frs_usize = frs_to_usize(p_frs);
            if p_frs_usize >= fragment.frs_to_idx.len() {
                // `p_frs` is masked to 48 bits, so `+ 1` cannot overflow usize
                // on 64-bit; saturate defensively to keep
                // arithmetic panic-free.
                fragment
                    .frs_to_idx
                    .resize(p_frs_usize.saturating_add(1), NO_ENTRY);
            }
            if fragment.frs_to_idx[p_frs_usize] == NO_ENTRY {
                let new_idx = len_to_u32(fragment.records.len());
                fragment.frs_to_idx[p_frs_usize] = new_idx;
                fragment
                    .records
                    .push(crate::index::FileRecord::new(crate::frs::Frs::new(p_frs)));
            }
            fragment.frs_to_idx[p_frs_usize]
        };

        let effective_name_idx = if existing_name_count == 0 {
            len_to_u16(name_idx)
        } else {
            // Preserve legacy off-by-one semantics; bounded u16 counters,
            // saturate.
            existing_name_count
                .saturating_sub(1)
                .saturating_add(len_to_u16(name_idx))
        };

        let child_idx = len_to_u32(fragment.children.len());
        let parent = &mut fragment.records[u32_as_usize(parent_idx)];
        let old_first_child = parent.first_child;
        parent.first_child = child_idx;

        fragment.children.push(ChildInfo {
            next_entry: old_first_child,
            _pad0: [0; 4],
            // Typed `Frs` slot — reuse cached typed FRS.
            child_frs: base_frs_typed,
            name_index: effective_name_idx,
            _pad1: [0; 6],
        });
    }
}

// ── Helpers (untrusted-byte readers, WI-5.2) ────────────────────────────────

/// Read a little-endian `u16` from `buf` at `off`, returning 0 if the 2-byte
/// field is out of bounds.
#[inline]
fn rd_u16(buf: &[u8], off: usize) -> u16 {
    off.checked_add(2)
        .and_then(|end| buf.get(off..end))
        .and_then(|sl| <[u8; 2]>::try_from(sl).ok())
        .map_or(0, u16::from_le_bytes)
}

/// Read a little-endian `u32` from `buf` at `off`, returning 0 if the 4-byte
/// field is out of bounds.
#[inline]
fn rd_u32(buf: &[u8], off: usize) -> u32 {
    off.checked_add(4)
        .and_then(|end| buf.get(off..end))
        .and_then(|sl| <[u8; 4]>::try_from(sl).ok())
        .map_or(0, u32::from_le_bytes)
}

/// Read a little-endian `u64` from `buf` at `off`, returning 0 if the 8-byte
/// field is out of bounds.
#[inline]
fn rd_u64(buf: &[u8], off: usize) -> u64 {
    off.checked_add(8)
        .and_then(|end| buf.get(off..end))
        .and_then(|sl| <[u8; 8]>::try_from(sl).ok())
        .map_or(0, u64::from_le_bytes)
}
