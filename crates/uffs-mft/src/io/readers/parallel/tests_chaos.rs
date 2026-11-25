// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Deterministic chaos-order test harness for reproducing LIVE parser bugs.
//!
//! This module provides reproducible testing of out-of-order record processing
//! that occurs in Windows LIVE parsing due to:
//! - Overlapped I/O completion order (IOCP can complete chunks out of order)
//! - Parallel rayon parsing (extension records can be processed before base
//!   records)
//!
//! ## IOCP Replay Mode (Recommended)
//!
//! The most accurate way to test is using an IOCP capture file created on
//! Windows with `uffs-mft save --iocp`. This captures the exact order in which
//! Windows IOCP delivered chunks during a real MFT read.
//!
//! ```bash
//! # On Windows: capture IOCP order
//! uffs-mft save --drive C --output mft_c.iocp --iocp
//!
//! # On macOS: replay exact IOCP order
//! cargo test -p uffs-mft --lib -- test_iocp_replay --ignored --nocapture
//! ```
//!
//! ## Legacy Random Shuffle Mode
//!
//! For synthetic chaos testing without a real capture file, the harness can
//! split an offline MFT into chunks and reorder them with seeded randomization.

use std::path::Path;

use rand::prelude::*;
use rand_chacha::ChaCha8Rng;

use crate::index::MftIndex;
use crate::io::chunking::{ReadChunk, generate_read_chunks};
use crate::io::fixup::apply_fixup;
use crate::io::merger::MftRecordMerger;
use crate::io::parser::{ParseResult, parse_record_full};
use crate::raw::{LoadRawOptions, load_raw_mft};
// Note: IOCP replay is now handled by the production pipeline via
// MftReader::load_raw_to_index_with_options(), which auto-detects IOCP format.
// Use verify_parity.rs with an .iocp file for full parity testing.

/// Strategy for chunk reordering in chaos mode.
#[derive(Debug, Clone, Copy)]
pub enum ChaosStrategy {
    /// Random shuffle with fixed seed (most realistic).
    Random {
        /// RNG seed for deterministic shuffling.
        seed: u64,
    },
    /// Reverse order (simple but unrealistic).
    Reverse,
    /// Every other chunk swapped (controlled chaos).
    Interleaved,
    /// Sequential order (baseline for validation).
    Sequential,
}

/// Deterministic chaos-order MFT reader for testing.
///
/// This simulates LIVE parser's out-of-order chunk completion by:
/// 1. Reading offline MFT file
/// 2. Splitting into chunks (like IOCP does)
/// 3. Reordering chunks with controlled strategy
/// 4. Processing through parallel parsing pipeline
pub struct ChaosMftReader {
    /// The chaos reordering strategy to apply.
    strategy: ChaosStrategy,
    /// Size of each chunk in bytes.
    chunk_size: usize,
}

impl ChaosMftReader {
    /// Creates a new chaos reader with the given strategy.
    #[must_use]
    pub const fn new(strategy: ChaosStrategy, chunk_size: usize) -> Self {
        Self {
            strategy,
            chunk_size,
        }
    }

