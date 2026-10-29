// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Live-volume NTFS metafile readers (Windows).
//!
//! Reads the reserved `$`-files straight off a live volume via the broker-safe
//! `read_handle_at` primitive: `$Boot`, non-resident `$DATA` streams (including
//! the named `$SDS` / `$J` streams that overflow into `$ATTRIBUTE_LIST`
//! extension records), fixed-up MFT records, and `$Extend` directory traversal
//! for `$UsnJrnl`. The persisted on-disk format and the offline decoders live
//! in [`crate::platform::metafile`] and [`crate::platform::metafile_decode`].

use super::metafile::MetafileKind;
use crate::error::{MftError, Result};
use crate::platform::DriveLetter;

/// `$Boot` payload size: the boot region is 16 sectors × 512 bytes.
#[cfg(windows)]
const BOOT_BYTES: usize = 8192;

/// Read a metafile's raw bytes from a live NTFS volume.
///
/// # Errors
///
/// Returns [`MftError::Io`] / [`MftError::Windows`] if opening the volume or
/// reading fails.
#[cfg(windows)]
pub fn read_metafile(drive: DriveLetter, kind: MetafileKind) -> Result<Vec<u8>> {
    let handle = crate::platform::VolumeHandle::open(drive)?;
    let vol = handle.volume_data();
    match kind {
        // `$Boot` is fixed at LCN 0; read it directly (no data-run parse).
        MetafileKind::Boot => read_boot(&handle),
        // `$Bitmap` is a non-resident unnamed `$DATA` stream.
        MetafileKind::Bitmap => read_data_stream(&handle, vol, 6, None),
        // `$Secure:$SDS` holds deduplicated security descriptors (ACLs).
        MetafileKind::Secure => read_data_stream(&handle, vol, 9, Some("$SDS")),
        // `$AttrDef` / `$MFTMirr` / `$LogFile` are unnamed non-resident `$DATA`
        // streams.
        MetafileKind::AttrDef => read_data_stream(&handle, vol, 4, None),
        MetafileKind::MftMirr => read_data_stream(&handle, vol, 1, None),
        MetafileKind::LogFile => read_data_stream(&handle, vol, 2, None),
        // `$Volume` / `$BadClus` keep their useful data in the MFT record
        // itself (the $VOLUME_* attributes; the $Bad run list), so capture the
        // fixed-up record.
        MetafileKind::Volume => read_frs_record(&handle, vol, 3),
        MetafileKind::BadClus => read_frs_record(&handle, vol, 8),
        // `$UsnJrnl:$J` lives under `$Extend`; resolve its FRS then read `$J`.
        MetafileKind::UsnJrnl => read_usn_journal(&handle, vol),
    }
}

/// Read a metafile's raw bytes (non-Windows stub).
///
/// # Errors
///
/// Always returns [`MftError::PlatformNotSupported`].
#[cfg(not(windows))]
pub const fn read_metafile(_drive: DriveLetter, _kind: MetafileKind) -> Result<Vec<u8>> {
    Err(MftError::PlatformNotSupported)
}

/// `$Boot` is the volume boot region: 8 KiB starting at LCN 0 (byte offset 0).
#[cfg(windows)]
fn read_boot(handle: &crate::platform::VolumeHandle) -> Result<Vec<u8>> {
    let mut buf = vec![0_u8; BOOT_BYTES];
    super::volume::read_handle_at(handle.raw_handle(), 0, &mut buf)?;
    Ok(buf)
}

/// Read a raw MFT file-record segment (FRS) with USA fixup applied.
#[cfg(windows)]
fn read_frs_record(
    handle: &crate::platform::VolumeHandle,
    vol: &crate::platform::NtfsVolumeData,
    frs: u64,
) -> Result<Vec<u8>> {
    let record_size = vol.bytes_per_file_record_segment as usize;
    let offset = handle.mft_byte_offset() + frs * u64::from(vol.bytes_per_file_record_segment);
    let mut record = vec![0_u8; record_size];
    super::volume::read_handle_at(handle.raw_handle(), offset, &mut record)?;
    crate::parse::apply_fixup(&mut record);
    Ok(record)
}

