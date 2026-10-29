// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Parallel direct-to-index reader path.

use super::prelude::*;

/// Maximum I/O size for direct chunk-to-I/O mapping (NVMe/SSD path).
const MAX_DIRECT_IO_SIZE: usize = 16 * 1024 * 1024;

/// Single I/O operation queued for the IOCP producer thread.
struct IoOp {
    /// Absolute byte offset on the volume where the read should start.
    disk_offset: u64,
    /// Number of bytes to read for this operation.
    size: usize,
    /// FRS of the first MFT record covered by this read.
    start_frs: u64,
}

/// In-flight IOCP slot owning the OVERLAPPED struct, the read buffer, and
/// the originating I/O op.
struct InFlightOp {
    /// Win32 OVERLAPPED struct used by `ReadFile` for async I/O completion.
    overlapped: windows::Win32::System::IO::OVERLAPPED,
    /// Sector-aligned buffer holding the bytes returned by the kernel.
    buffer: AlignedBuffer,
    /// Original `IoOp` this completion corresponds to (FRS / disk-offset).
    op: IoOp,
}

/// Message sent from the IOCP producer to the parsing workers: the buffer
/// holding a fixed-size batch of FRS records, the FRS of the first record,
/// and the record count in the buffer.
type WorkerMessage = (Vec<u8>, u64, usize);

/// Bounded channel used between the IOCP producer and parsing workers.
type WorkerChannel = (
    crossbeam_channel::Sender<Option<WorkerMessage>>,
    crossbeam_channel::Receiver<Option<WorkerMessage>>,
);