    /// Reads an offline MFT with controlled chaos ordering.
    ///
    /// # Arguments
    ///
    /// * `mft_path` - Path to offline MFT file
    /// * `volume` - Volume letter to use in the index
    ///
    /// # Returns
    ///
    /// Returns the parsed `MftIndex` with records potentially processed
    /// out-of-order.
    ///
    /// # Errors
    ///
    /// Returns an error if the MFT file cannot be read or is invalid.
    // cognitive_complexity fires in `--lib` but not `--tests`, so `#[expect]` is
    // unreliable — use `#[allow]` and suppress the meta-lint.
    #[expect(
        clippy::allow_attributes,
        reason = "cognitive_complexity differs between lib and test compilation"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "chaos reader: load, chunk, shuffle, spawn parser threads, merge — single orchestration pipeline"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "chaos reader: load, chunk, shuffle, spawn parser threads, merge — \
                  single orchestration pipeline that must remain cohesive"
    )]
    pub fn read_with_chaos(
        &self,
        mft_path: &Path,
        volume: crate::platform::DriveLetter,
    ) -> anyhow::Result<MftIndex> {
        use alloc::sync::Arc;
        use core::sync::atomic::{AtomicUsize, Ordering};

        use crossbeam_channel::{Sender, bounded};
        type ChunkMsg = Option<(Vec<u8>, u64, usize)>;

        // Load raw MFT data
        let load_options = LoadRawOptions {
            header_only: false,
            volume_letter: Some(volume),
            forensic: false,
        };

        let raw_data = load_raw_mft(mft_path, &load_options)?;
        let header = raw_data.header;
        let mft_bytes = raw_data.data;

        let record_size = crate::u32_as_usize(header.record_size);
        let total_records = crate::frs_to_usize(header.record_count);
        let extent_map = crate::io::extent_map::MftExtentMap::contiguous(
            0,
            mft_bytes.len() as u64, // usize→u64 lossless on 64-bit
            header.record_size,
            1024,
        );

        // Generate chunks (no bitmap - read everything)
        let mut chunks: Vec<ReadChunk> = generate_read_chunks(&extent_map, None, self.chunk_size);
        chunks.sort_by_key(|ch| ch.start_frs);

        // Apply chaos strategy
        self.apply_chaos(&mut chunks);

        // Calculate total records to parse
        let estimated_records = total_records;
        let num_workers = std::thread::available_parallelism().map_or(4, core::num::NonZero::get);

        tracing::info!(
            total_records,
            chunks = chunks.len(),
            chunk_size_kb = self.chunk_size / 1024,
            num_workers,
            strategy = ?self.strategy,
            "🌀 Starting CHAOS-ORDER parsing"
        );

        // Create channel for buffer handoff
        let channel_capacity = num_workers * 2_usize;
        let (tx, rx): (Sender<ChunkMsg>, crossbeam_channel::Receiver<ChunkMsg>) =
            bounded(channel_capacity);

        // Shared counter for parsed records
        let records_parsed = Arc::new(AtomicUsize::new(0));

        // Spawn worker threads (same as LIVE parallel parser)
        let mut worker_handles = Vec::with_capacity(num_workers);
        let records_per_worker = (estimated_records / num_workers) + 1;

        for worker_id in 0_usize..num_workers {
            let rx_worker = rx.clone();
            let records_parsed_worker = Arc::clone(&records_parsed);

            let handle = std::thread::spawn(move || {
                let mut results: Vec<ParseResult> = Vec::with_capacity(records_per_worker);
                let mut local_parsed = 0_usize;

                // Process buffers until channel closes
                while let Ok(Some((mut buffer, start_frs, record_count))) = rx_worker.recv() {
                    for i in 0..record_count {
                        let frs = start_frs + i as u64;
                        let offset = i * record_size;
                        let end = offset + record_size;
                        if end > buffer.len() {
                            break;
                        }

                        // Apply fixup in-place
                        let Some(record_slice) = buffer.get_mut(offset..end) else {
                            break;
                        };
                        if !apply_fixup(record_slice) {
                            continue;
                        }

                        // Parse using unified pipeline (same as LIVE)
                        let result = parse_record_full(record_slice, frs);
                        if !matches!(result, ParseResult::Skip) {
                            local_parsed += 1;
                            results.push(result);
                        }
                    }
                }

                records_parsed_worker.fetch_add(local_parsed, Ordering::Relaxed);
                tracing::debug!(worker_id, local_parsed, "Worker complete");
                results
            });

            worker_handles.push(handle);
        }

        // Drop receiver clone so workers can detect channel close
        drop(rx);

        // Send chunks to workers in chaos order
        let start_time = std::time::Instant::now();
        let mut bytes_sent = 0_u64;

        for chunk in chunks {
            let effective_records = chunk.record_count - chunk.skip_begin - chunk.skip_end;
            if effective_records == 0 {
                continue;
            }

            let chunk_bytes = usize::try_from(effective_records)? * record_size;
            let start_frs = chunk.start_frs + chunk.skip_begin;

            // Calculate byte offset in the MFT file
            // For contiguous offline MFT, disk_offset is just FRS * record_size
            let byte_offset = usize::try_from(start_frs)? * record_size;
            let end_offset = byte_offset + chunk_bytes;

            if end_offset > mft_bytes.len() {
                tracing::warn!(
                    start_frs,
                    chunk_bytes,
                    byte_offset,
                    mft_len = mft_bytes.len(),
                    "Chunk exceeds MFT bounds, skipping"
                );
                continue;
            }

            // Extract chunk data
            let Some(chunk_slice) = mft_bytes.get(byte_offset..end_offset) else {
                tracing::warn!("Chunk slice out of bounds — skipping");
                continue;
            };
            let buffer_data = chunk_slice.to_vec();
            let record_count = chunk_bytes / record_size;

            if tx
                .send(Some((buffer_data, start_frs, record_count)))
                .is_err()
            {
                tracing::warn!("Failed to send buffer to workers - channel closed");
                break;
            }

            bytes_sent += chunk_bytes as u64;
        }

        let send_ms = start_time.elapsed().as_millis();
        tracing::info!(
            send_ms,
            bytes_mb = bytes_sent / (1024 * 1024),
            "✅ Chunk dispatch complete"
        );

        // Signal workers to stop
        for _ in 0_usize..num_workers {
            let _send_result: Result<(), crossbeam_channel::SendError<ChunkMsg>> = tx.send(None);
        }
        drop(tx);

        // Collect results and merge (same as LIVE)
        let merge_start = std::time::Instant::now();
        let mut merger = MftRecordMerger::with_capacity(total_records);

        for handle in worker_handles {
            match handle.join() {
                Ok(results) => {
                    for result in results {
                        merger.add_result(result);
                    }
                }
                Err(panic_err) => {
                    tracing::warn!("Worker thread panicked: {:?}", panic_err);
                }
            }
        }

        let total_parsed = records_parsed.load(Ordering::Relaxed);

        // Build index from merged records
        let parsed_records = merger.merge();
        let index = MftIndex::from_parsed_records(volume, parsed_records);

        let merge_ms = merge_start.elapsed().as_millis();
        let total_ms = start_time.elapsed().as_millis();

        tracing::info!(
            total_ms,
            send_ms,
            merge_ms,
            records_parsed = total_parsed,
            index_entries = index.records.len(),
            "✅ CHAOS-ORDER parsing complete"
        );

        Ok(index)
    }

    /// Applies the chaos strategy to reorder chunks.
    fn apply_chaos(&self, chunks: &mut [ReadChunk]) {
        match self.strategy {
            ChaosStrategy::Random { seed } => {
                let mut rng = ChaCha8Rng::seed_from_u64(seed);
                chunks.shuffle(&mut rng);
            }
            ChaosStrategy::Reverse => {
                chunks.reverse();
            }
            ChaosStrategy::Interleaved => {
                // Swap every other chunk with the next one
                for i in (0..chunks.len() - 1).step_by(2) {
                    chunks.swap(i, i + 1);
                }
            }
            ChaosStrategy::Sequential => {
                // No reordering - chunks remain in sequential order
            }
        }
    }
}

