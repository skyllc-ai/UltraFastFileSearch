// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! What a search finds, end to end: the CLI's argument vector parsed by
//! `SearchParams::from_cli_args` (as `search_cli` does) and run by
//! [`IndexManager::search`] over a fixture drive.
//!
//! Covers `--word`, a case-insensitive regex with escapes, an invalid regex,
//! and `--exclude` together with `--not-contains`.

use alloc::sync::Arc;

use uffs_client::protocol::SearchParams;
use uffs_client::protocol::response::SearchPayload;
use uffs_core::compact::build_compact_index;
use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

use super::IndexManager;
use crate::index::search::SearchFailure;

/// The file names of the fixture drive, all in the root directory.
const NAMES: &[&str] = &[
    "test",
    "testing.log",
    "latest.doc",
    "report2024.txt",
    "REPORT7.TXT",
    "reportX.txt",
    "draft.tmp",
    "draft-notes.md",
    "notes.md",
];

/// A manager with one drive holding [`NAMES`].
async fn manager() -> IndexManager {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);
    for (frs, name) in (100_u64..).zip(NAMES) {
        let off = idx.add_name(name);
        let ext = idx.intern_extension(name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(off, uffs_mft::len_to_u16(name.len()), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 10,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
    }
    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    let (tx, _rx) = crate::events::event_channel();
    let mgr = IndexManager::new(None, tx, Arc::new(crate::config::Config::default()));
    mgr.add_drive(drive).await;
    mgr
}

/// Run the CLI argument vector `args` and return the matched names, sorted.
async fn names(cli: &[&str]) -> Result<Vec<String>, SearchFailure> {
    let owned: Vec<String> = cli.iter().copied().map(String::from).collect();
    let mut params = SearchParams::from_cli_args(&owned).expect("valid arguments");
    // Rows, not a pre-rendered CSV blob, so the test can read the names.
    params.output_format = None;
    let response = manager().await.search(&params).await?;
    let SearchPayload::InlineRows(rows) = response.payload else {
        return Ok(Vec::new());
    };
    let mut found: Vec<String> = rows.into_iter().map(|row| row.name).collect();
    found.sort();
    Ok(found)
}

#[tokio::test]
async fn word_matches_the_whole_name_only() {
    assert_eq!(names(&["test", "--word"]).await.expect("search runs"), [
        "test"
    ]);
    let substring = names(&["test"]).await.expect("search runs");
    assert!(
        substring.len() > 1,
        "without --word, test is a substring: {substring:?}"
    );
}

#[tokio::test]
async fn case_insensitive_regex_keeps_digit_class() {
    assert_eq!(
        names(&[r">^report\d+\.txt$"]).await.expect("search runs"),
        ["REPORT7.TXT", "report2024.txt"]
    );
}

#[tokio::test]
async fn invalid_regex_is_an_error_not_an_empty_result() {
    let err = names(&[">(unclosed"]).await.expect_err("invalid regex");
    assert!(
        matches!(err, SearchFailure::InvalidPattern(_)),
        "an invalid regex must be refused: {err}"
    );
    assert_eq!(err.rpc_code(), uffs_client::protocol::ERR_INVALID_PARAMS);
}

#[tokio::test]
async fn exclude_and_not_contains_both_drop_names() {
    assert_eq!(
        names(&[
            "*.md|*.tmp",
            "--exclude",
            "*.tmp",
            "--not-contains",
            "draft"
        ])
        .await
        .expect("search runs"),
        ["notes.md"]
    );
}