impl ParallelMftReader {
    /// Reads all MFT records using the legacy port parsing algorithm.
    ///
    /// This method uses `CppParsePipeline` which is a 100% faithful port of the
    /// single-pass parsing algorithm. It processes chunks using the two-phase
    /// pipeline:
    /// - Phase 1: `preload_concurrent()` - USA fixup, max FRS discovery (NO
    ///   LOCK)
    /// - Phase 2: `load()` - Serialized attribute parsing (WITH LOCK)
    ///
    /// Reads all MFT records using parallel parsing (M3 optimization).
    ///
    /// This method uses a producer-consumer pattern:
    /// - IOCP thread reads data and sends buffers to a channel
    /// - Worker threads parse buffers using `parse_record_full()` (unified
    ///   pipeline)
    /// - After all I/O completes, results are merged via `MftRecordMerger` into
    ///   final index
    ///
    /// This is beneficial for `NVMe` drives where I/O is faster than parsing.
    /// For HDD, use `read_all_sliding_window_iocp_to_index` (inline parsing).
    ///
    /// # Arguments
    ///
    /// * `overlapped_handle` - Windows file handle opened with OVERLAPPED flag
    /// * `volume` - Volume letter (e.g., 'C')
    /// * `concurrency` - Number of I/O ops in flight (None = auto based on
    ///   drive)
    /// * `io_chunk_size` - Size of each I/O in bytes (None = auto based on
    ///   drive)
    /// * `num_workers` - Number of parsing worker threads (None = `num_cpus`)
    /// * `_progress_callback` - Optional progress callback
    #[expect(
        unsafe_code,
        reason = "FFI: ReadFile, GetQueuedCompletionStatus for parallel IOCP reads"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "parallel I/O orchestration with worker threads requires sequential setup"
    )]
    #[expect(
        clippy::cognitive_complexity,
        reason = "parallel sliding-window IOCP-to-index reader: per-completion dispatch, parse-worker fan-out, deadline tracking, and replacement-read issuance must share one event loop to keep IOCP fairness; extracting helpers would either inline the same control flow or hide IO-completion invariants"
    )]
    /// # Errors
    ///
    /// Returns [`MftError::Io`] if IOCP setup, `ReadFile`, or completion wait
    /// fails in either the I/O or parse worker pool.
    pub fn read_all_sliding_window_iocp_to_index_parallel<F>(
        &self,
        overlapped_handle: HANDLE,
        volume: crate::platform::DriveLetter,
        concurrency: Option<usize>,
        io_chunk_size: Option<usize>,
        num_workers: Option<usize>,
        _progress_callback: Option<F>,
    ) -> Result<crate::index::MftIndex>
    where
        F: Fn(u64, u64),
    {
        use alloc::collections::VecDeque;
        use alloc::sync::Arc;
        use core::pin::Pin;
        use core::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        use crossbeam_channel::bounded;
        use windows::Win32::Foundation::{ERROR_IO_PENDING, GetLastError};

        use crate::platform::{
            IOCP_WAIT_COMPLETION_DEADLINE, IOCP_WAIT_POLL_INTERVAL_MS, WAIT_TIMEOUT_ERROR_CODE,
            classify_wait_error_code, wait_deadline_exceeded,
        };

        /// Operation name carried on a wait failure from this reader.
        const WAIT_OPERATION: &str = "read_all_sliding_window_iocp_to_index_parallel";
        use windows::Win32::Storage::FileSystem::ReadFile;
        use windows::Win32::System::IO::GetQueuedCompletionStatus;

        use crate::index::MftIndex;

        let record_size = u32_as_usize(self.extent_map.bytes_per_record);
        let total_records = frs_to_usize(self.extent_map.total_records());

        // Use provided values or adaptive defaults
        // For HDD, use extent-aware concurrency (fragmentation affects optimal
        // value)
        #[expect(
            clippy::shadow_reuse,
            reason = "idiomatic Option::unwrap_or_else override-resolution: the \
                      post-unwrap usize logically replaces the Option parameter \
                      for the remainder of this function; renaming would cascade \
                      through many downstream uses without improving semantics."
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
            reason = "same rationale as `concurrency` — Option default resolution."
        )]
        let io_chunk_size = io_chunk_size.unwrap_or_else(|| self.drive_type.optimal_io_size());
        #[expect(
            clippy::shadow_reuse,
            reason = "same rationale as `concurrency` — Option default resolution \
                      using `available_parallelism` with a fixed fallback of 4."
        )]
        let num_workers = num_workers.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, core::num::NonZero::get)
        });

        info!(
            total_records,
            concurrency,
            io_size_kb = io_chunk_size / 1024,
            num_workers,
            drive_type = ?self.drive_type,
            "🚀 Starting PARALLEL parsing IOCP (M3 optimization)"
        );

        // Generate read chunks with bitmap skip optimization
        // For NVMe/SSD, use precise chunk generation to skip unused regions
        // entirely
        let use_direct_chunk_io = matches!(
            self.drive_type,
            crate::platform::DriveType::Nvme | crate::platform::DriveType::Ssd
        );

        // For NVMe/SSD: use larger max to allow direct chunk-to-I/O mapping
        // For HDD: use standard io_chunk_size for predictable sequential reads
        let sorted_chunks: Vec<ReadChunk> = if let (
            crate::platform::DriveType::Nvme | crate::platform::DriveType::Ssd,
            Some(bitmap),
        ) = (&self.drive_type, &self.bitmap)
        {
            // NVMe/SSD: Use precise chunks that skip unused regions
            // min_gap_records=64 means gaps smaller than 64KB are read through
            // Use MAX_DIRECT_IO_SIZE as the max chunk size for direct I/O
            let mut chunks =
                generate_precise_read_chunks(&self.extent_map, bitmap, MAX_DIRECT_IO_SIZE, 64);
            chunks.sort_by_key(|chunk| chunk.disk_offset);
            chunks
        } else {
            // HDD or no bitmap: Use standard chunk generation
            let mut chunks =
                generate_read_chunks(&self.extent_map, self.bitmap.as_ref(), self.chunk_size);
            chunks.sort_by_key(|chunk| chunk.disk_offset);
            chunks
        };

        // Build I/O operations with FRS tracking
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
        let estimated_records = self
            .bitmap
            .as_ref()
            .map_or(total_records, crate::platform::MftBitmap::count_in_use);

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
            bytes_to_read_mb = total_bytes_to_read / (1024 * 1024),
            max_io_size_kb = max_io_size / 1024,
            direct_io = use_direct_chunk_io,
            "📊 Generated I/O operations for parallel parsing"
        );

        // Create channel for buffer handoff (bounded to prevent memory
        // explosion) Each message contains: (buffer_data, start_frs,
        // record_count)
        let channel_capacity = num_workers * 2;
        let (tx, rx): WorkerChannel = bounded(channel_capacity);

        // Shared counter for parsed records
        let records_parsed = Arc::new(AtomicUsize::new(0));

        // Clone bitmap for workers
        let bitmap_arc = self.bitmap.clone().map(Arc::new);

        // Spawn worker threads
        let mut worker_handles = Vec::with_capacity(num_workers);
        let records_per_worker = (estimated_records / num_workers) + 1;

        for worker_id in 0..num_workers {
            let worker_rx = rx.clone();
            let worker_bitmap = bitmap_arc.clone();
            let worker_records_parsed = Arc::clone(&records_parsed);
            let worker_record_size = record_size;

            let handle = std::thread::spawn(move || {
                let mut results: Vec<ParseResult> = Vec::with_capacity(records_per_worker);
                let mut local_parsed = 0_usize;

                // Process buffers until channel closes
                // Use `mut buffer` to apply fixup in-place (zero-copy
                // optimization)
                while let Ok(Some((mut buffer, start_frs, record_count))) = worker_rx.recv() {
                    for i in 0..record_count {
                        let frs = start_frs + usize_to_u64(i);

                        // Check bitmap
                        if let Some(bm) = &worker_bitmap
                            && !bm.is_record_in_use(frs)
                        {
                            continue;
                        }

                        let offset = i * worker_record_size;
                        let end = offset + worker_record_size;

                        // Apply fixup in-place (zero-copy - no per-record
                        // allocation!)
                        let Some(record_slice) = buffer.get_mut(offset..end) else {
                            break;
                        };
                        if !apply_fixup(record_slice) {
                            continue;
                        }

                        // Parse using unified pipeline
                        let result = parse_record_full(record_slice, frs);
                        if !matches!(result, ParseResult::Skip) {
                            local_parsed += 1;
                            results.push(result);
                        }
                    }
                }

                worker_records_parsed.fetch_add(local_parsed, Ordering::Relaxed);

                tracing::debug!(
                    worker_id,
                    local_parsed,
                    parse_results = results.len(),
                    "Worker complete"
                );

                results
            });

            worker_handles.push(handle);
        }

        // Drop the receiver clone so workers can detect channel close
        drop(rx);

        // IOCP reading (producer)
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

        // Queue initial reads
        for slot_id in 0..concurrency {
            if let Some(op) = io_ops.pop_front() {
                let Some(buffer) = buffer_pool.pop() else {
                    drop(tx);
                    return Err(MftError::InvalidData(
                        "I/O buffer pool exhausted while queuing worker reads".to_owned(),
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
                    // Unreachable: buffer was sized to ≥ read_size at
                    // allocation.
                    drop(tx);
                    return Err(MftError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "to-index-parallel buffer shorter than read_size",
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
                        // Signal workers to stop
                        drop(tx);
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

        // Process completions and send to workers
        // Stall clock for the bounded wait below: reset on every real
        // completion, so the deadline measures "no progress at all",
        // never "this read is slow".
        let mut last_completion_at = Instant::now();
        while completed_count < total_io_ops {
            let mut bytes_transferred: u32 = 0;
            let mut completion_key: usize = 0;
            let mut overlapped_ptr: *mut windows::Win32::System::IO::OVERLAPPED =
                core::ptr::null_mut();

            // SAFETY: `iocp.raw_handle()` is a live completion port and all
            // out-pointers reference writable stack storage for the
            // duration of the wait.
            let result = unsafe {
                GetQueuedCompletionStatus(
                    iocp.raw_handle(),
                    &raw mut bytes_transferred,
                    &raw mut completion_key,
                    &raw mut overlapped_ptr,
                    // Poll, never INFINITE. An infinite wait here could not be
                    // distinguished from a healthy slow read, and the error
                    // branch below used to `continue` WITHOUT clearing or
                    // re-issuing the failed slot: the in-flight count fell to
                    // zero while `completed_count < total_io_ops` stayed true,
                    // so the next wait parked forever on a port with no
                    // pending I/O — zero CPU, zero reads, no error, no end.
                    IOCP_WAIT_POLL_INTERVAL_MS,
                )
            };

            if result.is_err() {
                // SAFETY: `GetLastError` reads the calling thread's last-error
                // slot and does not dereference any Rust pointers.
                let last_error = unsafe { GetLastError() };
                if last_error.0 == WAIT_TIMEOUT_ERROR_CODE {
                    // Nothing completed in this poll window. That is normal;
                    // only a total absence of progress is fatal.
                    let stalled_for = last_completion_at.elapsed();
                    if stalled_for >= IOCP_WAIT_COMPLETION_DEADLINE {
                        return Err(wait_deadline_exceeded(
                            WAIT_OPERATION,
                            stalled_for,
                            format!(
                                "GetQueuedCompletionStatus observed no IOCP completions after {completed_count} of {total_io_ops} reads"
                            ),
                        ));
                    }
                    continue;
                }

                // A real wait failure. Returning is the whole point: the old
                // `continue` turned a recoverable error into a permanent hang.
                return Err(classify_wait_error_code(
                    WAIT_OPERATION,
                    last_error.0,
                    format!(
                        "GetQueuedCompletionStatus failed after {completed_count} of {total_io_ops} reads completed"
                    ),
                ));
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

                // Send buffer to workers (copy the data)
                let Some(buffer_slice) = op_mut
                    .buffer
                    .as_slice()
                    .get(..u32_as_usize(bytes_transferred))
                else {
                    // Unreachable: bytes_transferred ≤ allocated buffer size.
                    drop(tx);
                    return Err(MftError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "to-index-parallel completion reported more bytes than buffer size",
                    )));
                };
                let buffer_data = buffer_slice.to_vec();
                let start_frs = op_mut.op.start_frs;
                let record_count = u32_as_usize(bytes_transferred) / record_size;

                if tx
                    .send(Some((buffer_data, start_frs, record_count)))
                    .is_err()
                {
                    warn!("Failed to send buffer to workers - channel closed");
                }

                bytes_read_total += u64::from(bytes_transferred);
                completed_count += 1;

                // Recycle buffer and queue next read
                let recycled_buffer = core::mem::replace(&mut op_mut.buffer, AlignedBuffer::new(0));
                buffer_pool.push(recycled_buffer);

                if let Some(next_op) = io_ops.pop_front() {
                    let Some(buffer) = buffer_pool.pop() else {
                        drop(tx);
                        return Err(MftError::InvalidData(
                            "I/O buffer pool exhausted while recycling worker reads".to_owned(),
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
                        drop(tx);
                        return Err(MftError::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "to-index-parallel recycled buffer shorter than read_size",
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

        let read_ms = read_start.elapsed().as_millis();
        info!(
            read_ms,
            bytes_mb = bytes_read_total / (1024 * 1024),
            "✅ IOCP read complete, waiting for workers"
        );

        // Signal workers to stop (send None to each); drop the must-use
        // `Result` since worker shutdown is best-effort.
        for _ in 0..num_workers {
            drop(tx.send(None));
        }
        drop(tx);

        // Collect parse results from workers and merge using unified pipeline
        let merge_start = Instant::now();
        let mut merger = MftRecordMerger::with_capacity(total_records);

        for handle in worker_handles {
            match handle.join() {
                Ok(results) => {
                    for result in results {
                        merger.add_result(result);
                    }
                }
                Err(err) => {
                    warn!("Worker thread panicked: {:?}", err);
                }
            }
        }

        let total_parsed = records_parsed.load(Ordering::Relaxed);

        // Build index from merged records
        let parsed_records = merger.merge();
        let index = MftIndex::from_parsed_records(volume, parsed_records);

        let merge_ms = merge_start.elapsed().as_millis();
        let total_ms = read_start.elapsed().as_millis();

        info!(
            total_ms,
            read_ms,
            merge_ms,
            bytes_mb = bytes_read_total / (1024 * 1024),
            records_parsed = total_parsed,
            index_entries = index.records.len(),
            names_kb = index.names.len() / 1024,
            "✅ Parallel parsing IOCP with unified pipeline complete"
        );

        Ok(index)
    }
}