/// Locate a non-resident `$DATA` attribute in one MFT record — the unnamed
/// stream when `want_name` is `None`, or the named stream otherwise — returning
/// its data runs and logical size.
#[cfg(windows)]
fn find_data_attr(
    record: &[u8],
    want_name: Option<&[u16]>,
) -> Option<(Vec<crate::ntfs::DataRun>, u64)> {
    use crate::ntfs::{AttributeIterator, AttributeType};

    let mut attrs = AttributeIterator::new(record)?;
    let attr = attrs.find(|attr| {
        attr.attribute_type() == Some(AttributeType::Data)
            && attr.is_non_resident()
            && want_name.map_or(attr.header.name_length == 0, |want| {
                attr.name().as_deref() == Some(want)
            })
    })?;
    let non_resident = attr.non_resident_data()?;
    Some((attr.data_runs(), non_resident.data_size.cast_unsigned()))
}

/// Follow `$ATTRIBUTE_LIST` from a base record to the extension record(s) that
/// hold the named `$DATA` stream, returning its merged runs + logical size.
///
/// NTFS relocates attributes to extension records when a base record overflows,
/// which is how `$Secure:$SDS` and `$UsnJrnl:$J` are stored. Runs are
/// concatenated in `$ATTRIBUTE_LIST` (VCN) order; the real `data_size` lives in
/// the first (VCN-0) instance and later instances report `0`.
#[cfg(windows)]
fn find_data_in_extensions(
    handle: &crate::platform::VolumeHandle,
    vol: &crate::platform::NtfsVolumeData,
    base_record: &[u8],
    stream_name: &str,
) -> Result<Option<(Vec<crate::ntfs::DataRun>, u64)>> {
    use crate::ntfs::{AttributeIterator, AttributeType};

    // Materialize the $ATTRIBUTE_LIST payload (resident or non-resident).
    let list = {
        let mut attrs = AttributeIterator::new(base_record)
            .ok_or_else(|| MftError::InvalidData("record: invalid header".to_owned()))?;
        let Some(list_attr) =
            attrs.find(|attr| attr.attribute_type() == Some(AttributeType::AttributeList))
        else {
            return Ok(None);
        };
        if let Some(resident) = list_attr.resident_value() {
            resident.to_vec()
        } else if let Some(non_resident) = list_attr.non_resident_data() {
            read_runs(
                handle.raw_handle(),
                &list_attr.data_runs(),
                vol.bytes_per_cluster,
                non_resident.data_size.cast_unsigned(),
            )?
        } else {
            return Ok(None);
        }
    };

    let want: Vec<u16> = stream_name.encode_utf16().collect();
    let mut runs: Vec<crate::ntfs::DataRun> = Vec::new();
    let mut data_size = 0_u64;
    for ext_frs in crate::platform::metafile_decode::attribute_list_data_frs(&list, stream_name) {
        let ext = read_frs_record(handle, vol, ext_frs)?;
        if let Some((ext_runs, ext_size)) = find_data_attr(&ext, Some(&want)) {
            if data_size == 0 {
                data_size = ext_size; // VCN-0 instance carries the real size
            }
            runs.extend(ext_runs);
        }
    }
    if runs.is_empty() {
        Ok(None)
    } else {
        Ok(Some((runs, data_size)))
    }
}

/// Resolve a stream's data runs + logical size — the unnamed `$DATA` when
/// `stream_name` is `None`, or the named stream (e.g. `$SDS` / `$J`) otherwise
/// — looking in the base record first, then any `$ATTRIBUTE_LIST` extensions.
#[cfg(windows)]
fn resolve_data_runs(
    handle: &crate::platform::VolumeHandle,
    vol: &crate::platform::NtfsVolumeData,
    frs: u64,
    stream_name: Option<&str>,
) -> Result<(Vec<crate::ntfs::DataRun>, u64)> {
    let record = read_frs_record(handle, vol, frs)?;
    let want_name: Option<Vec<u16>> = stream_name.map(|name| name.encode_utf16().collect());

    // The attribute usually lives in the base record.
    if let Some(found) = find_data_attr(&record, want_name.as_deref()) {
        return Ok(found);
    }

    // Named streams ($SDS, $J) can overflow into an extension record.
    if let Some(name) = stream_name
        && let Some(found) = find_data_in_extensions(handle, vol, &record, name)?
    {
        return Ok(found);
    }

    Err(MftError::InvalidData(format!(
        "FRS {frs}: no matching non-resident DATA stream (name={stream_name:?})"
    )))
}

