// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `IndexManager` search / status / drive-letter inference tests.
//!
//! Covers:
//!
//! * Phase 3.1 NUL fast path (`include_rows = false` suppression).
//! * Zero-drive shutdown guard's reliance on `loaded_drive_letters`.
//! * Drive-letter inference + live-drive marker classification.
//! * Phase 0 telemetry: `status` populates `rss_bytes` +
//!   `mimalloc_committed_bytes`; `total_index_heap_bytes` matches the per-drive
//!   breakdown.

use alloc::sync::Arc;

use super::{IndexManager, build_test_drive};

/// Helper: construct a minimal [`IndexManager`] with the synthetic
/// test drive loaded.  Uses `tokio::test` so the async `add_drive` can
/// swap the internal snapshot pointer.
async fn test_manager_with_drive() -> IndexManager {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(build_test_drive()).await;
    mgr
}

/// Regression: when `include_rows = false`, [`IndexManager::search`]
/// must return a response with empty `rows`, empty `projected_rows`,
/// and `paths_blob = None` — while still populating `total_count` so
/// callers can display "N results suppressed" if they want to.
///
/// This is the daemon-side half of the Phase 3.1 NUL fast path: the
/// CLI injects `--no-output` when stdout points to the null device,
/// which sets this flag, and the daemon skips row materialisation +
/// `paths_blob` packing + IPC transfer.
#[tokio::test]
async fn search_with_include_rows_false_suppresses_rows_but_counts() {
    use uffs_client::protocol::response::SearchPayload;

    let mgr = test_manager_with_drive().await;

    let params = uffs_client::protocol::SearchParams {
        pattern: "*".to_owned(),
        include_rows: false,
        ..uffs_client::protocol::SearchParams::default()
    };

    let response = mgr.search(&params).await.expect("search completes");

    // `include_rows = false` must leave the payload as `Empty` —
    // any other variant (InlineRows, blob, shmem) would mean the
    // daemon allocated and populated rows despite the caller opting
    // out, which is the exact overhead the flag is meant to skip.
    assert!(
        matches!(response.payload, SearchPayload::Empty),
        "include_rows=false must produce SearchPayload::Empty; got {:?}",
        response.payload
    );
    assert!(
        response.total_count > 0,
        "total_count must reflect the matched record count regardless of include_rows; got {}",
        response.total_count
    );
}

/// Control: with `include_rows = true` (the default), the same query
/// returns non-empty rows.  Pins that the gate in
/// `IndexManager::search` does not accidentally suppress the
/// non-suppressed case.
#[tokio::test]
async fn search_with_include_rows_true_returns_rows() {
    use uffs_client::protocol::response::SearchPayload;

    let mgr = test_manager_with_drive().await;

    let params = uffs_client::protocol::SearchParams {
        pattern: "*".to_owned(),
        include_rows: true,
        ..uffs_client::protocol::SearchParams::default()
    };

    let response = mgr.search(&params).await.expect("search completes");

    // The happy path delivers `InlineRows` — the small-manager
    // fixture never breaches the `SHMEM_THRESHOLD` (100 K rows) or
    // `PATHS_BLOB_SHMEM_THRESHOLD` (512 KB) boundaries, and the `*`
    // pattern with default projection is not path-only so no
    // `InlineBlob` fast-path fires either.
    let total_count = response.total_count;
    let SearchPayload::InlineRows(rows) = response.payload else {
        panic!(
            "include_rows=true on a small fixture must deliver \
             InlineRows; got a non-rows payload variant"
        );
    };
    assert!(
        !rows.is_empty(),
        "include_rows=true must return matched rows; got 0"
    );
    assert_eq!(
        rows.len() as u64,
        total_count,
        "rows.len() must equal total_count when no limit is set and include_rows=true"
    );
}

// ── Zero-drive shutdown guard (prevents the Ready-with-no-data zombie) ──

/// Regression pin for the zero-drive guard in
/// `crate::run_daemon`'s `load_task`.  The guard keys off
/// `IndexManager::loaded_drive_letters().await.is_empty()` — if that
/// signal ever started reporting a non-empty vec for a fresh manager
/// (e.g. by accidentally seeding a placeholder drive), the guard
/// would silently stop firing and the zombie-daemon bug would
/// reappear.  This test pins the invariant the guard relies on.
///
/// The end-to-end check — that `run_daemon` actually calls
/// `request_shutdown` when every MFT parse fails — is covered by
/// `scripts/windows/api-validation.rs` which spins up a real daemon
/// with an empty `data_dir` and now observes it exit cleanly on
/// macOS/Linux instead of lingering in `Ready` with zero drives.
#[tokio::test]
async fn fresh_index_manager_reports_no_loaded_drives() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    let letters = mgr.loaded_drive_letters().await;
    assert!(
        letters.is_empty(),
        "a fresh IndexManager must report zero loaded drives — the run_daemon \
         zero-drive shutdown guard relies on this signal.  got: {letters:?}",
    );
}

// ── Pure helpers extracted from the load / refresh paths ─────────