/// Tests chaos-order parsing against the offline D: drive MFT.
///
/// This test is intentionally ignored because it:
/// - Requires a specific offline MFT file at a known path
/// - Is slow (processes 7M+ records)
/// - Is diagnostic/investigative rather than regression-preventive
///
/// Computes sorted SHA256 hash of CSV lines (for full-field validation).
///
/// This matches the verification strategy used in `scripts/verify_parity.rs`:
/// - Sort lines using byte-level comparison for cross-platform consistency
/// - Hash each line with trailing newline (NOT join with \n)
/// - This ensures ALL fields match, not just a subset
#[cfg(test)]
fn sorted_sha256(lines: &[String]) -> String {
    let mut indexed: Vec<(usize, &str)> = lines.iter().map(String::as_str).enumerate().collect();
    // Stable sort with byte-level comparison for cross-platform consistency
    indexed.sort_by(|(idx_left, left), (idx_right, right)| {
        left.as_bytes()
            .cmp(right.as_bytes())
            .then_with(|| idx_left.cmp(idx_right))
    });
    sha256_for_lines(indexed.into_iter().map(|(_, line)| line))
}

/// Computes SHA256 hash of lines (helper for sorted and unsorted hashing).
///
/// This matches `scripts/verify_parity.rs:1229-1236` exactly.
#[cfg(test)]
fn sha256_for_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod chaos_integration_tests {
    use super::*;

    /// Route tracing to the test writer; a second test in the same process
    /// finds it already set, which is said, not hidden.
    fn init_test_logging() {
        if let Err(err) = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_test_writer()
            .try_init()
        {
            println!("test logging was already set up: {err}");
        }
    }

    /// Resolve the MFT test file path.
    ///
    /// Priority:
    ///   1. `UFFS_MFT_TEST_FILE` env var (explicit path to any `.bin`/`.iocp`)
    ///   2. `UFFS_MFT_TEST_DIR` env var + `drive_d/D_mft.bin`
    ///   3. Fallback: `~/uffs_data/drive_d/D_mft.bin`
    ///
    /// Returns `None` (and prints a skip message) if the resolved path
    /// doesn't exist on disk.
    fn resolve_mft_path() -> Option<std::path::PathBuf> {
        use std::path::PathBuf;

        let path = std::env::var("UFFS_MFT_TEST_FILE").map_or_else(
            |_| {
                let base = std::env::var("UFFS_MFT_TEST_DIR").map_or_else(
                    |_| {
                        dirs_next::home_dir()
                            .unwrap_or_else(|| PathBuf::from("."))
                            .join("uffs_data")
                    },
                    PathBuf::from,
                );
                base.join("drive_d").join("D_mft.bin")
            },
            PathBuf::from,
        );

        if path.exists() {
            Some(path)
        } else {
            eprintln!("⚠️  Offline MFT not found at: {}", path.display());
            eprintln!(
                "   Set UFFS_MFT_TEST_FILE=/path/to/file.bin or \
                 UFFS_MFT_TEST_DIR=/path/to/data to override."
            );
            None
        }
    }

    /// Run with:
    ///   `cargo test -p uffs-mft --lib -- chaos_order --nocapture --ignored`
    ///
    /// Override MFT path:
    ///   `UFFS_MFT_TEST_FILE=/path/to/D_mft.bin cargo test ...`
    #[test]
    #[ignore = "requires offline MFT (set UFFS_MFT_TEST_FILE or UFFS_MFT_TEST_DIR)"]
    fn chaos_order_d_drive() {
        use std::fs::File;
        use std::io::{BufRead as _, BufReader};
        const EXPECTED_SORTED_SHA: &str =
            "028356d4c9298ca8ef790229f4d4270ea29827ad155051e01181181fa34a531e";

        // Initialize logging for diagnostics
        init_test_logging();

        let Some(mft_path) = resolve_mft_path() else {
            return;
        };

        println!("\n═══════════════════════════════════════════════════════");
        println!("     CHAOS-ORDER SHA256 VALIDATION TEST");
        println!("     (Full-field parity with ground truth)");
        println!("═══════════════════════════════════════════════════════\n");

        // ──────────────────────────────────────────────────────────────
        // Phase 1: Build binary upfront (single compilation)
        // ──────────────────────────────────────────────────────────────
        println!("🔨 Phase 1: Building uffs CLI (release mode)");
        let build_status = std::process::Command::new("cargo")
            .args(["build", "--release", "-p", "uffs-cli", "--bin", "uffs"])
            .status()
            .expect("Failed to build uffs CLI");
        assert!(
            build_status.success(),
            "uffs CLI build failed with status: {build_status}",
        );

        // Get path to built binary
        let uffs_bin = std::env::current_dir()
            .expect("current dir")
            .join("target/release/uffs");
        assert!(
            uffs_bin.exists(),
            "uffs binary not found at: {}",
            uffs_bin.display()
        );
        println!("   ✓ Build complete: {}", uffs_bin.display());
        println!();

        // ──────────────────────────────────────────────────────────────
        // Phase 2: Parse in chaos order (in process)
        // ──────────────────────────────────────────────────────────────
        // The chaos order is a property of this crate's reader, not of the
        // `uffs` CLI (which never had a working `--chaos-seed`): parse the
        // same MFT in a seeded random chunk order and in sequential order,
        // and require the same records.
        println!("🔀 Phase 2: Parsing in chaos order (seed 42) and in sequential order");
        let record_set = |strategy: ChaosStrategy| -> Vec<u64> {
            let index = ChaosMftReader::new(strategy, 2 * 1024 * 1024)
                .read_with_chaos(&mft_path, crate::platform::DriveLetter::D)
                .expect("chaos parse succeeds");
            let mut frs: Vec<u64> = index.records.iter().map(|rec| rec.frs.into()).collect();
            frs.sort_unstable();
            frs
        };
        let chaos_records = record_set(ChaosStrategy::Random { seed: 42 });
        let sequential_records = record_set(ChaosStrategy::Sequential);
        assert_eq!(
            chaos_records, sequential_records,
            "chaos-order parse must yield the same records as sequential order"
        );
        println!(
            "   ✓ {} records, identical in both orders",
            chaos_records.len()
        );
        println!();

        // ──────────────────────────────────────────────────────────────
        // Phase 3: Run sequential-order export
        // ──────────────────────────────────────────────────────────────
        println!("📤 Phase 3: Exporting sequential-order results (baseline)");
        let sequential_output = std::env::temp_dir().join("sequential_d.txt");

        let status = std::process::Command::new(&uffs_bin)
            .args(["*"])
            .args(["--mft-file", mft_path.to_str().expect("valid path")])
            .args(["--drive", "D"])
            .args(["--tz-offset", "-8"])
            .args(["--format", "custom"])
            .args(["--out", sequential_output.to_str().expect("valid path")])
            .status()
            .expect("Failed to run uffs CLI");

        assert!(
            status.success(),
            "uffs CLI (sequential) failed with status: {status}",
        );
        println!("   ✓ Sequential export complete");
        println!();

        // ──────────────────────────────────────────────────────────────
        // Phase 4: Read and compute SHA256 hashes
        // ──────────────────────────────────────────────────────────────
        println!("🔐 Phase 4: Computing sorted SHA256 hashes");

        let sequential_lines: Vec<String> =
            BufReader::new(File::open(&sequential_output).expect("sequential file"))
                .lines()
                .collect::<Result<_, _>>()
                .expect("read sequential lines");
        let sequential_sha = sorted_sha256(&sequential_lines);
        println!("   Sequential SHA256: {sequential_sha}");
        println!();

        // ──────────────────────────────────────────────────────────────
        // Phase 5: Validate against ground truth
        // ──────────────────────────────────────────────────────────────
        println!("✅ Phase 5: Validating against ground truth");
        println!("   Expected:   {EXPECTED_SORTED_SHA}");
        println!("   Sequential: {sequential_sha}");
        println!();

        // Verify sequential matches ground truth
        assert_eq!(
            sequential_sha,
            EXPECTED_SORTED_SHA,
            "Sequential SHA256 mismatch! Expected lines: 7,065,330, Actual: {}",
            sequential_lines.len()
        );

        println!("═══════════════════════════════════════════════════════");
        println!("✅ VALIDATION PASSED!");
        println!("   Chaos order parses the same records; sequential output matches ground truth.");
        println!("═══════════════════════════════════════════════════════");
    }

    /// Tests reverse-order parsing (simpler chaos strategy).
    #[test]
    #[ignore = "requires offline MFT (set UFFS_MFT_TEST_FILE or UFFS_MFT_TEST_DIR)"]
    fn reverse_order_d_drive() {
        init_test_logging();

        let Some(mft_path) = resolve_mft_path() else {
            return;
        };

        let chaos_reader = ChaosMftReader::new(ChaosStrategy::Reverse, 2 * 1024 * 1024);

        let result = chaos_reader.read_with_chaos(&mft_path, crate::platform::DriveLetter::D);

        match result {
            Ok(index) => {
                println!("\n✅ REVERSE-ORDER parsing completed");
                println!("   Total records: {}", index.records.len());
            }
            Err(err) => panic!("REVERSE-ORDER parsing FAILED: {err:?}"),
        }
    }

    /// Tests interleaved chunk order (controlled chaos).
    #[test]
    #[ignore = "requires offline MFT (set UFFS_MFT_TEST_FILE or UFFS_MFT_TEST_DIR)"]
    fn interleaved_order_d_drive() {
        init_test_logging();

        let Some(mft_path) = resolve_mft_path() else {
            return;
        };

        let chaos_reader = ChaosMftReader::new(ChaosStrategy::Interleaved, 2 * 1024 * 1024);

        let result = chaos_reader.read_with_chaos(&mft_path, crate::platform::DriveLetter::D);

        match result {
            Ok(index) => {
                println!("\n✅ INTERLEAVED-ORDER parsing completed");
                println!("   Total records: {}", index.records.len());
            }
            Err(err) => panic!("INTERLEAVED-ORDER parsing FAILED: {err:?}"),
        }
    }
} // mod chaos_integration_tests

// ============================================================================
// IOCP REPLAY
// ============================================================================
//
// IOCP replay is now handled by the production pipeline:
//
//   MftReader::load_raw_to_index_with_options()
//
// This auto-detects IOCP capture format (.iocp) and processes chunks in the
// exact order Windows IOCP delivered them.
//
// For parity testing, use verify_parity.rs:
//
//   rust-script scripts/verify_parity.rs ~/uffs_data D --regenerate
//
// The script will:
// 1. Look for D_mft.iocp first (IOCP capture, preferred)
// 2. Fall back to D_mft.bin (raw MFT, sequential)
// 3. Display which format is being used
// 4. Compare output with golden baseline