/// Read a metafile's non-resident `$DATA` stream — the unnamed stream when
/// `stream_name` is `None`, or the named stream (e.g. `$SDS`) otherwise — by
/// resolving its data runs and reading the referenced clusters.
#[cfg(windows)]
fn read_data_stream(
    handle: &crate::platform::VolumeHandle,
    vol: &crate::platform::NtfsVolumeData,
    frs: u64,
    stream_name: Option<&str>,
) -> Result<Vec<u8>> {
    let (runs, size) = resolve_data_runs(handle, vol, frs, stream_name)?;
    read_runs(handle.raw_handle(), &runs, vol.bytes_per_cluster, size)
}

/// Assemble a stream's data runs into a `data_size`-byte buffer. Raw volume
/// reads must be sector-aligned in length, so allocate the whole
/// cluster-aligned span the runs cover (≥ `data_size`), read each run in full,
/// then truncate to the real `data_size` (a last read clamped to a
/// non-cluster-aligned `data_size` fails with `ERROR_INVALID_PARAMETER`).
/// Sparse runs stay zeroed.
#[cfg(windows)]
fn read_runs(
    handle: windows::Win32::Foundation::HANDLE,
    runs: &[crate::ntfs::DataRun],
    bytes_per_cluster: u32,
    data_size: u64,
) -> Result<Vec<u8>> {
    let bpc = u64::from(bytes_per_cluster);
    let allocated: u64 = runs.iter().map(|run| run.cluster_count * bpc).sum();
    let capacity = usize::try_from(allocated.max(data_size)).map_err(|_err| {
        MftError::InvalidData("metafile stream size exceeds usize::MAX".to_owned())
    })?;
    let mut buf = alloc_zeroed(capacity)?;
    let mut offset: usize = 0;

    for run in runs {
        let run_bytes = usize::try_from(run.cluster_count * bpc).map_err(|_err| {
            MftError::InvalidData("metafile run byte count exceeds usize::MAX".to_owned())
        })?;
        if !run.is_sparse() {
            // Cluster-aligned offset + length → sector-aligned raw read.
            let disk_offset = crate::index::nonneg_to_u64(run.lcn.raw() * bpc.cast_signed());
            let Some(window) = buf.get_mut(offset..offset + run_bytes) else {
                return Err(MftError::InvalidData(format!(
                    "metafile run at offset {offset} len {run_bytes} exceeds buffer {capacity}"
                )));
            };
            super::volume::read_handle_at(handle, disk_offset, window)?;
        }
        offset = offset.saturating_add(run_bytes);
    }

    let final_len = usize::try_from(data_size).unwrap_or(capacity).min(capacity);
    buf.truncate(final_len);
    Ok(buf)
}

/// Allocate a zeroed buffer, failing cleanly (not aborting) when a sparse
/// stream's span is too large to materialize.
#[cfg(windows)]
fn alloc_zeroed(capacity: usize) -> Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(capacity).map_err(|_err| {
        MftError::InvalidData(format!(
            "metafile stream too large to materialize: {capacity} bytes"
        ))
    })?;
    buf.resize(capacity, 0);
    Ok(buf)
}

