// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Standard MFT record parsing for base and extension records.

use tracing::debug;
use zerocopy::FromBytes as _;

use super::{
    ExtensionAttributes, ParseResult, ParsedRecord, PrimaryNameTracker, parse_data_attribute_full,
    parse_file_name_full, parse_standard_info_full,
};
use crate::index::nonneg_to_u64;
use crate::ntfs::{ExtendedStandardInfo, NameInfo, ReparsePointHeader, StreamInfo};

/// Parses an MFT record and extracts relevant information.
///
/// **LEGACY MULTI-PASS PIPELINE:** This function is part of the old
/// `parse_record_full → MftRecordMerger → from_parsed_records` pipeline.
/// The production hot paths now use direct-to-index parsers
/// (`SlidingIocpInline` for live, `load_raw_to_index_direct` for files)
/// that skip this intermediate allocation. This function is still used by:
/// - Legacy read modes (`Parallel`, `Pipelined`, `PipelinedParallel`,
///   `SlidingIocp`)
/// - `DataFrame` export (`load_raw_to_dataframe_with_options`)
/// - Tests and diagnostic tools
///
/// This function handles both base records and extension records.
/// Extension records return `ParseResult::Extension` which must be
/// merged into the base record later.
///
/// # Arguments
///
/// * `data` - The raw record data (after fixup)
/// * `frs` - The File Record Segment number
///
/// # Returns
///
/// `ParseResult::Base` for base records, `ParseResult::Extension` for
/// extension records, or `ParseResult::Skip` if invalid/not in use.
#[must_use]
// 101 lines: just over limit due to P2 reparse_tag extraction; splitting would hurt readability
#[expect(
    clippy::cognitive_complexity,
    reason = "NTFS attribute dispatch is inherently complex"
)]
#[expect(
    clippy::too_many_lines,
    reason = "splitting would hurt readability of sequential NTFS parsing"
)]
pub fn parse_record_full(data: &[u8], frs: u64) -> ParseResult {
    use core::mem::size_of;

    use crate::ntfs::{
        AttributeRecordHeader, AttributeType, FileRecordSegmentHeader, file_reference_to_frs,
    };

    if data.len() < size_of::<FileRecordSegmentHeader>() {
        return ParseResult::Skip;
    }

    let Ok((header, _)) = FileRecordSegmentHeader::read_from_prefix(data) else {
        return ParseResult::Skip;
    };

    // Check if record is in use
    if !header.is_in_use() {
        return ParseResult::Skip;
    }

    // Copy the packed field to avoid unaligned reference
    let multi_sector_header = header.multi_sector_header;
    if !multi_sector_header.is_file_record() {
        return ParseResult::Skip;
    }

    // Check if this is an extension record.  On-disk → typed boundary:
    // `file_reference_to_frs` decodes the 48-bit `MFT_SEGMENT_REFERENCE`
    // back to a raw `u64`; we lift to `Frs` here so the rest of the
    // function (and every downstream consumer) handles a typed value.
    let is_extension = !header.is_base_record();
    let base_frs = if is_extension {
        crate::frs::Frs::new(file_reference_to_frs(header.base_file_record_segment))
    } else {
        crate::frs::Frs::new(frs)
    };

    // Extract sequence number and LSN from header
    let sequence_number = header.sequence_number;
    let lsn = header.log_file_sequence_number;

    // Prepare result containers
    let mut names: Vec<NameInfo> = Vec::new();
    let mut streams: Vec<StreamInfo> = Vec::new();
    let mut std_info = ExtendedStandardInfo::default();
    let mut std_info_parse = crate::parse::StdInfoParse::Absent;
    let mut primary = PrimaryNameTracker::default();
    let mut reparse_tag: u32 = 0;
    let mut reparse_size: u64 = 0; // Size of $REPARSE_POINT attribute (for junctions/symlinks)
    let mut dir_index_size: u64 = 0; // Size of $INDEX_ROOT + $INDEX_ALLOCATION with name $I30
    let mut dir_index_allocated: u64 = 0; // Allocated size of directory index

    // Parse attributes
    let mut offset = header.first_attribute_offset as usize;
    let max_offset = core::cmp::min(header.bytes_in_use as usize, data.len());

    while offset + size_of::<AttributeRecordHeader>() <= max_offset {
        let Ok((attr_header, _)) = AttributeRecordHeader::read_from_prefix(&data[offset..]) else {
            break;
        };

        if attr_header.type_code == AttributeType::End as u32 {
            break;
        }
        if attr_header.length == 0 || offset + attr_header.length as usize > max_offset {
            break;
        }

        match AttributeType::from_u32(attr_header.type_code) {
            Some(AttributeType::StandardInformation) if attr_header.is_non_resident == 0 => {
                std_info_parse = parse_standard_info_full(data, offset, &mut std_info);
            }
            Some(AttributeType::FileName) if attr_header.is_non_resident == 0 => {
                if let Some(name_info) = parse_file_name_full(data, offset, frs)
                    && name_info.namespace != 2
                {
                    // Skip DOS-only names
                    primary.update(&name_info);
                    names.push(name_info);
                }
            }
            Some(AttributeType::Data) => {
                if let Some(stream_info) =
                    parse_data_attribute_full(data, offset, &attr_header, frs)
                {
                    streams.push(stream_info);
                }
            }
            Some(AttributeType::ReparsePoint) => {
                // Parse $REPARSE_POINT to get the reparse tag and size.
                // Both resident and non-resident forms are handled:
                // - Resident: ValueLength from the attribute header
                // - Non-resident: DataSize (rare, but possible for large
                //   reparse data)
                //
                // $REPARSE_POINT is counted as a stream, which affects the
                // descendants count in tree metrics.
                let (rp_size, rp_allocated, is_resident) = if attr_header.is_non_resident == 0 {
                    // Resident reparse point (common case)
                    let value_length = u64::from(u32::from_le_bytes(
                        data.get(offset + 16..offset + 20)
                            .and_then(|b| b.try_into().ok())
                            .unwrap_or([0, 0, 0, 0]),
                    ));
                    reparse_size = value_length;

                    let value_offset = u16::from_le_bytes(
                        data.get(offset + 20..offset + 22)
                            .and_then(|b| b.try_into().ok())
                            .unwrap_or([0, 0]),
                    ) as usize;
                    let rp_offset = offset + value_offset;
                    if rp_offset + size_of::<ReparsePointHeader>() <= data.len()
                        && let Ok((rp_header, _)) =
                            ReparsePointHeader::read_from_prefix(&data[rp_offset..])
                    {
                        reparse_tag = rp_header.reparse_tag;
                    }
                    (value_length, 0_u64, true)
                } else {
                    // Non-resident reparse point (rare - large reparse data)
                    // Use DataSize from non-resident header (at offset+32, NOT
                    // 40)
                    let nr_offset = offset + 16; // After common header
                    let (data_size, alloc_size) = if nr_offset + 48 <= data.len() {
                        let ds = i64::from_le_bytes(
                            data[nr_offset + 32..nr_offset + 40]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        let alloc = i64::from_le_bytes(
                            data[nr_offset + 24..nr_offset + 32]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        (nonneg_to_u64(ds), nonneg_to_u64(alloc))
                    } else {
                        (0_u64, 0_u64)
                    };
                    reparse_size = data_size;
                    // Note: Can't easily read reparse_tag from non-resident
                    // data without reading the actual data runs.
                    (data_size, alloc_size, false)
                };

                // Count $REPARSE_POINT as a stream for descendants calculation
                // Add it as a special stream with name "$REPARSE" to
                // distinguish from $DATA Note: The size is
                // already captured in reparse_size for the record's size
                // calculation, but we need the stream for stream_count.
                streams.push(StreamInfo {
                    name: String::from("$REPARSE"),
                    size: rp_size,
                    allocated_size: rp_allocated,
                    is_sparse: false,
                    is_compressed: false,
                    is_resident,
                });
            }
            Some(
                AttributeType::IndexRoot | AttributeType::IndexAllocation | AttributeType::Bitmap,
            ) => {
                // $INDEX_ROOT and $INDEX_ALLOCATION with name $I30 contribute
                // to directory size (merged into a single stream entry).
                // NOTE: $I30:$BITMAP is EXCLUDED from directory size
                // (legacy-output parity). Non-$I30 indexes
                // ($SDH, $SII, $O, $Q, $R) are counted as
                // individual streams.

                // Extract attribute name
                let (is_i30, attr_name) = if attr_header.name_length > 0 {
                    let name_offset = offset + usize::from(attr_header.name_offset);
                    let name_len = usize::from(attr_header.name_length);
                    if name_offset + name_len * 2 <= data.len() {
                        let name_bytes = &data[name_offset..name_offset + name_len * 2];
                        // Check for "$I30" in UTF-16LE
                        let is_i30 =
                            attr_header.name_length == 4 && name_bytes == b"$\x00I\x003\x000\x00";
                        // Decode name for non-$I30 indexes
                        let name = if is_i30 {
                            String::new()
                        } else {
                            let name_u16: smallvec::SmallVec<[u16; 64]> = name_bytes
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|chunk| u16::from_le_bytes(*chunk))
                                .collect();
                            String::from_utf16(&name_u16).unwrap_or_default()
                        };
                        (is_i30, name)
                    } else {
                        (false, String::new())
                    }
                } else {
                    (false, String::new())
                };

                if is_i30 {
                    // Include ALL $I30 attributes uniformly (including
                    // $BITMAP). info->length +=
                    // IsNonResident ? DataSize : ValueLength for all.
                    {
                        if attr_header.is_non_resident == 0 {
                            // Resident: get size from resident header
                            let value_length = u64::from(u32::from_le_bytes(
                                data.get(offset + 16..offset + 20)
                                    .and_then(|b| b.try_into().ok())
                                    .unwrap_or([0; 4]),
                            ));
                            dir_index_size += value_length;
                            // Resident attributes have no allocated size
                        } else {
                            // Non-resident: get sizes from non-resident header
                            let nr_offset = offset + 16;
                            if nr_offset + 48 <= data.len() {
                                let allocated = i64::from_le_bytes(
                                    data[nr_offset + 24..nr_offset + 32]
                                        .try_into()
                                        .unwrap_or([0; 8]),
                                );
                                let data_size = i64::from_le_bytes(
                                    data[nr_offset + 32..nr_offset + 40]
                                        .try_into()
                                        .unwrap_or([0; 8]),
                                );
                                dir_index_size += nonneg_to_u64(data_size);
                                dir_index_allocated += nonneg_to_u64(allocated);
                            }
                        }
                    }
                } else {
                    // Non-$I30 index attribute — counted as individual streams.
                    // Examples: $SDH, $SII (in $Secure), $O, $Q (in $Quota), $R
                    // (in $Reparse) Also includes unnamed
                    // $BITMAP (e.g., in $MFT)

                    // legacy-output parity: Only primary attributes (LowestVCN
                    // == 0) count as streams. Continuation
                    // extents (LowestVCN > 0) are
                    // skipped. See ntfs_index_load.hpp:358
                    let is_primary = if attr_header.is_non_resident == 0 {
                        true // Resident attributes are always primary
                    } else {
                        let nr_offset = offset + 16;
                        if nr_offset + 8 <= data.len() {
                            let lowest_vcn = i64::from_le_bytes(
                                data[nr_offset..nr_offset + 8].try_into().unwrap_or([0; 8]),
                            );
                            lowest_vcn == 0
                        } else {
                            false // Can't verify, skip to be safe
                        }
                    };

                    if !is_primary {
                        // Skip continuation extents - they don't count as new
                        // streams
                        offset += attr_header.length as usize;
                        continue;
                    }

                    let (size, allocated_size, is_resident) = if attr_header.is_non_resident == 0 {
                        let value_length = u64::from(u32::from_le_bytes(
                            data.get(offset + 16..offset + 20)
                                .and_then(|b| b.try_into().ok())
                                .unwrap_or([0; 4]),
                        ));
                        (value_length, 0_u64, true)
                    } else {
                        let nr_offset = offset + 16;
                        if nr_offset + 48 <= data.len() {
                            let allocated = i64::from_le_bytes(
                                data[nr_offset + 24..nr_offset + 32]
                                    .try_into()
                                    .unwrap_or([0; 8]),
                            );
                            let data_size = i64::from_le_bytes(
                                data[nr_offset + 32..nr_offset + 40]
                                    .try_into()
                                    .unwrap_or([0; 8]),
                            );
                            (nonneg_to_u64(data_size), nonneg_to_u64(allocated), false)
                        } else {
                            (0_u64, 0_u64, false)
                        }
                    };
                    // Use attribute type name if no explicit name
                    let stream_name = if attr_name.is_empty() {
                        match AttributeType::from_u32(attr_header.type_code) {
                            Some(AttributeType::Bitmap) => String::from("$BITMAP"),
                            Some(AttributeType::IndexRoot) => String::from("$INDEX_ROOT"),
                            Some(AttributeType::IndexAllocation) => {
                                String::from("$INDEX_ALLOCATION")
                            }
                            _ => String::new(),
                        }
                    } else {
                        attr_name
                    };
                    streams.push(StreamInfo {
                        name: stream_name,
                        size,
                        allocated_size,
                        is_sparse: false,
                        is_compressed: false,
                        is_resident,
                    });
                }
            }
            // The following attribute types are counted as individual streams:
            // - $OBJECT_ID (0x40)
            // - $VOLUME_NAME (0x60)
            // - $VOLUME_INFORMATION (0x70)
            // - $PROPERTY_SET (0xF0)
            // - $EA (0xE0)
            // - $EA_INFORMATION (0xD0)
            // - $LOGGED_UTILITY_STREAM (0x100)
            // - And any other attribute type (catch-all)
            Some(
                AttributeType::ObjectId
                | AttributeType::VolumeName
                | AttributeType::VolumeInformation
                | AttributeType::PropertySet
                | AttributeType::Ea
                | AttributeType::EaInformation
                | AttributeType::LoggedUtilityStream
                | AttributeType::SecurityDescriptor
                | AttributeType::AttributeList,
            ) => {
                // Note: LoggedUtilityStream (0x100) and AttributeList (0x20)
                // are both counted as streams. This is critical
                // for tree metrics parity - ~60k records have
                // $ATTRIBUTE_LIST

                // Extract attribute name (if any)
                let attr_name = if attr_header.name_length > 0 {
                    let name_offset = offset + usize::from(attr_header.name_offset);
                    let name_len = usize::from(attr_header.name_length);
                    if name_offset + name_len * 2 <= data.len() {
                        let name_bytes = &data[name_offset..name_offset + name_len * 2];
                        let name_u16: smallvec::SmallVec<[u16; 64]> = name_bytes
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|chunk| u16::from_le_bytes(*chunk))
                            .collect();
                        String::from_utf16(&name_u16).unwrap_or_default()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // legacy-output parity: Only primary attributes (LowestVCN ==
                // 0) count as streams. Continuation extents
                // (LowestVCN > 0) are skipped.
                // See ntfs_index_load.hpp:358
                let is_primary = if attr_header.is_non_resident == 0 {
                    true // Resident attributes are always primary
                } else {
                    let nr_offset = offset + 16;
                    if nr_offset + 8 <= data.len() {
                        let lowest_vcn = i64::from_le_bytes(
                            data[nr_offset..nr_offset + 8].try_into().unwrap_or([0; 8]),
                        );
                        lowest_vcn == 0
                    } else {
                        false // Can't verify, skip to be safe
                    }
                };

                if !is_primary {
                    // Skip continuation extents - they don't count as new
                    // streams
                    offset += attr_header.length as usize;
                    continue;
                }

                // Get size information
                let (size, allocated_size, is_resident) = if attr_header.is_non_resident == 0 {
                    let value_length = u64::from(u32::from_le_bytes(
                        data.get(offset + 16..offset + 20)
                            .and_then(|b| b.try_into().ok())
                            .unwrap_or([0; 4]),
                    ));
                    (value_length, 0_u64, true)
                } else {
                    let nr_offset = offset + 16;
                    if nr_offset + 48 <= data.len() {
                        let allocated = i64::from_le_bytes(
                            data[nr_offset + 24..nr_offset + 32]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        let data_size = i64::from_le_bytes(
                            data[nr_offset + 32..nr_offset + 40]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        (nonneg_to_u64(data_size), nonneg_to_u64(allocated), false)
                    } else {
                        (0_u64, 0_u64, false)
                    }
                };

                // Create a stream name that identifies the attribute type
                // Note: LoggedUtilityStream (0x100) must have a synthetic name
                // to survive the named_streams filter in
                // index.rs - otherwise its size is dropped
                // while still being counted, causing a 48-byte size
                // discrepancy.
                let stream_name = if attr_name.is_empty() {
                    match AttributeType::from_u32(attr_header.type_code) {
                        Some(AttributeType::ObjectId) => String::from("$OBJECT_ID"),
                        Some(AttributeType::VolumeName) => String::from("$VOLUME_NAME"),
                        Some(AttributeType::VolumeInformation) => {
                            String::from("$VOLUME_INFORMATION")
                        }
                        Some(AttributeType::PropertySet) => String::from("$PROPERTY_SET"),
                        Some(AttributeType::Ea) => String::from("$EA"),
                        Some(AttributeType::EaInformation) => String::from("$EA_INFORMATION"),
                        Some(AttributeType::LoggedUtilityStream) => {
                            String::from("$LOGGED_UTILITY_STREAM")
                        }
                        Some(AttributeType::SecurityDescriptor) => {
                            String::from("$SECURITY_DESCRIPTOR")
                        }
                        Some(AttributeType::AttributeList) => String::from("$ATTRIBUTE_LIST"),
                        _ => String::new(),
                    }
                } else {
                    attr_name
                };

                streams.push(StreamInfo {
                    name: stream_name,
                    size,
                    allocated_size,
                    is_sparse: false,
                    is_compressed: false,
                    is_resident,
                });
            }
            // Skip known non-stream attributes silently.
            // Note: SecurityDescriptor (0x50) IS counted as a stream (catch-all
            // below).
            Some(AttributeType::StandardInformation | AttributeType::FileName) => {}
            _ => {
                // All remaining attribute types are counted as streams.
                // Any attribute type not explicitly handled above still gets
                // counted. This includes truly unknown types.
                // that from_u32() returns None for.
                let type_code = attr_header.type_code;
                debug!(
                    frs,
                    attr_type_code = type_code,
                    "Counting unknown attribute type as stream (legacy-output parity)"
                );

                // Extract attribute name (if any)
                let attr_name = if attr_header.name_length > 0 {
                    let name_offset = offset + usize::from(attr_header.name_offset);
                    let name_len = usize::from(attr_header.name_length);
                    if name_offset + name_len * 2 <= data.len() {
                        let name_bytes = &data[name_offset..name_offset + name_len * 2];
                        let name_u16: smallvec::SmallVec<[u16; 64]> = name_bytes
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|chunk| u16::from_le_bytes(*chunk))
                            .collect();
                        String::from_utf16(&name_u16).unwrap_or_default()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // legacy-output parity: Only primary attributes (LowestVCN ==
                // 0) count as streams. Continuation extents
                // (LowestVCN > 0) are skipped.
                // See ntfs_index_load.hpp:358
                let is_primary = if attr_header.is_non_resident == 0 {
                    true // Resident attributes are always primary
                } else {
                    let nr_offset = offset + 16;
                    if nr_offset + 8 <= data.len() {
                        let lowest_vcn = i64::from_le_bytes(
                            data[nr_offset..nr_offset + 8].try_into().unwrap_or([0; 8]),
                        );
                        lowest_vcn == 0
                    } else {
                        false // Can't verify, skip to be safe
                    }
                };

                if !is_primary {
                    // Skip continuation extents - they don't count as new
                    // streams
                    offset += attr_header.length as usize;
                    continue;
                }

                // Get size information
                let (size, allocated_size, is_resident) = if attr_header.is_non_resident == 0 {
                    let value_length = u64::from(u32::from_le_bytes(
                        data.get(offset + 16..offset + 20)
                            .and_then(|b| b.try_into().ok())
                            .unwrap_or([0; 4]),
                    ));
                    (value_length, 0_u64, true)
                } else {
                    let nr_offset = offset + 16;
                    if nr_offset + 48 <= data.len() {
                        let allocated = i64::from_le_bytes(
                            data[nr_offset + 24..nr_offset + 32]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        let data_size = i64::from_le_bytes(
                            data[nr_offset + 32..nr_offset + 40]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        (nonneg_to_u64(data_size), nonneg_to_u64(allocated), false)
                    } else {
                        (0_u64, 0_u64, false)
                    }
                };

                let stream_name = if attr_name.is_empty() {
                    format!("$UNKNOWN_0x{type_code:X}")
                } else {
                    attr_name
                };

                streams.push(StreamInfo {
                    name: stream_name,
                    size,
                    allocated_size,
                    is_sparse: false,
                    is_compressed: false,
                    is_resident,
                });
            }
        }
        offset += attr_header.length as usize;
    }

    // Handle extension records
    if is_extension {
        return ParseResult::Extension(ExtensionAttributes {
            base_frs,
            extension_frs: crate::frs::Frs::new(frs),
            names,
            streams,
            dir_index_size,
            dir_index_allocated,
        });
    }

    // Note: We do NOT skip records without a $FILE_NAME attribute here.
    // Some records have their $FILE_NAME attributes in extension records
    // (when the base record has an $ATTRIBUTE_LIST). These base records
    // will have their names populated during the merge step.
    // The unified parser handles this in a single pass; the two-pass parser
    // defers extension record merging to a later step.

    // Calculate primary size from default stream
    // For reparse points (junctions/symlinks), use $REPARSE_POINT size if no
    // $DATA stream
    // For directories, size includes $INDEX_ROOT + $INDEX_ALLOCATION
    let is_directory = header.is_directory();

    // For directories with $I30 index, add a stream entry so it's counted in
    // total_stream_count. The merged $I30 is counted as a stream with
    // type_name_id=0.
    // This is essential for tree metrics parity - each directory's $I30
    // contributes +1 to descendants.
    //
    // IMPORTANT: Junctions/reparse directories ALSO get the $I30 stream
    // counted. Two-channel model for reparse directories:
    //   - Channel A (propagation): ALL streams count (dir + reparse) -> parents
    //     see 2
    //   - Channel B (printed): only directory stream -> junction prints
    //     descendants=1
    // The tree metrics algorithm handles this by storing printed_desc = 1 +
    // children while returning result.treesize = total_stream_count + children
    // for propagation.
    if is_directory && dir_index_size > 0 {
        // Add $I30 as the default stream (empty name) for directories
        // This matches established behavior where $I30 is the "default" stream
        // for directories just like $DATA is the default stream for
        // files
        streams.push(StreamInfo {
            name: String::new(), // Empty name = default stream
            size: dir_index_size,
            allocated_size: dir_index_allocated,
            is_sparse: false,
            is_compressed: false,
            is_resident: false, // $INDEX_ALLOCATION is typically non-resident
        });
    }

    let (size, allocated_size) = if is_directory && dir_index_size > 0 {
        // Directory with index allocation - use index size (legacy-output
        // parity)
        (dir_index_size, dir_index_allocated)
    } else {
        streams.iter().find(|s| s.name.is_empty()).map_or_else(
            || {
                // No default $DATA stream — use reparse_size for
                // junctions/symlinks (resident ValueLength from
                // the $REPARSE_POINT attribute)
                if reparse_tag != 0 {
                    (reparse_size, 0) // Reparse point data is resident,
                // allocated=0
                } else {
                    (0, 0)
                }
            },
            |s| (s.size, s.allocated_size),
        )
    };

    ParseResult::Base(ParsedRecord {
        frs: crate::frs::Frs::new(frs),
        sequence_number,
        lsn,
        parent_frs: primary.parent_frs,
        name: primary.name,
        namespace: primary.namespace,
        names,
        streams,
        size,
        allocated_size,
        std_info,
        in_use: true,
        is_directory,
        fn_created: primary.fn_created,
        fn_modified: primary.fn_modified,
        fn_accessed: primary.fn_accessed,
        fn_mft_changed: primary.fn_mft_changed,
        reparse_tag,
        std_info_parse,
        // P3 forensic fields (not populated in normal mode)
        is_deleted: false,
        is_corrupt: false,
        is_extension: false,
        // Base records carry the documented "no base record" sentinel.
        base_frs: crate::frs::Frs::ZERO,
    })
}

/// Parses a single MFT record and returns the base record if successful.
///
/// This is a convenience wrapper around `parse_record_full` that returns only
/// base records and skips extension records.
#[must_use]
pub fn parse_record(data: &[u8], frs: u64) -> Option<ParsedRecord> {
    match parse_record_full(data, frs) {
        ParseResult::Base(record) => Some(record),
        ParseResult::Extension(_) | ParseResult::Skip => None,
    }
}
