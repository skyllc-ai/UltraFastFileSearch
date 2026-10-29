// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Sliding-window direct-to-index reader path.
//!
//! Windows-only: requires IOCP and HANDLE.

#![cfg(windows)]

use super::prelude::*;

/// Offset of the `Flags` field inside `FILE_RECORD_SEGMENT_HEADER`.
const FRS_FLAGS_OFFSET: usize = 0x16;

/// `IN_USE` bit inside `FILE_RECORD_SEGMENT_HEADER.flags`.
const FRS_IN_USE_FLAG: u16 = 0x0001;

impl ParallelMftReader {
    /// Sliding window IOCP read with inline parsing directly to `MftIndex`.
    ///
    /// This is the legacy-output parity implementation that:
    /// - Parses each 1MB chunk as soon as it completes (no buffering)
    /// - Builds the index incrementally during I/O
    /// - Creates parent placeholders on-demand
    ///
    /// This eliminates the separate parse and index build phases, saving ~7s
    /// on large MFTs by overlapping CPU work with I/O.
    ///
    /// # I/O Overlap Architecture
    ///
    /// This function achieves true I/O-compute overlap using IOCP's sliding
    /// window:
    ///
    /// 1. **Multiple I/O in flight**: Maintains 2-8 concurrent I/O operations
    ///    (adaptive based on drive type). While one operation completes, others
    ///    are still reading from disk.
    ///
    /// 2. **Inline parsing**: When `GetQueuedCompletionStatus` returns, the
    ///    completion handler immediately applies fixup and parses records
    ///    directly into the index. Critically, this parse happens while other
    ///    I/O operations remain in flight.
    ///
    /// 3. **Immediate requeue**: After parsing completes, the buffer is
    ///    recycled and the next I/O operation is queued immediately,
    ///    maintaining the sliding window.
    ///
    /// Parse time per chunk is typically <1ms, so parsing on the IOCP
    /// completion thread is optimal—it avoids thread synchronization
    /// overhead and maintains cache locality. The overlap comes from having
    /// multiple chunks in flight, not from multi-threaded parsing.
    ///
    /// Timing instrumentation (added for profiling) logs `wait_ms`, `parse_ms`,
    /// and `overlap_pct` to quantify how much parse work was hidden behind
    /// I/O latency.
    ///
    /// # Arguments
    ///
    /// * `overlapped_handle` - IOCP handle for async I/O
    /// * `volume` - Volume letter (e.g., 'C')
    /// * `concurrency` - Number of I/O ops in flight (None = 2 for HDD)
    /// * `io_chunk_size` - Size of each I/O in bytes (None = 1MB)
    /// * `_progress_callback` - Optional progress callback
    #[expect(
        unsafe_code,
        reason = "FFI: ReadFile, GetQueuedCompletionStatus for IOCP-to-index reads"
    )]
    #[expect(
        clippy::float_arithmetic,
        reason = "telemetry: I/O-vs-parse overlap percentage requires float division for human-readable logging"
    )]
    #[expect(
        clippy::cognitive_complexity,
        reason = "sliding-window IOCP reader: completion dispatch, deadline tracking, inline parse, and replacement-read issuance must share one event loop to keep IOCP fairness; extracting helpers would either inline the same control flow or hide IO-completion invariants"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "sliding-window IOCP reader: completion dispatch, deadline tracking, inline parse-into-MftIndex, and replacement-read issuance must share OVERLAPPED slots, the buffer pool, and per-completion FRS / fixup state in a single event loop. Splitting into helpers would either inline the same control flow back or smear the shared mutable state across multiple call sites and obscure IOCP-fairness invariants"
    )]
    /// # Errors
    ///
    /// Returns [`MftError::Io`] when an IOCP `ReadFile` or the completion
    /// wait fails for any outstanding sliding-window request.
    pub(crate) fn read_all_sliding_window_iocp_to_index<F>(
        &self,
        overlapped_handle: HANDLE,
        volume: crate::platform::DriveLetter,
        concurrency: Option<usize>,
        io_chunk_size: Option<usize>,
        _progress_callback: Option<F>,
    ) -> Result<crate::index::MftIndex>
    where
        F: Fn(u64, u64),
    {
        use alloc::collections::VecDeque;
        use core::pin::Pin;
        use std::time::Instant;

        use windows::Win32::Foundation::{ERROR_IO_PENDING, GetLastError};
        use windows::Win32::Storage::FileSystem::ReadFile;
        use windows::Win32::System::IO::GetQueuedCompletionStatus;

        use crate::index::{MftIndex, u64_to_f64};
        use crate::platform::{
            IOCP_WAIT_COMPLETION_DEADLINE, IOCP_WAIT_POLL_INTERVAL_MS, WAIT_TIMEOUT_ERROR_CODE,
            classify_wait_error_code, wait_deadline_exceeded,
        };

        const WAIT_OPERATION: &str = "read_all_sliding_window_iocp_to_index";

        // Build I/O operations with FRS tracking for inline parsing
        struct IoOp {
            disk_offset: u64,
            size: usize,
            start_frs: u64, // First FRS in this I/O
        }

        // Sliding window state
        struct InFlightOp {
            overlapped: windows::Win32::System::IO::OVERLAPPED,
            buffer: AlignedBuffer,
            op: IoOp,
        }

        debug!("[PARITY_TRACE] to_index.rs: read_all_sliding_window_iocp_to_index ENTER");
        let record_size = u32_as_usize(self.extent_map.bytes_per_record);
        let total_records = frs_to_usize(self.extent_map.total_records());
        debug!(
            record_size,
            total_records, "[PARITY_TRACE] to_index.rs: config"
        );

        // Use provided values or adaptive defaults based on drive type
        // M1: Adaptive concurrency and I/O size based on drive type
        // For HDD, use extent-aware concurrency (fragmentation affects optimal
        // value)
        #[expect(
            clippy::shadow_reuse,
            reason = "idiomatic Option::unwrap_or_else override-resolution: the \
                      post-unwrap usize logically replaces the Option parameter \
                      for the remainder of this function; renaming to \
                      `effective_concurrency` would cascade through 9 downstream \
                      uses without improving semantics."
        )]
        let concurrency = concurrency.unwrap_or_else(|| {
            if matches!(
                self.drive_type,
                crate::platform::DriveType::Hdd
                    | crate::platform::DriveType::Removable
                    | crate::platform::DriveType::Virtual
            ) {
                crate::platform::DriveType::optimal_concurrency_for_hdd(
                    self.extent_map.extent_count(),
                )
            } else {
                self.drive_type.optimal_concurrency()
            }
        });
        #[expect(
            clippy::shadow_reuse,
            reason = "same rationale as `concurrency` above — Option default \
                      resolution for an optional config parameter."
        )]
        let io_chunk_size = io_chunk_size.unwrap_or_else(|| self.drive_type.optimal_io_size());

        info!(
            total_records,
            concurrency,
            io_size_kb = io_chunk_size / 1024,
            drive_type = ?self.drive_type,
            "🚀 Starting sliding window IOCP with INLINE parsing (adaptive settings)"
        );

        // Generate read chunks with bitmap skip optimization
        // CRITICAL: Use standard chunking for ALL drive types (bitmap is
        // advisory, not authoritative) The bitmap should be used for
        // I/O optimization (skip ranges, pre-allocation), NOT as an
        // authoritative filter for which regions to read. If the bitmap
        // is stale (common on live filesystems), treating it as
        // authoritative causes record loss. Evidence: HDD path
        // (using advisory bitmap) has only 6 missing records vs 10K+ on
        // NVMe/SSD.
        let use_direct_chunk_io = matches!(
            self.drive_type,
            crate::platform::DriveType::Nvme | crate::platform::DriveType::Ssd
        );

        let sorted_chunks: Vec<ReadChunk> = {
            let mut chunks =
                generate_read_chunks(&self.extent_map, self.bitmap.as_ref(), self.chunk_size);
            chunks.sort_by_key(|chunk| chunk.disk_offset);
            chunks
        };

        let mut io_ops: VecDeque<IoOp> = VecDeque::new();

        for chunk in &sorted_chunks {
            let skip_begin_bytes = frs_to_usize(chunk.skip_begin) * record_size;
            let effective_records = chunk.record_count - chunk.skip_begin - chunk.skip_end;
            if effective_records == 0 {
                continue;
            }

            let chunk_bytes = frs_to_usize(effective_records) * record_size;

            if use_direct_chunk_io {
                // NVMe/SSD: Use chunk directly as one I/O operation (no
                // splitting) This minimizes syscall overhead
                // since there's no seek penalty
                io_ops.push_back(IoOp {
                    disk_offset: chunk.disk_offset + usize_to_u64(skip_begin_bytes),
                    size: chunk_bytes,
                    start_frs: chunk.start_frs + chunk.skip_begin,
                });
            } else {
                // HDD: Split into io_chunk_size pieces for predictable
                // sequential reads
                let mut offset_within_chunk = 0_usize;
                let mut frs_offset = 0_u64;

                while offset_within_chunk < chunk_bytes {
                    let io_size = core::cmp::min(io_chunk_size, chunk_bytes - offset_within_chunk);
                    let records_in_io = io_size / record_size;
                    let disk_offset = chunk.disk_offset
                        + usize_to_u64(skip_begin_bytes)
                        + usize_to_u64(offset_within_chunk);

                    io_ops.push_back(IoOp {
                        disk_offset,
                        size: io_size,
                        start_frs: chunk.start_frs + chunk.skip_begin + frs_offset,
                    });

                    offset_within_chunk += io_size;
                    frs_offset += usize_to_u64(records_in_io);
                }
            }
        }

        let total_io_ops = io_ops.len();
        let (estimated_records, max_frs) = self.bitmap.as_ref().map_or_else(
            || {
                // No bitmap: use total records as both count and max FRS
                (total_records, usize_to_u64(total_records.saturating_sub(1)))
            },
            |bitmap| (bitmap.count_in_use(), bitmap.max_frs_in_use()),
        );

        // Calculate total bytes to read and max I/O size for buffer allocation
        let total_bytes_to_read: u64 = io_ops.iter().map(|op| usize_to_u64(op.size)).sum();
        let max_io_size = io_ops
            .iter()
            .map(|op| op.size)
            .max()
            .unwrap_or(io_chunk_size);

        info!(
            io_ops = total_io_ops,
            estimated_records,
            max_frs,
            bytes_to_read_mb = total_bytes_to_read / (1024 * 1024),
            max_io_size_kb = max_io_size / 1024,
            direct_io = use_direct_chunk_io,
            "📊 Generated I/O operations for inline parsing"
        );

        // Pre-allocate MftIndex with tuned ratios to eliminate resizing during
        // parse
        let mut index = MftIndex::with_capacity_optimized(volume, estimated_records, max_frs);

        // Create IOCP
        let read_start = Instant::now();
        let iocp = IoCompletionPort::new(0)?;
        iocp.associate(overlapped_handle, 0)?;

        // Allocate buffers sized for the max I/O operation
        let mut buffer_pool: Vec<AlignedBuffer> = (0..concurrency)
            .map(|_| AlignedBuffer::new(max_io_size))
            .collect();

        let mut in_flight: Vec<Option<Pin<Box<InFlightOp>>>> =
            (0..concurrency).map(|_| None).collect();

        let mut completed_count = 0_usize;
        let mut bytes_read_total = 0_u64;
        let mut records_parsed = 0_usize;

        // Queue initial reads
        for slot_id in 0..concurrency {
            if let Some(op) = io_ops.pop_front() {
                let Some(buffer) = buffer_pool.pop() else {
                    return Err(MftError::InvalidData(
                        "I/O buffer pool exhausted while queuing inline-parse reads".to_owned(),
                    ));
                };
                let mut in_flight_op = Box::pin(InFlightOp {
                    // SAFETY: `OVERLAPPED` is a plain Windows FFI struct and an
                    // all-zero value is the required initial state before
                    // offsets are set.
                    overlapped: unsafe { core::mem::zeroed() },
                    buffer,
                    op,
                });

                let offset = in_flight_op.op.disk_offset;
                // SAFETY: The pinned allocation remains in place while the I/O
                // is in flight; this only projects a mutable
                // reference without moving it.
                let op_mut = unsafe { in_flight_op.as_mut().get_unchecked_mut() };
                set_overlapped_offset(&mut op_mut.overlapped, offset);

                let overlapped_ptr = &raw mut op_mut.overlapped;
                let read_size = op_mut.op.size;
                let Some(read_slice) = op_mut.buffer.as_mut_slice().get_mut(..read_size) else {
                    // Unreachable: op.buffer was sized to ≥ read_size at
                    // allocation.
                    return Err(MftError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "to-index buffer shorter than read_size",
                    )));
                };
                // SAFETY: `overlapped_handle` is a live overlapped-capable
                // handle, the buffer slice spans `read_size`
                // writable bytes in the pinned op,
                // and `overlapped_ptr` points into that same pinned operation.
                let result = unsafe {
                    ReadFile(
                        overlapped_handle,
                        Some(read_slice),
                        None,
                        Some(overlapped_ptr),
                    )
                };

                if result.is_err() {
                    // SAFETY: `GetLastError` reads the calling thread's
                    // last-error slot and does not
                    // dereference any Rust pointers.
                    let last_error = unsafe { GetLastError() };
                    if last_error != ERROR_IO_PENDING {
                        return Err(MftError::Io(std::io::Error::from_raw_os_error(
                            last_error.0.cast_signed(),
                        )));
                    }
                }

                if let Some(slot) = in_flight.get_mut(slot_id) {
                    *slot = Some(in_flight_op);
                }
            }
        }

        // Process completions with inline parsing
        let bitmap_ref = self.bitmap.as_ref();
        let mut last_completion_at = Instant::now();
        let mut name_buf = String::with_capacity(256);

        // Timing instrumentation for I/O overlap analysis
        let mut total_wait_time_ns = 0_u64;
        let mut total_parse_time_ns = 0_u64;

        while completed_count < total_io_ops {
            let mut bytes_transferred: u32 = 0;
            let mut completion_key: usize = 0;
            let mut overlapped_ptr: *mut windows::Win32::System::IO::OVERLAPPED =
                core::ptr::null_mut();

            // Time I/O wait (GetQueuedCompletionStatus)
            let wait_start = Instant::now();

            // SAFETY: `iocp.raw_handle()` is a live completion port and all
            // out-pointers reference writable stack storage for the
            // duration of the wait.
            let result = unsafe {
                GetQueuedCompletionStatus(
                    iocp.raw_handle(),
                    &raw mut bytes_transferred,
                    &raw mut completion_key,
                    &raw mut overlapped_ptr,
                    IOCP_WAIT_POLL_INTERVAL_MS,
                )
            };

            total_wait_time_ns += nanos_to_u64(wait_start.elapsed().as_nanos());

            if result.is_err() {
                // SAFETY: `GetLastError` reads the calling thread's last-error
                // slot and does not dereference any Rust
                // pointers.
                let last_error = unsafe { GetLastError() };
                if last_error.0 == WAIT_TIMEOUT_ERROR_CODE {
                    let stalled_for = last_completion_at.elapsed();
                    if stalled_for >= IOCP_WAIT_COMPLETION_DEADLINE {
                        return Err(wait_deadline_exceeded(
                            WAIT_OPERATION,
                            stalled_for,
                            format!(
                                "GetQueuedCompletionStatus observed no inline IOCP completions after {completed_count} of {total_io_ops} reads"
                            ),
                        ));
                    }
                    continue;
                }

                return Err(classify_wait_error_code(
                    WAIT_OPERATION,
                    last_error.0,
                    format!(
                        "GetQueuedCompletionStatus failed after {completed_count} of {total_io_ops} inline IOCP reads completed"
                    ),
                ));
            }

            if overlapped_ptr.is_null() {
                return Err(MftError::WaitFailed {
                    operation: WAIT_OPERATION,
                    reason: format!(
                        "GetQueuedCompletionStatus returned a null OVERLAPPED pointer after {completed_count} of {total_io_ops} inline IOCP reads completed"
                    ),
                });
            }

            last_completion_at = Instant::now();

            // Find completed slot
            let mut completed_slot = None;
            for (idx, slot) in in_flight.iter().enumerate() {
                if let Some(op) = slot {
                    let op_overlapped_ptr = (&raw const op.overlapped).cast_mut();
                    if op_overlapped_ptr == overlapped_ptr {
                        completed_slot = Some(idx);
                        break;
                    }
                }
            }

            if let Some(slot_idx) = completed_slot
                && let Some(op_slot) = in_flight.get_mut(slot_idx)
                && let Some(mut completed_op) = op_slot.take()
            {
                // SAFETY: The `Pin<Box<_>>` is still pinned in this scope; we
                // only project a mutable reference without moving the
                // allocation.
                let op_mut = unsafe { completed_op.as_mut().get_unchecked_mut() };

                // Time parse phase (fixup + process_record)
                let parse_start = Instant::now();

                // DIRECT-TO-INDEX: parse records directly into MftIndex
                let Some(buffer_slice) = op_mut
                    .buffer
                    .as_mut_slice()
                    .get_mut(..u32_as_usize(bytes_transferred))
                else {
                    // Unreachable: completion bytes_transferred ≤ allocated
                    // buffer size.
                    return Err(MftError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "to-index completion reported more bytes than buffer size",
                    )));
                };
                let records_in_buffer = u32_as_usize(bytes_transferred) / record_size;

                for i in 0..records_in_buffer {
                    let frs = op_mut.op.start_frs + usize_to_u64(i);
                    let offset = i * record_size;

                    // Check bitmap
                    if let Some(bm) = bitmap_ref
                        && !bm.is_record_in_use(frs)
                    {
                        // Bitmap says unused, but extension records have IN_USE
                        // set in their header while NOT being marked in
                        // $Bitmap. Peek at the FILE
                        // record header flags (offset 0x16, 2 bytes LE)
                        // before skipping.
                        let Some(record_slice) = buffer_slice.get(offset..offset + record_size)
                        else {
                            break;
                        };

                        let Some(flag_bytes) = record_slice
                            .get(FRS_FLAGS_OFFSET..FRS_FLAGS_OFFSET + 2)
                            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
                        else {
                            continue;
                        };
                        let flags = u16::from_le_bytes(flag_bytes);
                        if flags & FRS_IN_USE_FLAG == 0 {
                            // Record header also says not in use — safe to skip
                            continue;
                        }
                        // Header says IN_USE — this is an extension record,
                        // process it
                    }

                    let Some(record_slice) = buffer_slice.get_mut(offset..offset + record_size)
                    else {
                        break;
                    };

                    // Apply fixup
                    if !apply_fixup(record_slice) {
                        continue;
                    }

                    // Parse directly into index using unified parser
                    if process_record(record_slice, frs, &mut index, &mut name_buf) {
                        records_parsed += 1;
                    }
                }

                total_parse_time_ns += nanos_to_u64(parse_start.elapsed().as_nanos());

                bytes_read_total += u64::from(bytes_transferred);
                completed_count += 1;

                // Recycle buffer and queue next read
                let recycled_buffer = core::mem::replace(&mut op_mut.buffer, AlignedBuffer::new(0));
                buffer_pool.push(recycled_buffer);

                if let Some(next_op) = io_ops.pop_front() {
                    let Some(buffer) = buffer_pool.pop() else {
                        return Err(MftError::InvalidData(
                            "I/O buffer pool exhausted while recycling inline-parse reads"
                                .to_owned(),
                        ));
                    };
                    let mut new_in_flight = Box::pin(InFlightOp {
                        // SAFETY: `OVERLAPPED` is a plain Windows FFI struct
                        // and an all-zero value is the required initial state
                        // before offsets are set.
                        overlapped: unsafe { core::mem::zeroed() },
                        buffer,
                        op: next_op,
                    });

                    let offset = new_in_flight.op.disk_offset;
                    // SAFETY: The pinned allocation remains in place while the
                    // I/O is in flight; this only projects
                    // a mutable reference.
                    let new_op_mut = unsafe { new_in_flight.as_mut().get_unchecked_mut() };
                    set_overlapped_offset(&mut new_op_mut.overlapped, offset);

                    let new_overlapped_ptr = &raw mut new_op_mut.overlapped;
                    let read_size = new_op_mut.op.size;
                    let Some(read_slice) = new_op_mut.buffer.as_mut_slice().get_mut(..read_size)
                    else {
                        // Unreachable: buffer was sized to ≥ read_size at
                        // allocation.
                        return Err(MftError::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "to-index recycled buffer shorter than read_size",
                        )));
                    };
                    // SAFETY: `overlapped_handle` is a live overlapped-capable
                    // handle, the buffer slice spans `read_size` writable bytes
                    // in the pinned op, and
                    // `new_overlapped_ptr` points into that op.
                    let submit_result = unsafe {
                        ReadFile(
                            overlapped_handle,
                            Some(read_slice),
                            None,
                            Some(new_overlapped_ptr),
                        )
                    };

                    if submit_result.is_err() {
                        // SAFETY: `GetLastError` reads the calling thread's
                        // last-error slot and does not dereference Rust
                        // pointers.
                        let last_error = unsafe { GetLastError() };
                        if last_error != ERROR_IO_PENDING {
                            warn!(error = ?last_error, "Failed to queue next read");
                        }
                    }

                    if let Some(slot) = in_flight.get_mut(slot_idx) {
                        *slot = Some(new_in_flight);
                    }
                }
            }
        }

        let total_ms = millis_to_u64(read_start.elapsed().as_millis());
        let wait_ms = total_wait_time_ns / 1_000_000;
        let parse_ms = total_parse_time_ns / 1_000_000;

        // Calculate overlap efficiency: if wait_ms + parse_ms > total_ms,
        // then we had effective overlap (parse happened while other I/O was in
        // flight)
        let overlap_pct = if total_ms > 0 {
            (u64_to_f64((wait_ms + parse_ms).saturating_sub(total_ms)) / u64_to_f64(total_ms))
                * 100.0_f64
        } else {
            0.0_f64
        };

        debug!(
            records_parsed,
            index_entries = index.records.len(),
            "[PARITY_TRACE] to_index.rs: I/O complete"
        );
        info!(
            total_ms,
            wait_ms,
            parse_ms,
            overlap_pct = format!("{:.1}%", overlap_pct),
            bytes_mb = bytes_read_total / (1024 * 1024),
            records_parsed,
            index_entries = index.records.len(),
            "✅ Sliding window IOCP with direct-to-index parsing complete (I/O overlap analysis)"
        );

        // Parity debug: count files with size=0 vs size>0
        if std::env::var("UFFS_PARITY_DEBUG").is_ok() {
            let mut files_with_size = 0_usize;
            let mut files_with_zero_size = 0_usize;
            let mut dirs = 0_usize;
            for record in &index.records {
                if record.stdinfo.is_directory() {
                    dirs += 1;
                } else if record.first_stream.size.length > 0 {
                    files_with_size += 1;
                } else {
                    files_with_zero_size += 1;
                }
            }
            debug!(
                total_records = index.records.len(),
                directories = dirs,
                files_with_size,
                files_with_zero_size,
                "[PARITY_DEBUG] Summary"
            );
        }

        debug!(
            records = index.records.len(),
            "[PARITY_TRACE] to_index.rs: EXIT (NO compute_tree_metrics yet)"
        );
        Ok(index)
    }
}