/// Read only the non-sparse (allocated) runs of a stream, concatenated.
///
/// `$UsnJrnl:$J` is a sparse file whose logical size can reach hundreds of GB
/// while the live journal is a tiny allocated tail (the purged prefix is a hole
/// carrying no data). So this captures just the allocated bytes — each USN
/// record is self-describing, so dropping the leading hole loses no data.
#[cfg(windows)]
fn read_runs_sparse_compact(
    handle: windows::Win32::Foundation::HANDLE,
    runs: &[crate::ntfs::DataRun],
    bytes_per_cluster: u32,
) -> Result<Vec<u8>> {
    let bpc = u64::from(bytes_per_cluster);
    let allocated: u64 = runs
        .iter()
        .filter(|run| !run.is_sparse())
        .map(|run| run.cluster_count * bpc)
        .sum();
    let capacity = usize::try_from(allocated).map_err(|_err| {
        MftError::InvalidData("metafile stream size exceeds usize::MAX".to_owned())
    })?;
    let mut buf = alloc_zeroed(capacity)?;
    let mut offset: usize = 0;
    for run in runs.iter().filter(|run| !run.is_sparse()) {
        let run_bytes = usize::try_from(run.cluster_count * bpc).map_err(|_err| {
            MftError::InvalidData("metafile run byte count exceeds usize::MAX".to_owned())
        })?;
        // Cluster-aligned offset + length → sector-aligned raw read.
        let disk_offset = crate::index::nonneg_to_u64(run.lcn.raw() * bpc.cast_signed());
        let Some(window) = buf.get_mut(offset..offset + run_bytes) else {
            return Err(MftError::InvalidData(format!(
                "metafile run at offset {offset} len {run_bytes} exceeds buffer {capacity}"
            )));
        };
        super::volume::read_handle_at(handle, disk_offset, window)?;
        offset = offset.saturating_add(run_bytes);
    }
    Ok(buf)
}

/// FRS of the `$Extend` metadata directory.
#[cfg(windows)]
const EXTEND_FRS: u64 = 11;

/// Read the `$UsnJrnl:$J` change journal via `$Extend` directory traversal.
///
/// Captures only the allocated journal data (see [`read_runs_sparse_compact`]);
/// the huge sparse prefix of `$J` is a purged hole with nothing to store.
#[cfg(windows)]
fn read_usn_journal(
    handle: &crate::platform::VolumeHandle,
    vol: &crate::platform::NtfsVolumeData,
) -> Result<Vec<u8>> {
    let usn_frs = resolve_extend_child(handle, vol, "$UsnJrnl")?;
    let (runs, _size) = resolve_data_runs(handle, vol, usn_frs, Some("$J"))?;
    read_runs_sparse_compact(handle.raw_handle(), &runs, vol.bytes_per_cluster)
}

/// Resolve the FRS of a `$Extend` (FRS 11) child by name, walking its directory
/// index — `$INDEX_ROOT` first, then `$INDEX_ALLOCATION` if the index is large.
#[cfg(windows)]
fn resolve_extend_child(
    handle: &crate::platform::VolumeHandle,
    vol: &crate::platform::NtfsVolumeData,
    child: &str,
) -> Result<u64> {
    use crate::ntfs::{AttributeIterator, AttributeType, DataRun};

    let record = read_frs_record(handle, vol, EXTEND_FRS)?;
    let target: Vec<u16> = child.encode_utf16().collect();

    let mut root_value: Option<Vec<u8>> = None;
    let mut block_size: usize = 0;
    let mut alloc: Option<(Vec<DataRun>, u64)> = None;

    let attrs = AttributeIterator::new(&record)
        .ok_or_else(|| MftError::InvalidData("$Extend: invalid record header".to_owned()))?;
    for attr in attrs {
        match attr.attribute_type() {
            Some(AttributeType::IndexRoot) => {
                if let Some(value) = attr.resident_value() {
                    // IndexRoot.bytes_per_index_block is at offset 8.
                    block_size = value
                        .get(8..12)
                        .and_then(|slice| slice.try_into().ok())
                        .map_or(0, |bytes| u32::from_le_bytes(bytes) as usize);
                    root_value = Some(value.to_vec());
                }
            }
            Some(AttributeType::IndexAllocation) if attr.is_non_resident() => {
                if let Some(nr) = attr.non_resident_data() {
                    alloc = Some((attr.data_runs(), nr.data_size.cast_unsigned()));
                }
            }
            _ => {}
        }
    }

    // 1. Search the resident $INDEX_ROOT (its INDEX_HEADER begins at offset
    //    16).
    if let Some(frs) = root_value
        .as_deref()
        .and_then(|root| scan_index_entries(root, 16, &target))
    {
        return Ok(frs);
    }

    // 2. Search the $INDEX_ALLOCATION INDX blocks, if the index spilled.
    if let Some((runs, size)) = alloc
        && block_size > 0
    {
        let buf = read_runs(handle.raw_handle(), &runs, vol.bytes_per_cluster, size)?;
        if let Some(frs) = scan_index_blocks(&buf, block_size, &target) {
            return Ok(frs);
        }
    }

    Err(MftError::InvalidData(format!(
        "$Extend index: {child} not found (no active USN journal?)"
    )))
}

