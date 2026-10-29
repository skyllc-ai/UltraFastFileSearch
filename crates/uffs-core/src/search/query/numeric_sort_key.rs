// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Numeric sort-key extraction for the top-N collector.
//!
//! Split out of `numeric_top_n.rs` so that module stays under the
//! 800-LOC file-size policy; the ~100-line `match` over every `FieldId`
//! is the bulk of it and is cohesive on its own.

use super::super::derived::bulkiness_for_record;
use super::super::field::FieldId;
use crate::compact::{CompactRecord, DriveCompactIndex};

/// Extract the numeric sort key for `rec` under `sort_column`.
///
/// Pure function of the record + column — no shared mutable state —
/// so it is trivially `Sync` and safe to call from every rayon worker
/// inside the per-drive scan.  Moved out of the scan closure so the
/// drive loop can be parallelised without duplicating the 100-line
/// `match` across each branch.
pub(super) fn extract_sort_key(
    rec: &CompactRecord,
    sort_column: FieldId,
    drive: &DriveCompactIndex,
) -> i64 {
    let drive_fold = drive.fold;
    // All `u64 -> i64` conversions below use `u64::cast_signed` (stable
    // since Rust 1.87) to document the exact-bit-pattern reinterpret
    // without needing a `cast_possible_wrap` expect.  Real NTFS file /
    // tree sizes never approach `i64::MAX`, so the high-bit flip is
    // unreachable in practice.
    match sort_column {
        FieldId::Size => rec.size.cast_signed(),
        FieldId::SizeOnDisk => rec.allocated.cast_signed(),
        FieldId::Created => rec.created,
        FieldId::Accessed => rec.accessed,
        FieldId::Descendants => i64::from(rec.descendants),
        FieldId::TreeAllocated => {
            if rec.is_directory() {
                rec.tree_allocated.cast_signed()
            } else {
                rec.allocated.cast_signed()
            }
        }
        FieldId::Bulkiness => bulkiness_for_record(rec).cast_signed(),
        FieldId::Extension | FieldId::Type => i64::from(rec.extension_id),
        FieldId::Name => {
            let name = rec.name(&drive.names);
            let mut key = [0_u8; 8];
            for (dst, ch) in key.iter_mut().zip(name.chars()) {
                let folded = drive_fold.fold_char(ch);
                // Sort-key prefix: the low byte of the folded u16 is the
                // canonical 8-byte name-prefix; `to_be_bytes()[1]` is the
                // lint-free way to take it (vs `folded as u8` which would
                // trigger `clippy::cast_possible_truncation`).
                *dst = folded.to_be_bytes()[1];
            }
            i64::from_be_bytes(key)
        }
        FieldId::Drive => {
            let name = rec.name(&drive.names);
            let mut key = [0_u8; 8];
            key[0] = u8::try_from(u32::from(drive.letter.as_byte())).unwrap_or(b'?');
            for (dst, ch) in key[1..].iter_mut().zip(name.chars()) {
                let folded = drive_fold.fold_char(ch);
                *dst = folded.to_be_bytes()[1];
            }
            i64::from_be_bytes(key)
        }
        FieldId::TreeSize => {
            if rec.is_directory() {
                rec.treesize.cast_signed()
            } else {
                rec.size.cast_signed()
            }
        }
        // Boolean attribute flags: extract the individual bit as 0/1.
        FieldId::DirectoryFlag => i64::from(rec.is_directory()),
        FieldId::Hidden => i64::from(rec.flags & 0x0002 != 0),
        FieldId::System => i64::from(rec.flags & 0x0004 != 0),
        FieldId::ReadOnly => i64::from(rec.flags & 0x0001 != 0),
        FieldId::Archive => i64::from(rec.flags & 0x0020 != 0),
        FieldId::Compressed => i64::from(rec.flags & 0x0800 != 0),
        FieldId::Encrypted => i64::from(rec.flags & 0x4000 != 0),
        FieldId::Sparse => i64::from(rec.flags & 0x0200 != 0),
        FieldId::Reparse => i64::from(rec.flags & 0x0400 != 0),
        FieldId::Offline => i64::from(rec.flags & 0x1000 != 0),
        FieldId::NotIndexed => i64::from(rec.flags & 0x2000 != 0),
        FieldId::Temporary => i64::from(rec.flags & 0x0100 != 0),
        FieldId::Integrity => i64::from(rec.flags & 0x8000 != 0),
        FieldId::NoScrub => i64::from(rec.flags & 0x0002_0000 != 0),
        FieldId::Pinned => i64::from(rec.flags & 0x0008_0000 != 0),
        FieldId::Unpinned => i64::from(rec.flags & 0x0010_0000 != 0),
        FieldId::RecallOnOpen => i64::from(rec.flags & 0x0004_0000 != 0),
        FieldId::RecallOnDataAccess => i64::from(rec.flags & 0x0040_0000 != 0),
        // Composite attribute fields use the raw flags value.
        FieldId::Attributes | FieldId::AttributeValue | FieldId::ParityAttributes => {
            i64::from(rec.flags)
        }
        FieldId::Virtual => i64::from(rec.flags & 0x0001_0000 != 0),
        // WI-4.4: leaf-name malformity as 0/1, from the lossless bytes (matches
        // the hot-path filter). `MalformedPath` needs the resolved parent chain
        // (unavailable at this sort-key stage) and `NameHex` is not sortable,
        // so both fall through to the `Modified` proxy below.
        FieldId::Malformed => {
            i64::from(core::str::from_utf8(rec.name_bytes(&drive.names)).is_err())
        }
        // Modified is the default; Path/PathOnly handled by tree walk upstream.
        FieldId::Path
        | FieldId::PathOnly
        | FieldId::Modified
        | FieldId::MalformedPath
        | FieldId::NameHex => rec.modified,
        FieldId::NameLength => {
            i64::try_from(rec.name(&drive.names).chars().count()).unwrap_or(i64::MAX)
        }
        FieldId::PathLength => {
            // Use name length as a proxy at the sort-key stage
            // (full path unavailable here).
            i64::try_from(rec.name(&drive.names).chars().count()).unwrap_or(i64::MAX)
        }
    }
}
