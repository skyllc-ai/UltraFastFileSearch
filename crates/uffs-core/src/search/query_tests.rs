// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for compact-index query search: matching, sorting, filtering, limits.
//! Exception: `file_size_policy` — query test suite, shared index fixture
//! requires cohesion.

use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

use super::*;
use crate::compact::build_compact_index;
use crate::search::backend::{FilterMode, MultiDriveBackend, SearchRequest, SortColumn};
use crate::search::filters::SearchFilters;

/// Build a test fixture with root + files + dir + system metafile.
fn build_test_drive() -> DriveCompactIndex {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    let dir_name = "Projects";
    let dir_off = idx.add_name(dir_name);
    let dir_ext = idx.intern_extension(dir_name);
    let dir = idx.get_or_create(100.into());
    dir.stdinfo.set_directory(true);
    dir.stdinfo.flags = 0x10;
    dir.first_name.name = IndexNameRef::new(
        dir_off,
        u16::try_from(dir_name.len()).expect("name too long"),
        true,
        dir_ext,
    );
    dir.first_name.parent_frs = Into::into(ROOT_FRS);
    dir.descendants = 2;
    dir.treesize = 700;

    let f1_name = "readme.txt";
    let f1_off = idx.add_name(f1_name);
    let f1_ext = idx.intern_extension(f1_name);
    let f1 = idx.get_or_create(200.into());
    f1.first_name.name = IndexNameRef::new(
        f1_off,
        u16::try_from(f1_name.len()).expect("name too long"),
        true,
        f1_ext,
    );
    f1.first_name.parent_frs = Into::into(100);
    f1.first_stream.size = SizeInfo {
        length: 400,
        allocated: 512,
    };
    f1.stdinfo.flags = 0x20;
    f1.stdinfo.modified = 5_000_000;
    f1.stdinfo.created = 1_000_000;

    let f2_name = "data.csv";
    let f2_off = idx.add_name(f2_name);
    let f2_ext = idx.intern_extension(f2_name);
    let f2 = idx.get_or_create(201.into());
    f2.first_name.name = IndexNameRef::new(
        f2_off,
        u16::try_from(f2_name.len()).expect("name too long"),
        true,
        f2_ext,
    );
    f2.first_name.parent_frs = Into::into(100);
    f2.first_stream.size = SizeInfo {
        length: 300,
        allocated: 512,
    };
    f2.stdinfo.flags = 0x20;
    f2.stdinfo.modified = 3_000_000;

    let sys_name = "$MFT";
    let sys_off = idx.add_name(sys_name);
    let sys_ext = idx.intern_extension(sys_name);
    let sys = idx.get_or_create(0.into());
    sys.first_name.name = IndexNameRef::new(
        sys_off,
        u16::try_from(sys_name.len()).expect("name too long"),
        true,
        sys_ext,
    );
    sys.first_name.parent_frs = Into::into(ROOT_FRS);
    sys.first_stream.size = SizeInfo {
        length: 1_000_000,
        allocated: 1_048_576,
    };
    sys.stdinfo.flags = 0x06;

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    drive
}

/// Build a fixture with `count` files under root (for limit tests).
fn build_large_drive(count: usize) -> DriveCompactIndex {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);
    for i in 0..count {
        let frs = (i as u64) + 100;
        let name = format!("f{i:05}.txt");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            ext,
        );
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
    }
    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    drive
}

// ── Search by name ─────────────────────────────────────────────────────

#[test]
fn search_compact_finds_file_by_name() {
    let drive = build_test_drive();
    let rows = search_compact_drive(
        &drive,
        "readme",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "search for 'readme' must find readme.txt"
    );
}

// ── DisplayRow field correctness ───────────────────────────────────────