/// Scan NTFS directory-index entries for the entry whose `$FILE_NAME` matches
/// `target` (UTF-16), returning its FRS. `header_start` is the byte offset of
/// the `INDEX_HEADER` within `buf` (whose `first_entry_offset` is relative to
/// it).
#[cfg(any(windows, test))]
fn scan_index_entries(buf: &[u8], header_start: usize, target: &[u16]) -> Option<u64> {
    let first_entry_offset =
        u32::from_le_bytes(buf.get(header_start..header_start + 4)?.try_into().ok()?) as usize;
    let mut pos = header_start.checked_add(first_entry_offset)?;
    loop {
        let entry = buf.get(pos..)?;
        let flags = u16::from_le_bytes(entry.get(12..14)?.try_into().ok()?);
        // Last-entry flag (0x02): end of this node.
        if (flags & 0x02) != 0 {
            return None;
        }
        let file_reference = u64::from_le_bytes(entry.get(0..8)?.try_into().ok()?);
        // The $FILE_NAME key starts at entry offset 16: name_length @0x40,
        // UTF-16 name @0x42.
        let name_length = usize::from(*entry.get(16 + 0x40)?);
        let name_bytes = entry.get(16 + 0x42..16 + 0x42 + name_length * 2)?;
        let name: Vec<u16> = name_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        if name == target {
            return Some(crate::ntfs::file_reference_to_frs(file_reference));
        }
        let entry_length = usize::from(u16::from_le_bytes(entry.get(8..10)?.try_into().ok()?));
        if entry_length == 0 {
            return None;
        }
        pos = pos.checked_add(entry_length)?;
    }
}

/// Scan the `INDX` blocks in an `$INDEX_ALLOCATION` buffer for `target`.
#[cfg(windows)]
fn scan_index_blocks(buf: &[u8], block_size: usize, target: &[u16]) -> Option<u64> {
    if block_size == 0 {
        return None;
    }
    for start in (0..buf.len()).step_by(block_size) {
        let Some(end) = start.checked_add(block_size) else {
            break;
        };
        let Some(block) = buf.get(start..end) else {
            break;
        };
        if !block.starts_with(b"INDX") {
            continue; // sparse / unused block
        }
        let mut owned = block.to_vec();
        let usa_offset = u16::from_le_bytes(owned.get(4..6)?.try_into().ok()?);
        let usa_count = u16::from_le_bytes(owned.get(6..8)?.try_into().ok()?);
        if !crate::ntfs::apply_usa_fixup(&mut owned, usa_offset, usa_count) {
            continue;
        }
        // The INDEX_HEADER begins at offset 0x18 within an INDX block.
        if let Some(frs) = scan_index_entries(&owned, 0x18, target) {
            return Some(frs);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::scan_index_entries;

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "test builds a fixed 256-byte index buffer with known in-bounds offsets"
    )]
    fn scan_index_entries_resolves_child_frs() {
        // Minimal $INDEX_ROOT-style buffer: INDEX_HEADER at offset 0
        // (first_entry_offset = 16), one entry for "$UsnJrnl" → FRS 42.
        let mut buf = vec![0_u8; 256];
        buf[0..4].copy_from_slice(&16_u32.to_le_bytes()); // first_entry_offset
        let entry = 16_usize;
        buf[entry..entry + 8].copy_from_slice(&42_u64.to_le_bytes()); // file_reference
        // flags @ entry+12 stay 0 (not the last entry).
        let name: Vec<u16> = "$UsnJrnl".encode_utf16().collect();
        let key = entry + 16;
        buf[key + 0x40] = 8; // name_length (UTF-16 code units)
        for (i, unit) in name.iter().enumerate() {
            let off = key + 0x42 + i * 2;
            buf[off..off + 2].copy_from_slice(&unit.to_le_bytes());
        }

        let target: Vec<u16> = "$UsnJrnl".encode_utf16().collect();
        assert_eq!(scan_index_entries(&buf, 0, &target), Some(42));

        // A miss: entry_length is 0 after the single entry, so the scan stops.
        let miss: Vec<u16> = "$Nope".encode_utf16().collect();
        assert_eq!(scan_index_entries(&buf, 0, &miss), None);
    }
}