/// `infer_drive_letter` keys MFT-snapshot file paths to a canonical
/// drive letter so the hot-load path can short-circuit when that
/// drive is already loaded.  The contract is:
///
/// * first ASCII-alphabetic character of the file stem,
/// * uppercased,
/// * `'X'` fallback for non-conforming names so callers always have a stable
///   handle to log against.
#[test]
fn infer_drive_letter_pins_canonical_mapping() {
    use std::path::Path;

    // Standard captures: `<letter>_mft.iocp`.
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("G_mft.iocp")),
        uffs_mft::platform::DriveLetter::G
    );
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("c_mft.iocp")),
        uffs_mft::platform::DriveLetter::C
    );

    // Lone letter, no extension.
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("d")),
        uffs_mft::platform::DriveLetter::D
    );

    // Path with directory components — only the file stem matters.
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("data_dir/drive_e/E_mft.iocp")),
        uffs_mft::platform::DriveLetter::E
    );

    // Non-conforming names fall back to 'X' rather than panicking.
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("1bad_name")),
        uffs_mft::platform::DriveLetter::X
    );
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("_underscore.iocp")),
        uffs_mft::platform::DriveLetter::X
    );

    // Empty path components default to 'X' (file_name() returns None).
    assert_eq!(
        IndexManager::infer_drive_letter(Path::new("")),
        uffs_mft::platform::DriveLetter::X
    );
}

/// `is_live_drive_marker` distinguishes a cached source whose
/// recorded path is the bare drive marker (e.g. `"C:"`) from a real
/// on-disk MFT snapshot.  The threshold is `len <= 2` so a stray
/// trailing backslash on Windows still counts as a real path.
#[test]
fn is_live_drive_marker_recognises_cached_volume_marker() {
    use std::path::Path;

    // The two canonical live-drive markers used by the cache layer.
    assert!(IndexManager::is_live_drive_marker(Path::new("C:")));
    assert!(IndexManager::is_live_drive_marker(Path::new("d:")));
    // Single-char shorthand also classifies as live.
    assert!(IndexManager::is_live_drive_marker(Path::new("D")));

    // Anything ≥ 3 bytes is treated as an on-disk snapshot.
    assert!(!IndexManager::is_live_drive_marker(Path::new("C:\\")));
    assert!(!IndexManager::is_live_drive_marker(Path::new(
        "C:\\snap\\C_mft.iocp"
    )));
    assert!(!IndexManager::is_live_drive_marker(Path::new(
        "./C_mft.iocp"
    )));
}

// ── Phase 0 telemetry: status() surfaces RSS + mimalloc committed ──

/// `IndexManager::status` populates both `rss_bytes` and
/// `mimalloc_committed_bytes` via [`crate::telemetry::mem_snapshot`]
/// — the two new fields landed by Phase 0 of the memory-tiering
/// work.  This pin guards against a future refactor accidentally
/// dropping the wiring (which would silently zero the telemetry
/// dataset the rest of the tiering work measures itself against).
#[tokio::test]
async fn status_populates_rss_and_mimalloc_committed() {
    let mgr = test_manager_with_drive().await;
    let status = mgr.status(0).await;

    let rss = status
        .rss_bytes
        .expect("Phase 0: status must surface rss_bytes via mem_snapshot");
    assert!(
        rss > 0,
        "rss_bytes must be positive in a live test process; got {rss}"
    );

    // Committed bytes is `Option<u64>` on the wire because mimalloc's
    // `current_commit` can underflow on macOS under heavy allocation
    // churn (observed during v0.5.77 baseline capture); the daemon's
    // `sanity_clamp_committed` rejects those readings, surfacing
    // `None` instead of a `~u64::MAX` value.  Test asserts the bound
    // only when `Some` is present so it stays meaningful on every
    // platform without forcing macOS to lie.
    if let Some(committed) = status.mimalloc_committed_bytes {
        assert!(
            committed < u64::MAX / 2,
            "mimalloc_committed_bytes looks like an underflow: {committed}"
        );
    }
}

/// `IndexManager::total_index_heap_bytes` returns 0 for a fresh
/// manager (no drives loaded).  Pins the empty-state contract the
/// `mem.snapshot` heartbeat relies on so the first event after
/// startup carries a real number rather than a panic.
#[tokio::test]
async fn total_index_heap_bytes_zero_for_empty_manager() {
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    assert_eq!(mgr.total_index_heap_bytes().await, 0);
}

/// After [`IndexManager::add_drive`] the heap total is non-zero
/// and matches the sum reported via the per-drive breakdown in
/// `status().drive_memory`.  Pins the contract that the heartbeat
/// path and the JSON-RPC `status` path agree on the same number.
#[tokio::test]
async fn total_index_heap_bytes_matches_status_breakdown() {
    let mgr = test_manager_with_drive().await;
    let total = mgr.total_index_heap_bytes().await;
    assert!(
        total > 0,
        "loaded drive must report a positive heap; got {total}"
    );

    let status = mgr.status(0).await;
    let summed: u64 = status.drive_memory.iter().map(|dm| dm.heap_bytes).sum();
    assert_eq!(
        total, summed,
        "total_index_heap_bytes ({total}) must equal sum of \
         drive_memory.heap_bytes ({summed})"
    );
    assert_eq!(
        status.index_heap_bytes,
        Some(total),
        "status.index_heap_bytes must equal total_index_heap_bytes",
    );
}

/// `status` reports the process's own command line, which is what
/// `uffs --daemon restart` starts the new daemon with, and it survives the
/// JSON round trip the CLI reads it through.
#[tokio::test]
async fn status_reports_launch_args_for_restart() {
    let mgr = test_manager_with_drive().await;
    let status = mgr.status(0).await;
    let expected: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    assert_eq!(status.launch_args.as_ref(), Some(&expected));

    let wire = serde_json::to_string(&status).expect("status serialises");
    let back: uffs_client::protocol::response::StatusResponse =
        serde_json::from_str(&wire).expect("status deserialises");
    assert_eq!(back.launch_args, Some(expected));
}