#[test]
fn display_row_fields_match_source_data() {
    let drive = build_test_drive();
    let rows = search_compact_drive(
        &drive,
        "readme",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    let row = rows
        .iter()
        .find(|row| row.name() == "readme.txt")
        .expect("not found");
    assert_eq!(row.drive, uffs_mft::platform::DriveLetter::C);
    assert_eq!(row.size, 400);
    assert_eq!(row.allocated, 512);
    assert_eq!(row.flags, 0x20);
    assert!(!row.is_directory);
    assert_eq!(row.modified, 5_000_000);
    assert_eq!(row.created, 1_000_000);
}

// ── Directory tree metrics ─────────────────────────────────────────────

#[test]
fn display_row_directory_has_tree_metrics() {
    let drive = build_test_drive();
    let rows = search_compact_drive(
        &drive,
        "projects",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    let row = rows
        .iter()
        .find(|row| row.name() == "Projects")
        .expect("not found");
    assert!(row.is_directory);
    assert_eq!(row.descendants, 2);
    assert_eq!(row.treesize, 700);
}

// ── MultiDriveBackend: filters ─────────────────────────────────────────

#[test]
fn multi_drive_search_applies_filters() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let mut filters = SearchFilters {
        hide_system: true,
        ..Default::default()
    };
    let result = backend
        .search(SearchRequest {
            result_limit: Some(100),
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    assert!(
        !result.rows.iter().any(|row| row.name() == "$MFT"),
        "hide_system must filter $MFT"
    );
}

#[test]
fn multi_drive_search_files_only() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let mut filters = SearchFilters::default();
    let result = backend
        .search(SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    assert!(
        !result.rows.iter().any(|row| row.is_directory),
        "FilesOnly must not return dirs"
    );
}

#[test]
fn multi_drive_search_dirs_only() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let mut filters = SearchFilters::default();
    let result = backend
        .search(SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::DirsOnly,
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    assert!(
        result.rows.iter().all(|row| row.is_directory),
        "DirsOnly must only return dirs"
    );
}

// ── Sort correctness ───────────────────────────────────────────────────

#[test]
fn multi_drive_search_sort_by_size_desc() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.sort_column = SortColumn::Size;
    backend.sort_desc = true;
    backend.drives.push(drive);
    let mut filters = SearchFilters {
        hide_system: true,
        ..Default::default()
    };
    let result = backend
        .search(SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    for pair in result.rows.windows(2) {
        let left = pair.first().expect("window has first");
        let right = pair.get(1).expect("window has second");
        assert!(left.size >= right.size, "size desc violated");
    }
}

// ── Path resolution ────────────────────────────────────────────────────

#[test]
fn display_row_path_includes_volume_prefix() {
    let drive = build_test_drive();
    let rows = search_compact_drive(
        &drive,
        "readme",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    let row = rows
        .iter()
        .find(|row| row.name() == "readme.txt")
        .expect("not found");
    assert!(
        row.path.starts_with("C:\\"),
        "path must start with C:\\, got: {}",
        row.path
    );
}

// ── Limit semantics ───────────────────────────────────────────────────
// Regression: None = unlimited (no hidden default cap), Some(n) = capped.

#[test]
fn match_all_none_limit_is_unlimited() {
    let drive = build_large_drive(1_500);
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let mut filters = SearchFilters::default();
    let all = backend
        .search(SearchRequest::new("*", &mut filters))
        .expect("search runs");
    assert!(
        all.rows.len() >= 1_500,
        "None must be unlimited, got {}",
        all.rows.len()
    );
}

#[test]
fn match_all_explicit_limit_caps_results() {
    let drive = build_large_drive(1_500);
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let mut filters = SearchFilters::default();
    let cap = backend
        .search(SearchRequest {
            result_limit: Some(500),
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    assert!(
        cap.rows.len() <= 500,
        "Some(500) must cap, got {}",
        cap.rows.len()
    );
}

#[test]
fn unlimited_returns_more_than_capped() {
    let drive = build_large_drive(1_500);
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let mut filters = SearchFilters::default();
    let all = backend
        .search(SearchRequest::new("*", &mut filters))
        .expect("search runs");
    let cap = backend
        .search(SearchRequest {
            result_limit: Some(500),
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    assert!(all.rows.len() > cap.rows.len(), "unlimited > capped");
}

// ═══════════════════════════════════════════════════════════════════════
// Prefix search (search_compact_drive_prefix) — trigram fast-path
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn prefix_search_matches_generic_glob_path() {
    // The trigram-accelerated prefix path must return exactly the same set of
    // rows as the ground-truth generic glob scan. `f000` matches
    // f00000..f00099.
    let drive = build_large_drive(1_500);
    let prefix_rows =
        search_compact_drive_prefix(&drive, "f000", 10_000, false, &SearchFilters::default());
    let glob_rows = search_compact_drive(
        &drive,
        "f000*",
        10_000,
        false,
        false,
        false,
        &SearchFilters::default(),
    );

    let mut prefix_names: Vec<&str> = prefix_rows.iter().map(DisplayRow::name).collect();
    let mut glob_names: Vec<&str> = glob_rows.iter().map(DisplayRow::name).collect();
    prefix_names.sort_unstable();
    glob_names.sort_unstable();

    assert!(!prefix_names.is_empty(), "fixture must contain f000* files");
    assert_eq!(
        prefix_names, glob_names,
        "prefix fast-path must return the same set as the generic glob scan"
    );
}

#[test]
fn prefix_search_respects_limit() {
    let drive = build_large_drive(1_500);
    let rows = search_compact_drive_prefix(&drive, "f00", 25, false, &SearchFilters::default());
    assert!(
        rows.len() <= 25,
        "prefix search must respect limit, got {}",
        rows.len()
    );
}

#[test]
fn large_glob_uses_parallel_resolve_with_correct_rows() {
    // 9 000 files all share the "f0" stem, so a "f0*" glob yields more than
    // RESOLVE_CHUNK_SIZE (4 096) matches and drives indices_to_rows down its
    // parallel branch. Verify that path returns every match with intact paths
    // (no dropped, duplicated, or misordered rows from the chunk reduce).
    let drive = build_large_drive(9_000);
    let rows = search_compact_drive(
        &drive,
        "f0*",
        20_000,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    assert_eq!(
        rows.len(),
        9_000,
        "every f0* file must resolve via the parallel path"
    );
    assert!(
        rows.iter()
            .all(|row| row.path.starts_with("C:\\") && row.name().starts_with("f0")),
        "parallel-resolved rows must keep correct volume prefix and name"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Regex search (search_compact_drive_regex)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn regex_search_finds_matching_files() {
    let drive = build_test_drive();
    let re = regex::Regex::new("(?i)readme").expect("valid regex");
    let rows = search_compact_drive_regex(&drive, &re, 100, &SearchFilters::default());
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "regex 'readme' must find readme.txt"
    );
}

#[test]
fn regex_search_no_match_returns_empty() {
    let drive = build_test_drive();
    let re = regex::Regex::new("zzz_no_match[0-9]+").expect("valid regex");
    let rows = search_compact_drive_regex(&drive, &re, 100, &SearchFilters::default());
    assert!(rows.is_empty(), "regex with no match must return empty");
}

#[test]
fn regex_search_respects_limit() {
    let drive = build_large_drive(500);
    let re = regex::Regex::new("f[0-9]+").expect("valid regex");
    let rows = search_compact_drive_regex(&drive, &re, 10, &SearchFilters::default());
    assert!(
        rows.len() <= 10,
        "regex search must respect limit, got {}",
        rows.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// make_display_row ADS logic
// ═══════════════════════════════════════════════════════════════════════

/// Build a fixture with an ADS on a directory.
fn build_ads_on_dir_drive() -> DriveCompactIndex {
    use uffs_mft::index::{
        IndexNameRef, IndexStreamInfo, MftIndex, NO_ENTRY, ROOT_FRS, SizeInfo, StandardInfo,
    };

    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // A directory with an ADS
    let dir_name = "MyFolder";
    let dir_off = idx.add_name(dir_name);
    let dir_ext = idx.intern_extension(dir_name);
    let dir_rec = idx.get_or_create(100.into());
    dir_rec.stdinfo.set_directory(true);
    dir_rec.stdinfo.flags |= StandardInfo::IS_ARCHIVE;
    dir_rec.first_name.name = IndexNameRef::new(
        dir_off,
        u16::try_from(dir_name.len()).expect("len"),
        true,
        dir_ext,
    );
    dir_rec.first_name.parent_frs = Into::into(ROOT_FRS);

    // Add ADS stream
    let stream_name = "metadata";
    let stream_off = idx.add_name(stream_name);
    let stream_ref = IndexNameRef::new(
        stream_off,
        u16::try_from(stream_name.len()).expect("len"),
        true,
        0,
    );
    let si = uffs_mft::len_to_u32(idx.streams.len());
    idx.streams.push(IndexStreamInfo {
        size: SizeInfo {
            length: 42,
            allocated: 64,
        },
        next_entry: NO_ENTRY,
        name: stream_ref,
        flags: 8 << 2,
        _pad0: [0; 3],
    });

    let dir_idx = idx.frs_to_idx_opt(100.into()).expect("dir idx");
    let dir_mut = idx.records.get_mut(dir_idx).expect("dir record");
    dir_mut.first_stream.next_entry = si;
    dir_mut.stream_count = 2;
    dir_mut.total_stream_count = 2;

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    drive
}

#[test]
fn ads_on_directory_display_row_is_not_directory() {
    let drive = build_ads_on_dir_drive();
    // needle must be lowered — search_compact_drive expects pre-lowered for
    // case-insensitive
    let rows = search_compact_drive(
        &drive,
        "myfolder:metadata",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    let ads_row = rows
        .iter()
        .find(|row| row.name().contains(':'))
        .expect("ADS row must exist");
    assert!(
        !ads_row.is_directory,
        "ADS on directory must render as non-directory in DisplayRow"
    );
    assert_eq!(ads_row.size, 42, "ADS must show stream size");
}

#[test]
fn normal_directory_display_row_is_directory() {
    let drive = build_ads_on_dir_drive();
    // needle must be lowered — search_compact_drive expects pre-lowered for
    // case-insensitive
    let rows = search_compact_drive(
        &drive,
        "myfolder",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    let dir_row = rows
        .iter()
        .find(|row| row.name() == "MyFolder")
        .expect("directory row must exist");
    assert!(
        dir_row.is_directory,
        "normal directory must render as directory in DisplayRow"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Case-sensitive and whole-word search
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn case_sensitive_search_misses_wrong_case() {
    let drive = build_test_drive();
    let rows = search_compact_drive(
        &drive,
        "README",
        100,
        true,
        false,
        false,
        &SearchFilters::default(),
    );
    assert!(
        !rows.iter().any(|row| row.name() == "readme.txt"),
        "case-sensitive 'README' must not match 'readme.txt'"
    );
}

#[test]
fn case_insensitive_search_finds_any_case() {
    let drive = build_test_drive();
    // needle must be pre-lowered for case-insensitive search (caller's
    // responsibility)
    let rows = search_compact_drive(
        &drive,
        "readme",
        100,
        false,
        false,
        false,
        &SearchFilters::default(),
    );
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "case-insensitive 'readme' must match 'readme.txt'"
    );
}

#[test]
fn whole_word_search_exact_match() {
    let drive = build_test_drive();
    // Whole-word with exact name (no extension)
    let rows = search_compact_drive(
        &drive,
        "readme.txt",
        100,
        false,
        true,
        false,
        &SearchFilters::default(),
    );
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "whole-word exact match must find readme.txt"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// collect_global_top_n — boolean flag sort keys
// ═══════════════════════════════════════════════════════════════════════

/// Build a test drive where two files differ only by a single NTFS attribute
/// flag.  `flagged_name` has the bit set; `unflagged_name` does not.
///
/// Both files have identical modified timestamps so any sort that falls back
/// to `rec.modified` will produce an UNSTABLE order — the test catches this.
fn build_flag_test_drive(
    flag_bit: u32,
    flagged_name: &str,
    unflagged_name: &str,
) -> DriveCompactIndex {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    // Root directory.
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // File WITH the flag.
    let f1_off = idx.add_name(flagged_name);
    let f1_ext = idx.intern_extension(flagged_name);
    let f1 = idx.get_or_create(100.into());
    f1.first_name.name = IndexNameRef::new(
        f1_off,
        u16::try_from(flagged_name.len()).expect("name"),
        true,
        f1_ext,
    );
    f1.first_name.parent_frs = Into::into(ROOT_FRS);
    f1.first_stream.size = SizeInfo {
        length: 100,
        allocated: 512,
    };
    f1.stdinfo.flags = flag_bit | 0x20; // flag + archive
    f1.stdinfo.modified = 5_000_000;
    f1.stdinfo.created = 5_000_000;

    // File WITHOUT the flag — same timestamps.
    let f2_off = idx.add_name(unflagged_name);
    let f2_ext = idx.intern_extension(unflagged_name);
    let f2 = idx.get_or_create(101.into());
    f2.first_name.name = IndexNameRef::new(
        f2_off,
        u16::try_from(unflagged_name.len()).expect("name"),
        true,
        f2_ext,
    );
    f2.first_name.parent_frs = Into::into(ROOT_FRS);
    f2.first_stream.size = SizeInfo {
        length: 100,
        allocated: 512,
    };
    f2.stdinfo.flags = 0x20; // archive only, flag NOT set
    f2.stdinfo.modified = 5_000_000; // same as f1 — deliberate
    f2.stdinfo.created = 5_000_000;

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    drive
}

/// Verify that sorting by a boolean flag field places the flagged record
/// first when descending, and last when ascending.
///
/// This test covers the TOP-N heap path in `collect_global_top_n_numeric`
/// (pattern `"*"` with a limit), which uses a numeric sort key.  A past
/// regression fell back to `rec.modified` for all boolean fields, making
/// boolean sorts effectively random.
fn assert_boolean_sort(field: FieldId, flag_bit: u32) {
    let drive = build_flag_test_drive(flag_bit, "flagged.dat", "plain.dat");
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    // Descending: flagged record should come first.
    let (rows_desc, _) =
        collect_global_top_n(&drives, 10, field, true, FilterMode::All, &mut filters);
    assert!(
        rows_desc.len() >= 2,
        "{field:?} desc: expected ≥2 rows, got {}",
        rows_desc.len()
    );
    assert_eq!(
        rows_desc.first().expect("first").name(),
        "flagged.dat",
        "{field:?} desc: flagged record must sort first"
    );

    // Ascending: flagged record should come last.
    let (rows_asc, _) =
        collect_global_top_n(&drives, 10, field, false, FilterMode::All, &mut filters);
    assert!(
        rows_asc.len() >= 2,
        "{field:?} asc: expected ≥2 rows, got {}",
        rows_asc.len()
    );
    assert_eq!(
        rows_asc.last().expect("last").name(),
        "flagged.dat",
        "{field:?} asc: flagged record must sort last"
    );
}

#[test]
fn top_n_sort_by_directory_flag() {
    // DirectoryFlag uses is_directory() (bit 0x0010).
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // A directory.
    let dir_off = idx.add_name("mydir");
    let dir_ext = idx.intern_extension("mydir");
    let dir_rec = idx.get_or_create(100.into());
    dir_rec.stdinfo.set_directory(true);
    dir_rec.stdinfo.flags = 0x0010; // directory
    dir_rec.first_name.name = IndexNameRef::new(dir_off, 5, true, dir_ext);
    dir_rec.first_name.parent_frs = Into::into(ROOT_FRS);
    dir_rec.stdinfo.modified = 5_000_000;

    // A file with the same timestamp.
    let file_off = idx.add_name("myfile.txt");
    let file_ext = idx.intern_extension("myfile.txt");
    let file_rec = idx.get_or_create(101.into());
    file_rec.stdinfo.flags = 0x0020; // archive, NOT directory
    file_rec.first_name.name = IndexNameRef::new(file_off, 10, true, file_ext);
    file_rec.first_name.parent_frs = Into::into(ROOT_FRS);
    file_rec.first_stream.size = SizeInfo {
        length: 50,
        allocated: 512,
    };
    file_rec.stdinfo.modified = 5_000_000;

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    // Desc: directory first.
    let (rows, _) = collect_global_top_n(
        &drives,
        10,
        FieldId::DirectoryFlag,
        true,
        FilterMode::All,
        &mut filters,
    );
    assert!(rows.len() >= 2, "expected ≥2 rows, got {}", rows.len());
    assert_eq!(
        rows.first().expect("first").name(),
        "mydir",
        "DirectoryFlag desc: directory must sort first"
    );
    assert_eq!(
        rows.last().expect("last").name(),
        "myfile.txt",
        "DirectoryFlag desc: file must sort last"
    );
}

#[test]
fn top_n_sort_by_hidden_flag() {
    assert_boolean_sort(FieldId::Hidden, 0x0002);
}

#[test]
fn top_n_sort_by_system_flag() {
    assert_boolean_sort(FieldId::System, 0x0004);
}

#[test]
fn top_n_sort_by_readonly_flag() {
    assert_boolean_sort(FieldId::ReadOnly, 0x0001);
}

#[test]
fn top_n_sort_by_compressed_flag() {
    assert_boolean_sort(FieldId::Compressed, 0x0800);
}

#[test]
fn top_n_sort_by_encrypted_flag() {
    assert_boolean_sort(FieldId::Encrypted, 0x4000);
}

#[test]
fn top_n_sort_by_sparse_flag() {
    assert_boolean_sort(FieldId::Sparse, 0x0200);
}

#[test]
fn top_n_sort_by_reparse_flag() {
    assert_boolean_sort(FieldId::Reparse, 0x0400);
}

#[test]
fn top_n_sort_by_offline_flag() {
    assert_boolean_sort(FieldId::Offline, 0x1000);
}

#[test]
fn top_n_sort_by_not_indexed_flag() {
    assert_boolean_sort(FieldId::NotIndexed, 0x2000);
}

#[test]
fn top_n_sort_by_integrity_flag() {
    assert_boolean_sort(FieldId::Integrity, 0x8000);
}

#[test]
fn top_n_sort_by_no_scrub_flag() {
    assert_boolean_sort(FieldId::NoScrub, 0x0002_0000);
}

#[test]
fn top_n_sort_by_pinned_flag() {
    assert_boolean_sort(FieldId::Pinned, 0x0008_0000);
}

#[test]
fn top_n_sort_by_unpinned_flag() {
    assert_boolean_sort(FieldId::Unpinned, 0x0010_0000);
}

// ═══════════════════════════════════════════════════════════════════════
// Heap eviction — more records than limit
// ═══════════════════════════════════════════════════════════════════════

/// Build a drive with `n_files` regular files and `n_dirs` directories.
///
/// ALL records share the same `modified` timestamp so any sort that
/// falls back to `rec.modified` will produce arbitrary (wrong) order.
fn build_mixed_drive(n_files: usize, n_dirs: usize) -> DriveCompactIndex {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    // Root directory (FRS 5).
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // Files first (FRS 100..).
    for i in 0..n_files {
        let frs = (i as u64) + 100;
        let name = format!("file_{i:04}.dat");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("name"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20; // archive only, NOT directory
        rec.stdinfo.modified = 5_000_000; // same timestamp for all
        rec.stdinfo.created = 5_000_000;
    }

    // Directories (FRS 10_000..).
    for i in 0..n_dirs {
        let frs = (i as u64) + 10_000;
        let name = format!("dir_{i:04}");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.stdinfo.set_directory(true);
        rec.stdinfo.flags = 0x10; // directory flag
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("name"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.stdinfo.modified = 5_000_000; // same timestamp
        rec.stdinfo.created = 5_000_000;
    }

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    drive
}

/// Heap eviction: 30 files + 10 dirs, limit 5, sort directory:desc.
///
/// The heap processes files FIRST (FRS 100-129), filling with key=0.
/// Then directories (FRS 10000-10009) arrive with key=1 and must EVICT
/// the files.  Result: all 5 slots should be directories.
#[test]
fn heap_eviction_directory_desc_dirs_come_last() {
    let drive = build_mixed_drive(30, 10);
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        5,
        FieldId::DirectoryFlag,
        true,
        FilterMode::All,
        &mut filters,
    );
    assert_eq!(rows.len(), 5, "expected 5 rows");
    for (i, row) in rows.iter().enumerate() {
        assert!(
            row.is_directory,
            "row {i} '{}' must be directory (DirectoryFlag desc, limit 5, 10 dirs available)",
            row.name()
        );
    }
}

/// Heap eviction: same setup but ascending — all 5 should be files.
#[test]
fn heap_eviction_directory_asc_files_come_last() {
    let drive = build_mixed_drive(30, 10);
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        5,
        FieldId::DirectoryFlag,
        false,
        FilterMode::All,
        &mut filters,
    );
    assert_eq!(rows.len(), 5, "expected 5 rows");
    for (i, row) in rows.iter().enumerate() {
        assert!(
            !row.is_directory,
            "row {i} '{}' must be file (DirectoryFlag asc, limit 5, 30 files available)",
            row.name()
        );
    }
}

/// Heap eviction with hidden flag: 20 normal + 10 hidden, limit 5, desc.
#[test]
fn heap_eviction_hidden_desc() {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // 20 normal files (no hidden flag).
    for i in 0_u64..20 {
        let frs = i + 100;
        let name = format!("normal_{i:04}.dat");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("name"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20; // archive only
        rec.stdinfo.modified = 5_000_000;
    }
    // 10 hidden files.
    for i in 0_u64..10 {
        let frs = i + 10_000;
        let name = format!("hidden_{i:04}.dat");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("name"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x22; // archive + hidden
        rec.stdinfo.modified = 5_000_000;
    }
    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        5,
        FieldId::Hidden,
        true,
        FilterMode::All,
        &mut filters,
    );
    assert_eq!(rows.len(), 5);
    for (i, row) in rows.iter().enumerate() {
        assert!(
            row.flags & 0x0002 != 0,
            "row {i} '{}' flags=0x{:X} must have hidden bit (Hidden desc, limit 5)",
            row.name(),
            row.flags
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// search_index integration — pattern "*" with boolean sort
// ═══════════════════════════════════════════════════════════════════════

/// End-to-end test through `search_index` (the daemon's entry point).
///
/// Pattern `"*"` triggers the `is_match_all` path → `collect_global_top_n`.
/// This test verifies the EXACT code path that the daemon uses when the
/// CLI sends `--sort directory:desc --limit 5`.
#[test]
fn search_index_star_sort_directory_desc() {
    use alloc::sync::Arc;

    use crate::search::backend::{DriveIndex, SearchRequest, search_index};

    let drive = build_mixed_drive(30, 10);
    let index = DriveIndex {
        drives: vec![Arc::new(drive)],
    };
    let mut filters = SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            pattern: "*",
            case_sensitive: false,
            whole_word: false,
            match_path: false,
            result_limit: Some(5),
            filter_mode: FilterMode::All,
            search_filters: &mut filters,
            drives_filter: &[],
        },
        FieldId::DirectoryFlag,
        true, // descending
        &[],
    )
    .expect("search runs");
    assert_eq!(result.rows.len(), 5, "expected 5 rows from search_index");
    for (i, row) in result.rows.iter().enumerate() {
        assert!(
            row.is_directory,
            "search_index row {i} '{}' flags=0x{:X} must be directory \
             (sort=DirectoryFlag desc, limit=5, 10 dirs available)",
            row.name(),
            row.flags
        );
    }
}

/// Same test but ascending — all 5 should be files.
#[test]
fn search_index_star_sort_directory_asc() {
    use alloc::sync::Arc;

    use crate::search::backend::{DriveIndex, SearchRequest, search_index};

    let drive = build_mixed_drive(30, 10);
    let index = DriveIndex {
        drives: vec![Arc::new(drive)],
    };
    let mut filters = SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            pattern: "*",
            case_sensitive: false,
            whole_word: false,
            match_path: false,
            result_limit: Some(5),
            filter_mode: FilterMode::All,
            search_filters: &mut filters,
            drives_filter: &[],
        },
        FieldId::DirectoryFlag,
        false, // ascending
        &[],
    )
    .expect("search runs");
    assert_eq!(result.rows.len(), 5, "expected 5 rows from search_index");
    for (i, row) in result.rows.iter().enumerate() {
        assert!(
            !row.is_directory,
            "search_index row {i} '{}' flags=0x{:X} must be file \
             (sort=DirectoryFlag asc, limit=5, 30 files available)",
            row.name(),
            row.flags
        );
    }
}

/// Integration: sort by hidden flag through `search_index`.
#[test]
fn search_index_star_sort_hidden_desc() {
    use alloc::sync::Arc;

    use crate::search::backend::{DriveIndex, SearchRequest, search_index};

    // Reuse the hidden drive from heap_eviction_hidden_desc.
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);
    for i in 0_u64..20 {
        let frs = i + 100;
        let name = format!("normal_{i:04}.dat");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("name"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
        rec.stdinfo.modified = 5_000_000;
    }
    for i in 0_u64..10 {
        let frs = i + 10_000;
        let name = format!("hidden_{i:04}.dat");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("name"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x22; // archive + hidden
        rec.stdinfo.modified = 5_000_000;
    }
    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    let index = DriveIndex {
        drives: vec![Arc::new(drive)],
    };
    let mut filters = SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            pattern: "*",
            case_sensitive: false,
            whole_word: false,
            match_path: false,
            result_limit: Some(5),
            filter_mode: FilterMode::All,
            search_filters: &mut filters,
            drives_filter: &[],
        },
        FieldId::Hidden,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(result.rows.len(), 5);
    for (i, row) in result.rows.iter().enumerate() {
        assert!(
            row.flags & 0x0002 != 0,
            "search_index row {i} '{}' flags=0x{:X} must be hidden \
             (sort=Hidden desc, limit=5)",
            row.name(),
            row.flags
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// collect_global_top_n — `FieldId::Bulkiness` integration pins
// ═══════════════════════════════════════════════════════════════════════
//
// `bulkiness_for_row` vs `bulkiness_for_record` equivalence is pinned
// by the unit tests in `search/derived.rs`.  These tests pin the
// *integration* — i.e. that `numeric_top_n.rs::push_record` reaches
// the right function with the right argument, casts cleanly to the
// heap's i64 sort key, and produces a stable high-to-low ordering.
// A regression in any of those links (wrong cast, wrong type passed,
// DisplayRow dance silently reintroduced) fails these.

/// Build a drive with three files whose `bulkiness_for_record` values
/// are distinct and easy to rank: 1.0×, 2.0×, and 4.0× the logical
/// size.  Returns (drive, expected descending order by name).
fn build_bulkiness_test_drive() -> (DriveCompactIndex, [&'static str; 3]) {
    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // (name, frs, size, allocated) — all three files share the same
    // `modified` so a regression that falls back to `rec.modified`
    // (the pre-Run-10 default) would produce an unstable order.
    let files: &[(&str, u64, u64, u64)] = &[
        // bulkiness = 1.0 × SCALE = 1_000_000
        ("dense.dat", 100, 1_000, 1_000),
        // bulkiness = 2.0 × SCALE = 2_000_000
        ("medium.dat", 101, 1_000, 2_000),
        // bulkiness = 4.0 × SCALE = 4_000_000 (bulkiest)
        ("sparse.dat", 102, 1_000, 4_000),
    ];

    for &(name, frs, size, allocated) in files {
        let off = idx.add_name(name);
        let ext = idx.intern_extension(name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            ext,
        );
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: size,
            allocated,
        };
        rec.stdinfo.flags = 0x20; // archive, not a directory
        rec.stdinfo.modified = 7_000_000; // deliberately identical
        rec.stdinfo.created = 7_000_000;
    }

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    // Expected order when sorted by bulkiness DESC.
    (drive, ["sparse.dat", "medium.dat", "dense.dat"])
}

/// Desc sort by `FieldId::Bulkiness` must produce bulkiest-first.
///
/// Exercises the exact hot-path arm at
/// `numeric_top_n.rs::push_record::FieldId::Bulkiness` — the one
/// that was collapsed from an 18-line `DisplayRow::new(...)` dance
/// into a single `bulkiness_for_record(rec) as i64` call.  A future
/// regression there (wrong argument, forgotten cast, accidental
/// re-introduction of the allocation) fails this test.
#[test]
fn top_n_sort_by_bulkiness_desc_orders_by_ratio() {
    let (drive, expected_desc) = build_bulkiness_test_drive();
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        10,
        FieldId::Bulkiness,
        true,
        FilterMode::All,
        &mut filters,
    );

    let got: Vec<&str> = rows.iter().map(DisplayRow::name).collect();
    assert!(
        got.len() >= expected_desc.len(),
        "expected ≥{} rows, got {}: {got:?}",
        expected_desc.len(),
        got.len()
    );
    // Compare only the first three — the root entry may or may not
    // appear below these depending on filter mode, but the three
    // test files must appear in bulkiness order at the top.
    let got_top_three = got.get(..3).expect("asserted ≥3 rows above");
    assert_eq!(
        got_top_three,
        &expected_desc[..],
        "bulkiness desc must rank sparse > medium > dense; got {got:?}",
    );
}

/// Asc sort is the mirror image — least bulky first.  Pins that the
/// heap's inversion flag is threaded correctly through the
/// `FieldId::Bulkiness` arm (same sort-key computation, opposite
/// ordering).
#[test]
fn top_n_sort_by_bulkiness_asc_orders_by_ratio() {
    let (drive, expected_desc) = build_bulkiness_test_drive();
    let drives = vec![drive];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        10,
        FieldId::Bulkiness,
        false,
        FilterMode::All,
        &mut filters,
    );

    let got: Vec<&str> = rows.iter().map(DisplayRow::name).collect();
    // The three test files must appear in *reverse* bulkiness order
    // among the last three positions.  Use `rev()` on the expected
    // list for the assertion.
    let expected_last_three: Vec<&str> = expected_desc.iter().rev().copied().collect();
    let got_last_three: Vec<&str> = got.iter().rev().take(3).copied().collect();
    // `got_last_three` is in reverse-iteration order; flip it so the
    // comparison matches `expected_last_three` (dense → medium → sparse).
    let got_last_three_forward: Vec<&str> = got_last_three.iter().rev().copied().collect();
    assert_eq!(
        got_last_three_forward, expected_last_three,
        "bulkiness asc must rank dense < medium < sparse at the tail; got {got:?}",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 5 regression tests — parallel per-drive scan + ext fast path on
// `FieldId::Path` sort.  Both pin fixes published in the v0.5.66
// cross-tool benchmark:
//
//   • `* --limit 100`  (regressed from 163 ms → 1 112 ms in Phase 2)
//   • `*.dll --sort path` (scaled with drive size, not match count)
//
// See `docs/benchmarks/2026-04-v0.5.66-vs-everything-and-cpp.md`.
// ═══════════════════════════════════════════════════════════════════════

/// Build a drive with `count` files plus a root.  Each file has a
/// *unique* `modified` timestamp equal to its FRS so the top-N by
/// Modified-DESC is fully determined by the fixture (the N largest
/// FRS values).
fn build_modified_gradient_drive(
    letter: uffs_mft::platform::DriveLetter,
    base_frs: u64,
    count: usize,
) -> DriveCompactIndex {
    let mut idx = MftIndex::new(letter);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);
    for i in 0..count {
        let frs = base_frs + (i as u64);
        let name = format!("{letter}_f{i:05}.txt");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            ext,
        );
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
        // Monotonic timestamps: ensures Modified-DESC order is
        // fully determined by FRS, independent of drive order or
        // rayon scheduling.  `u64::cast_signed` is the Rust 1.87
        // exact-bit-pattern reinterpret; small test-only FRS values
        // stay well inside i64 so the high bit never flips.
        rec.stdinfo.modified = frs.cast_signed();
    }
    let (drive, _, _) = build_compact_index(letter, &idx);
    drive
}

/// Parallel per-drive scan must produce the same global top-N
/// regardless of the order drives arrive.  Uses three drives, each
/// with 200 files, and a limit of 10 — the 10 newest records across
/// the union must be the 10 highest-FRS records from drive C
/// (because C's FRS range is highest).
///
/// Regression target: the parallel rayon drive scan introduced by
/// the Phase 5 fix.  If per-drive heaps were swapped for a shared
/// `&mut` reference (or if merge / truncate dropped rows from any
/// drive) this test would fail.
#[test]
fn parallel_drive_scan_merges_global_top_n_across_drives() {
    // Three drives, non-overlapping FRS ranges → deterministic
    // global top-10 = drive-C's highest 10 FRS values.
    let drive_a = build_modified_gradient_drive(uffs_mft::platform::DriveLetter::A, 100, 200);
    let drive_b = build_modified_gradient_drive(uffs_mft::platform::DriveLetter::B, 10_000, 200);
    let drive_c = build_modified_gradient_drive(uffs_mft::platform::DriveLetter::C, 1_000_000, 200);
    let drives = vec![drive_a, drive_b, drive_c];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        10,
        FieldId::Modified,
        true,
        FilterMode::All,
        &mut filters,
    );

    assert_eq!(rows.len(), 10, "limit=10 must return exactly 10 rows");
    for row in &rows {
        assert_eq!(
            row.drive,
            uffs_mft::platform::DriveLetter::C,
            "Modified-DESC top-10 must all come from drive C \
             (highest FRS range); got {}:{}",
            row.drive,
            row.name()
        );
    }
    // The 10 rows must be modified-DESC sorted.
    for pair in rows.windows(2) {
        let [lhs, rhs] = pair else {
            continue;
        };
        assert!(
            lhs.modified >= rhs.modified,
            "Modified-DESC order violated: {} ({}) came before {} ({})",
            lhs.name(),
            lhs.modified,
            rhs.name(),
            rhs.modified
        );
    }
}

/// Parallel per-drive scan must respect the global `limit` even
/// when each individual drive holds *more* records than the limit.
///
/// Exercises the scenario that triggered the `BinaryHeap::with_capacity(limit)`
/// requirement: a bounded per-drive heap plus an outer merge +
/// truncate that never allocates `drives.len() * drive.records.len()`
/// intermediate memory.
#[test]
fn parallel_drive_scan_respects_limit_smaller_than_per_drive_count() {
    let drive_a = build_modified_gradient_drive(uffs_mft::platform::DriveLetter::A, 100, 500);
    let drive_b = build_modified_gradient_drive(uffs_mft::platform::DriveLetter::B, 10_000, 500);
    let drives = vec![drive_a, drive_b];
    let mut filters = SearchFilters::default();

    let (rows, _) = collect_global_top_n(
        &drives,
        5,
        FieldId::Modified,
        true,
        FilterMode::All,
        &mut filters,
    );

    assert_eq!(rows.len(), 5, "limit=5 must cap merged result set");
    // Top-5 Modified-DESC across both drives must all come from B
    // (B's FRS range is higher, so B has the 5 newest records).
    for row in &rows {
        assert_eq!(
            row.drive,
            uffs_mft::platform::DriveLetter::B,
            "top-5 must all be from drive B"
        );
    }
}

/// Build a drive with `count` files split across two extensions:
/// half `.dll`, half `.txt`.  All share the same parent.  Used by
/// the `FieldId::Path` ext fast-path regression test to verify that
/// the ext-only fast path returns exactly the `.dll` rows — no
/// `.txt` leakage — and in lexicographic full-path order.
fn build_two_extension_drive(
    letter: uffs_mft::platform::DriveLetter,
    count: usize,
) -> DriveCompactIndex {
    let mut idx = MftIndex::new(letter);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);
    for i in 0..count {
        let frs = 100 + (i as u64);
        let ext_part = if i % 2 == 0 { "dll" } else { "txt" };
        let name = format!("file_{i:05}.{ext_part}");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            ext,
        );
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
    }
    let (drive, _, _) = build_compact_index(letter, &idx);
    drive
}

/// `FieldId::Path` with an ext-only filter must take the ext-index
/// fast path: the result must contain *only* the matching-extension
/// rows (not the other extension's rows) and must be full-path
/// sorted.
///
/// Regression target: `collect_path_sorted_top_n`'s newly-added
/// ext-index fast path.  If the fast-path branch leaks non-matching
/// records, or if `backend::sort_rows(.., FieldId::Path, ..)` is
/// bypassed, this test fails.
#[test]
fn path_sort_ext_only_returns_matching_ext_in_path_order() {
    let drive = build_two_extension_drive(uffs_mft::platform::DriveLetter::C, 20);
    let drives = vec![drive];
    let mut filters = SearchFilters {
        extensions: vec!["dll".into()],
        ..Default::default()
    };

    let (rows, _) = collect_global_top_n(
        &drives,
        100,
        FieldId::Path,
        false,
        FilterMode::All,
        &mut filters,
    );

    // Only `.dll` rows — no `.txt` leakage.
    for row in &rows {
        let ext = std::path::Path::new(row.name())
            .extension()
            .and_then(std::ffi::OsStr::to_str);
        assert!(
            ext.is_some_and(|found| found.eq_ignore_ascii_case("dll")),
            "ext fast path leaked non-matching extension: {}",
            row.name()
        );
    }
    // 20 files, half are `.dll`, so 10 rows.
    assert_eq!(rows.len(), 10, "expected 10 .dll rows, got {}", rows.len());
    // Paths must be in lexicographic ascending order (case-folded,
    // same contract as `backend::sort_rows(.., FieldId::Path, false, ..)`).
    for pair in rows.windows(2) {
        let [lhs, rhs] = pair else {
            continue;
        };
        assert!(
            lhs.path.to_lowercase() <= rhs.path.to_lowercase(),
            "Path-ASC violated: {} came before {}",
            lhs.path,
            rhs.path
        );
    }
}

/// `FieldId::Path` DESC with ext-only filter must produce the
/// reverse-lex order via the same fast path.  Pins that the
/// `sort_desc` flag is threaded through `backend::sort_rows(..,
/// FieldId::Path, sort_desc, ..)` inside the fast path.
#[test]
fn path_sort_ext_only_desc_returns_reverse_lex_order() {
    let drive = build_two_extension_drive(uffs_mft::platform::DriveLetter::C, 20);
    let drives = vec![drive];
    let mut filters = SearchFilters {
        extensions: vec!["dll".into()],
        ..Default::default()
    };

    let (rows, _) = collect_global_top_n(
        &drives,
        100,
        FieldId::Path,
        true,
        FilterMode::All,
        &mut filters,
    );

    assert_eq!(rows.len(), 10, "expected 10 .dll rows");
    for pair in rows.windows(2) {
        let [lhs, rhs] = pair else {
            continue;
        };
        assert!(
            lhs.path.to_lowercase() >= rhs.path.to_lowercase(),
            "Path-DESC violated: {} came before {}",
            lhs.path,
            rhs.path
        );
    }
}

/// `FieldId::Path` with a non-ext filter (e.g. `min_size`) must
/// route through the legacy tree walk — not the ext fast path.
/// Pins the `is_ext_only()` gate: adding a size predicate
/// disqualifies the fast path, so the tree walk runs instead.
#[test]
fn path_sort_non_ext_filter_uses_tree_walk() {
    let drive = build_two_extension_drive(uffs_mft::platform::DriveLetter::C, 20);
    let drives = vec![drive];
    let mut filters = SearchFilters {
        // min_size disqualifies is_ext_only, forcing the tree walk.
        min_size: Some(50),
        ..Default::default()
    };

    let (rows, _) = collect_global_top_n(
        &drives,
        100,
        FieldId::Path,
        false,
        FilterMode::All,
        &mut filters,
    );

    // All 20 files are 100 bytes so all pass min_size=50.  Plus the
    // root directory.  Rows must still be full-path sorted.
    assert!(
        rows.len() >= 20,
        "expected ≥20 rows (20 files passing min_size), got {}",
        rows.len()
    );
    for pair in rows.windows(2) {
        let [lhs, rhs] = pair else {
            continue;
        };
        assert!(
            lhs.path.to_lowercase() <= rhs.path.to_lowercase(),
            "Path-ASC violated on tree-walk fallback: {} came before {}",
            lhs.path,
            rhs.path
        );
    }
}
