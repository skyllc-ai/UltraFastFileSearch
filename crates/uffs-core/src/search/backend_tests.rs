// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for `MultiDriveBackend`, sort spec parsing, sort correctness,
//! `display_rows_to_dataframe`, and `format_sort_spec`.
//! Exception: `file_size_policy` — backend test suite, shared fixtures require
//! cohesion.

use super::*;

// ── Helpers ──────────────────────────────────────────────────────────

/// Build a minimal `DisplayRow` for sort / aggregation tests.
fn row(
    name: &str,
    drive: uffs_mft::platform::DriveLetter,
    size: u64,
    modified: i64,
    created: i64,
) -> DisplayRow {
    DisplayRow::new(
        0,
        drive,
        format!("{drive}:\\{name}"),
        size,
        false,
        modified,
        created,
        0,
        0x20,
        size.next_multiple_of(512),
        0,
        0,
        0,
    )
}

fn dir_row(
    name: &str,
    drive: uffs_mft::platform::DriveLetter,
    descendants: u32,
    treesize: u64,
) -> DisplayRow {
    DisplayRow::new(
        0,
        drive,
        format!("{drive}:\\{name}"),
        0,
        true,
        0,
        0,
        0,
        0x10,
        0,
        descendants,
        treesize,
        treesize,
    )
}

// ═══════════════════════════════════════════════════════════════════════
// parse_sort_spec
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn parse_sort_spec_single_column_with_direction() {
    let specs = parse_sort_spec("name:asc");
    assert_eq!(specs.len(), 1);
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Name);
    assert!(!first.descending);
}

#[test]
fn parse_sort_spec_default_direction_for_size_is_desc() {
    let specs = parse_sort_spec("size");
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Size);
    assert!(first.descending, "size default direction must be desc");
}

#[test]
fn parse_sort_spec_default_direction_for_name_is_asc() {
    let specs = parse_sort_spec("name");
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Name);
    assert!(!first.descending, "name default direction must be asc");
}

#[test]
fn parse_sort_spec_all_column_aliases() {
    let cases = [
        ("name", SortColumn::Name),
        ("size", SortColumn::Size),
        ("sizeondisk", SortColumn::SizeOnDisk),
        ("allocated", SortColumn::SizeOnDisk),
        ("created", SortColumn::Created),
        ("modified", SortColumn::Modified),
        ("date", SortColumn::Modified),
        ("written", SortColumn::Modified),
        ("accessed", SortColumn::Accessed),
        ("path", SortColumn::Path),
        ("drive", SortColumn::Drive),
        ("ext", SortColumn::Extension),
        ("extension", SortColumn::Extension),
        ("type", SortColumn::Type),
        ("descendants", SortColumn::Descendants),
    ];
    for (input, expected) in cases {
        let specs = parse_sort_spec(input);
        assert_eq!(
            specs.first().map(|spec| spec.column),
            Some(expected),
            "alias '{input}' must map to {expected:?}"
        );
    }
}

#[test]
fn parse_sort_spec_multi_tier() {
    let specs = parse_sort_spec("size:desc,name:asc");
    assert_eq!(specs.len(), 2);
    let first = specs.first().expect("first");
    let second = specs.get(1).expect("second");
    assert_eq!(first.column, SortColumn::Size);
    assert!(first.descending);
    assert_eq!(second.column, SortColumn::Name);
    assert!(!second.descending);
}

#[test]
fn parse_sort_spec_unknown_column_ignored() {
    let specs = parse_sort_spec("bogus,size:desc");
    assert_eq!(specs.len(), 1, "unknown column must be skipped");
    let first = specs.first().expect("first");
    assert_eq!(first.column, SortColumn::Size);
}

#[test]
fn parse_sort_spec_empty_string() {
    let specs = parse_sort_spec("");
    assert!(specs.is_empty());
}

/// Regression: `-size` prefix means descending (T84, T87).
#[test]
fn parse_sort_spec_dash_prefix_forces_descending() {
    let specs = parse_sort_spec("-size");
    assert_eq!(specs.len(), 1);
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Size);
    assert!(first.descending, "dash prefix must force descending");
}

/// Regression: `-modified,name` → modified desc, name asc (T67).
#[test]
fn parse_sort_spec_dash_prefix_multi_tier() {
    let specs = parse_sort_spec("-modified,name");
    assert_eq!(specs.len(), 2);
    let first = specs.first().expect("first");
    assert_eq!(first.column, SortColumn::Modified);
    assert!(first.descending, "dash prefix must force descending");
    let second = specs.get(1).expect("second");
    assert_eq!(second.column, SortColumn::Name);
    assert!(!second.descending, "name without dash should be asc");
}

/// Regression: dash prefix mixed with colon-suffix direction.
#[test]
fn parse_sort_spec_dash_prefix_with_colon_suffix() {
    // Colon suffix takes precedence over dash prefix.
    let specs = parse_sort_spec("-size:asc");
    assert_eq!(specs.len(), 1);
    let first = specs.first().expect("first");
    assert_eq!(first.column, SortColumn::Size);
    assert!(
        !first.descending,
        "explicit :asc suffix must override dash prefix"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// format_sort_spec
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn format_sort_spec_primary_only() {
    let result = format_sort_spec(SortColumn::Size, true, &[]);
    assert_eq!(result, "size:desc");
}

#[test]
fn format_sort_spec_with_extra_tiers() {
    let extra = vec![SortSpec {
        column: SortColumn::Name,
        descending: false,
    }];
    let result = format_sort_spec(SortColumn::Modified, true, &extra);
    assert_eq!(result, "modified:desc,name:asc");
}

// ═══════════════════════════════════════════════════════════════════════
// sort_rows — all column variants
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sort_by_name_ascending() {
    let mut rows = vec![
        row("zebra.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("alpha.txt", uffs_mft::platform::DriveLetter::C, 200, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Name, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "alpha.txt");
}

#[test]
fn sort_by_modified_descending() {
    let mut rows = vec![
        row("old.txt", uffs_mft::platform::DriveLetter::C, 100, 1000, 0),
        row("new.txt", uffs_mft::platform::DriveLetter::C, 100, 9000, 0),
    ];
    sort_rows(&mut rows, SortColumn::Modified, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "new.txt");
}

#[test]
fn sort_by_created_descending() {
    let mut rows = vec![
        row("old.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 1000),
        row("new.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 9000),
    ];
    sort_rows(&mut rows, SortColumn::Created, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "new.txt");
}

#[test]
fn sort_by_path_ascending() {
    let mut rows = vec![
        row("z.txt", uffs_mft::platform::DriveLetter::D, 100, 0, 0),
        row("a.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Path, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "a.txt");
}

#[test]
fn sort_by_extension_ascending() {
    let mut rows = vec![
        row("file.zip", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("file.abc", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Extension, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "file.abc");
}

#[test]
fn sort_by_descendants_descending() {
    let mut rows = vec![
        dir_row("small", uffs_mft::platform::DriveLetter::C, 5, 1000),
        dir_row("big", uffs_mft::platform::DriveLetter::C, 500, 50_000),
    ];
    sort_rows(&mut rows, SortColumn::Descendants, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "big");
}

#[test]
fn sort_by_drive_ascending() {
    let mut rows = vec![
        row("file.txt", uffs_mft::platform::DriveLetter::D, 100, 0, 0),
        row("file.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Drive, false, &[]);
    assert_eq!(
        rows.first().expect("first").drive,
        uffs_mft::platform::DriveLetter::C
    );
}

#[test]
fn sort_by_size_on_disk_descending() {
    let mut rows = vec![
        row("small.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("big.txt", uffs_mft::platform::DriveLetter::C, 5000, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::SizeOnDisk, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "big.txt");
}

#[test]
fn sort_multi_tier_size_then_name() {
    let mut rows = vec![
        row("beta.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("alpha.txt", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("gamma.txt", uffs_mft::platform::DriveLetter::C, 200, 0, 0),
    ];
    let tiers = vec![SortSpec {
        column: SortColumn::Name,
        descending: false,
    }];
    sort_rows(&mut rows, SortColumn::Size, true, &tiers);
    // gamma (200) first, then alpha (100), then beta (100) — name tiebreaker
    assert_eq!(rows.first().expect("first").name(), "gamma.txt");
    assert_eq!(rows.get(1).expect("second").name(), "alpha.txt");
    assert_eq!(rows.get(2).expect("third").name(), "beta.txt");
}

#[test]
fn sort_by_type_groups_by_semantic_category() {
    // .rs → code, .zip → archive, .jpg → picture
    // ascending: archive < code < picture
    let mut rows = vec![
        row("photo.jpg", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("main.rs", uffs_mft::platform::DriveLetter::C, 200, 0, 0),
        row("backup.zip", uffs_mft::platform::DriveLetter::C, 50, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Type, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "backup.zip"); // archive
    assert_eq!(rows.get(1).expect("second").name(), "main.rs"); // code
    assert_eq!(rows.get(2).expect("third").name(), "photo.jpg"); // picture
}

#[test]
fn sort_by_type_descending() {
    let mut rows = vec![
        row("photo.jpg", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("main.rs", uffs_mft::platform::DriveLetter::C, 200, 0, 0),
        row("backup.zip", uffs_mft::platform::DriveLetter::C, 50, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Type, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "photo.jpg"); // picture
    assert_eq!(rows.get(1).expect("second").name(), "main.rs"); // code
    assert_eq!(rows.get(2).expect("third").name(), "backup.zip"); // archive
}

#[test]
fn sort_by_type_then_size() {
    // Two code files with different sizes
    let mut rows = vec![
        row("big.rs", uffs_mft::platform::DriveLetter::C, 5000, 0, 0),
        row("small.rs", uffs_mft::platform::DriveLetter::C, 100, 0, 0),
        row("photo.jpg", uffs_mft::platform::DriveLetter::C, 300, 0, 0),
    ];
    let tiers = vec![SortSpec {
        column: SortColumn::Size,
        descending: true,
    }];
    sort_rows(&mut rows, SortColumn::Type, false, &tiers);
    // code first (asc), then picture; within code, big before small (desc)
    assert_eq!(rows.first().expect("first").name(), "big.rs");
    assert_eq!(rows.get(1).expect("second").name(), "small.rs");
    assert_eq!(rows.get(2).expect("third").name(), "photo.jpg");
}

#[test]
fn sort_by_descendants_in_dir() {
    let mut rows = vec![
        dir_row("few", uffs_mft::platform::DriveLetter::C, 5, 100),
        dir_row("many", uffs_mft::platform::DriveLetter::C, 50, 500),
        dir_row("empty", uffs_mft::platform::DriveLetter::C, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Descendants, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "many");
    assert_eq!(rows.get(1).expect("second").name(), "few");
    assert_eq!(rows.get(2).expect("third").name(), "empty");
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-drive aggregation
// ═══════════════════════════════════════════════════════════════════════

/// Build a minimal `DriveCompactIndex` from the `query_tests` helpers.
fn build_two_drive_backend() -> MultiDriveBackend {
    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    let mut backend = MultiDriveBackend::new();

    for (letter, file_name, file_size) in [
        (uffs_mft::platform::DriveLetter::C, "report.txt", 400_u64),
        (uffs_mft::platform::DriveLetter::D, "data.csv", 800),
    ] {
        let mut idx = MftIndex::new(letter);
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS.into());
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = Into::into(ROOT_FRS);

        let f_off = idx.add_name(file_name);
        let f_ext = idx.intern_extension(file_name);
        let file_rec = idx.get_or_create(200.into());
        file_rec.first_name.name = IndexNameRef::new(
            f_off,
            u16::try_from(file_name.len()).expect("name too long"),
            true,
            f_ext,
        );
        file_rec.first_name.parent_frs = Into::into(ROOT_FRS);
        file_rec.first_stream.size = SizeInfo {
            length: file_size,
            allocated: file_size.next_multiple_of(512),
        };
        file_rec.stdinfo.flags = 0x20;

        let (drive, _, _) = build_compact_index(letter, &idx);
        backend.drives.push(drive);
    }
    backend
}

#[test]
fn multi_drive_merges_results_from_both_drives() {
    let mut backend = build_two_drive_backend();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend
        .search(SearchRequest::new("*", &mut filters))
        .expect("search runs");
    let drives: std::collections::HashSet<uffs_mft::platform::DriveLetter> =
        result.rows.iter().map(|row| row.drive).collect();
    assert!(
        drives.contains(&uffs_mft::platform::DriveLetter::C),
        "must include drive C results"
    );
    assert!(
        drives.contains(&uffs_mft::platform::DriveLetter::D),
        "must include drive D results"
    );
}

#[test]
fn multi_drive_sort_across_drives() {
    let mut backend = build_two_drive_backend();
    backend.sort_column = SortColumn::Size;
    backend.sort_desc = true;
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend
        .search(SearchRequest {
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    // data.csv (800) on D should come before report.txt (400) on C
    let first = result.rows.first().expect("first");
    assert_eq!(
        first.name(),
        "data.csv",
        "largest file across drives must be first"
    );
    assert_eq!(first.drive, uffs_mft::platform::DriveLetter::D);
}

#[test]
fn multi_drive_limit_caps_total() {
    let mut backend = build_two_drive_backend();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend
        .search(SearchRequest {
            result_limit: Some(2),
            ..SearchRequest::new("*", &mut filters)
        })
        .expect("search runs");
    assert!(
        result.rows.len() <= 2,
        "limit must cap across drives, got {}",
        result.rows.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// display_rows_to_dataframe
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn display_rows_to_dataframe_column_count_and_height() {
    let rows = vec![
        row(
            "file.txt",
            uffs_mft::platform::DriveLetter::C,
            1024,
            5_000_000,
            1_000_000,
        ),
        row(
            "data.csv",
            uffs_mft::platform::DriveLetter::D,
            2048,
            3_000_000,
            2_000_000,
        ),
    ];
    let df = display_rows_to_dataframe(&rows).expect("DataFrame creation must not fail");
    assert_eq!(df.height(), 2, "row count");
    assert_eq!(df.width(), 14, "column count must be 14");
}

#[test]
fn display_rows_to_dataframe_values_correct() {
    let rows = vec![row(
        "test.rs",
        uffs_mft::platform::DriveLetter::C,
        4096,
        9_000_000,
        1_000_000,
    )];
    let df = display_rows_to_dataframe(&rows).expect("DataFrame creation must not fail");

    // Check name column
    let name_col = df.column("name").expect("name column");
    let name_val = name_col
        .str()
        .expect("str chunked")
        .get(0)
        .expect("first value");
    assert_eq!(name_val, "test.rs");

    // Check size column
    let size_col = df.column("size").expect("size column");
    let size_val = size_col
        .u64()
        .expect("u64 chunked")
        .get(0)
        .expect("first value");
    assert_eq!(size_val, 4096);

    // Check drive column (formatted as "C:")
    let drive_col = df.column("drive").expect("drive column");
    let drive_val = drive_col
        .str()
        .expect("str chunked")
        .get(0)
        .expect("first value");
    assert_eq!(drive_val, "C:");
}

#[test]
fn display_rows_to_dataframe_path_only_extracts_directory() {
    let rows = vec![DisplayRow::new(
        0,
        uffs_mft::platform::DriveLetter::C,
        "C:\\Users\\john\\file.txt".to_owned(),
        100,
        false,
        0,
        0,
        0,
        0x20,
        512,
        0,
        0,
        0,
    )];
    let df = display_rows_to_dataframe(&rows).expect("DataFrame creation must not fail");

    let path_only_col = df.column("path_only").expect("path_only column");
    let val = path_only_col
        .str()
        .expect("str chunked")
        .get(0)
        .expect("first value");
    assert_eq!(
        val, "C:\\Users\\john\\",
        "path_only must be directory portion including trailing backslash"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Empty pattern returns empty
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn search_empty_pattern_returns_empty() {
    let mut backend = build_two_drive_backend();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend
        .search(SearchRequest::new("", &mut filters))
        .expect("search runs");
    assert!(
        result.rows.is_empty(),
        "empty pattern must return no results"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Regression: TreeSize sort must use treesize, not modified time (T67a)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sort_by_treesize_uses_treesize_not_modified() {
    let mut rows = vec![
        dir_row("small", uffs_mft::platform::DriveLetter::C, 5, 1_000),
        dir_row("large", uffs_mft::platform::DriveLetter::C, 10, 1_000_000),
        dir_row("medium", uffs_mft::platform::DriveLetter::C, 7, 100_000),
    ];
    sort_rows(&mut rows, FieldId::TreeSize, true, &[]);
    let sizes: Vec<u64> = rows.iter().map(|row| row.treesize).collect();
    assert_eq!(
        sizes,
        vec![1_000_000, 100_000, 1_000],
        "treesize desc: largest first"
    );
}

#[test]
fn sort_by_treesize_ascending() {
    let mut rows = vec![
        dir_row("large", uffs_mft::platform::DriveLetter::C, 10, 1_000_000),
        dir_row("small", uffs_mft::platform::DriveLetter::C, 5, 1_000),
        dir_row("medium", uffs_mft::platform::DriveLetter::C, 7, 100_000),
    ];
    sort_rows(&mut rows, FieldId::TreeSize, false, &[]);
    let sizes: Vec<u64> = rows.iter().map(|row| row.treesize).collect();
    assert_eq!(
        sizes,
        vec![1_000, 100_000, 1_000_000],
        "treesize asc: smallest first"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// search_index() free function — concurrent-safe API
// ═══════════════════════════════════════════════════════════════════════

/// Build a single-drive `DriveIndex` with three sibling files in the
/// root directory, added to the `MftIndex` in non-alphabetical order.
///
/// Files: `gamma.txt`, `alpha.txt`, `beta.txt` (insertion order).
///
/// Used to verify `PathOnly` sort's name-ASC tiebreaker within the
/// same folder (Windows Explorer `Folder` column convention): all
/// three files have identical `path_only` (`C:\`) so the sort must
/// fall through to the Name tiebreaker to break the tie.
fn build_siblings_fixture() -> DriveIndex {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // Insertion order (FRS 200, 201, 202) is deliberately non-alphabetical
    // so tree-walk `sort_indices_by_name` on the root's children has to
    // reorder them — and the final PathOnly sort must *preserve* that
    // name-ASC order via the Name tiebreaker even though the primary
    // `path_only` key is identical for all three.
    for (frs, name) in [(200, "gamma.txt"), (201, "alpha.txt"), (202, "beta.txt")] {
        let n_off = idx.add_name(name);
        let n_ext = idx.intern_extension(name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            n_off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            n_ext,
        );
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
    }

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    DriveIndex {
        drives: vec![Arc::new(drive)],
    }
}

/// Build a `DriveIndex` with two drives (C: and D:) for `search_index` tests.
fn build_two_drive_index() -> DriveIndex {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    let mut drives = Vec::new();
    for (letter, file_name, file_size) in [
        (uffs_mft::platform::DriveLetter::C, "report.txt", 400_u64),
        (uffs_mft::platform::DriveLetter::D, "data.csv", 800),
    ] {
        let mut idx = MftIndex::new(letter);
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS.into());
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = Into::into(ROOT_FRS);

        let f_off = idx.add_name(file_name);
        let f_ext = idx.intern_extension(file_name);
        let file_rec = idx.get_or_create(200.into());
        file_rec.first_name.name = IndexNameRef::new(
            f_off,
            u16::try_from(file_name.len()).expect("name too long"),
            true,
            f_ext,
        );
        file_rec.first_name.parent_frs = Into::into(ROOT_FRS);
        file_rec.first_stream.size = SizeInfo {
            length: file_size,
            allocated: file_size.next_multiple_of(512),
        };
        file_rec.stdinfo.flags = 0x20;

        let (drive, _, _) = build_compact_index(letter, &idx);
        drives.push(Arc::new(drive));
    }
    DriveIndex { drives }
}

#[test]
fn search_index_returns_results_from_both_drives() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    let drives: std::collections::HashSet<uffs_mft::platform::DriveLetter> =
        result.rows.iter().map(|row| row.drive).collect();
    assert!(
        drives.contains(&uffs_mft::platform::DriveLetter::C),
        "must include C: results"
    );
    assert!(
        drives.contains(&uffs_mft::platform::DriveLetter::D),
        "must include D: results"
    );
}

#[test]
fn search_index_drives_filter_excludes_non_matching() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            drives_filter: &[uffs_mft::platform::DriveLetter::C],
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.drive == uffs_mft::platform::DriveLetter::C),
        "drive filter must exclude D: results"
    );
    assert!(!result.rows.is_empty(), "must have at least one C: result");
}

// ── *.ext → ext-filter safety-net promotion tests ───────────────────
//
// These pin the dispatch-time rewrite in `search_index` that routes
// `pattern="*.txt"` through `numeric_top_n::ext_fast_path` instead of
// the trigram+glob path.  Paired with the parse-time rewrite in
// `uffs_client::protocol::cli_args::is_pure_ext_glob` (see its
// regression tests for shape-acceptance coverage).

/// Baseline: `*.txt` on the fixture with `report.txt` on C and
/// `data.csv` on D must return exactly the C file.
#[test]
fn search_index_promotes_ext_glob_and_returns_matching_results() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    // After the safety net mutates filters, the ext should be populated.
    assert_eq!(
        filters.extensions,
        vec!["txt".to_owned()],
        "safety net must push lowercase ext into filters"
    );
    // Fixture has exactly one .txt file on C.
    assert_eq!(result.rows.len(), 1, "expected one .txt result");
    let first = result.rows.first().expect("asserted non-empty above");
    assert!(
        first.path.ends_with("report.txt"),
        "expected report.txt, got: {}",
        first.path
    );
}

/// Parity: `*.txt` (with empty extensions) and `*` with
/// `extensions=["txt"]` must produce identical result sets.
#[test]
fn search_index_ext_glob_parity_with_explicit_extensions() {
    let index = build_two_drive_index();

    let mut filters_glob = super::super::filters::SearchFilters::default();
    let r_glob = search_index(
        &index,
        SearchRequest::new("*.txt", &mut filters_glob),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    let mut filters_explicit = super::super::filters::SearchFilters {
        extensions: vec!["txt".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let r_explicit = search_index(
        &index,
        SearchRequest::new("*", &mut filters_explicit),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    assert_eq!(
        r_glob.rows.len(),
        r_explicit.rows.len(),
        "row count parity between *.txt glob and --ext txt"
    );
    let glob_paths: std::collections::HashSet<_> =
        r_glob.rows.iter().map(|row| row.path.clone()).collect();
    let explicit_paths: std::collections::HashSet<_> =
        r_explicit.rows.iter().map(|row| row.path.clone()).collect();
    assert_eq!(
        glob_paths, explicit_paths,
        "result set parity between *.txt glob and --ext txt"
    );
}

/// Multi-segment `*.tar.gz` must stay on the trigram path and NOT
/// populate `extensions`.
#[test]
fn search_index_multi_segment_ext_not_promoted() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*.tar.gz", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.name().to_ascii_lowercase().ends_with(".tar.gz")),
        "the glob still decides the rows"
    );
    assert!(
        filters.extensions.is_empty(),
        "safety net must NOT promote multi-segment ext globs — filters.extensions should stay empty"
    );
}

/// Case-sensitive `*.TXT` must NOT promote (ext index is case-folded,
/// would produce wrong semantics for strict-case callers).
#[test]
fn search_index_case_sensitive_ext_glob_not_promoted() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            case_sensitive: true,
            ..SearchRequest::new("*.TXT", &mut filters)
        },
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert!(
        result.rows.iter().all(|row| row
            .name()
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext == "TXT")),
        "a case-sensitive glob keeps only upper-case .TXT names"
    );
    assert!(
        filters.extensions.is_empty(),
        "case-sensitive *.TXT must stay on trigram path — filters.extensions should stay empty"
    );
}

/// Explicit `extensions` pre-populated must NOT be clobbered even if
/// pattern is `*.<other>`.
#[test]
fn search_index_explicit_extensions_not_clobbered() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters {
        extensions: vec!["csv".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert!(
        result.rows.is_empty(),
        "a .txt pattern and a csv-only extension filter cannot both hold"
    );
    // Safety net must leave existing extensions alone.
    assert_eq!(
        filters.extensions,
        vec!["csv".to_owned()],
        "explicit extensions must not be clobbered by safety net"
    );
}

/// Build a single C: drive where many files share the substring `pcl5`
/// but only the LAST record (highest index) has extension `pdf`:
/// `pcl5_00.dll` … `pcl5_07.dll` then `pcl5_report.pdf`.
///
/// Mirrors the field report: `PCL5-Assessment-*.pdf` sits among many
/// `PCL5*.DLL` system files, and the pdf is not among the first matches.
fn build_substring_then_extension_fixture() -> MultiDriveBackend {
    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    let letter = uffs_mft::platform::DriveLetter::C;
    let mut mft = MftIndex::new(letter);
    let root_off = mft.add_name(".");
    let root = mft.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // Eight `pcl5_NN.dll` files first (FRS 200..208), then one pdf last
    // (FRS 999 → highest record index). All contain the substring "pcl5".
    let add = |index: &mut MftIndex, frs: u64, name: &str, size: u64| {
        let off = index.add_name(name);
        let ext = index.intern_extension(name);
        let rec = index.get_or_create(frs.into());
        rec.first_name.name =
            IndexNameRef::new(off, u16::try_from(name.len()).expect("len"), true, ext);
        rec.first_name.parent_frs = Into::into(ROOT_FRS);
        rec.first_stream.size = SizeInfo {
            length: size,
            allocated: size.next_multiple_of(512),
        };
        rec.stdinfo.flags = 0x20;
    };
    for seq in 0..8_u64 {
        add(&mut mft, 200 + seq, &format!("pcl5_{seq:02}.dll"), 1000);
    }
    add(&mut mft, 999, "pcl5_report.pdf", 5000);

    let mut backend = MultiDriveBackend::new();
    let (drive, _, _) = build_compact_index(letter, &mft);
    backend.drives.push(drive);
    backend
}

/// Regression (v0.6.13 field report): `uffs pcl5 --ext pdf --limit 5`
/// returned nothing even though `pcl5_report.pdf` exists, matches the
/// substring, and has extension `pdf`.
///
/// Root cause this pins: the per-drive substring scan truncates to
/// `--limit` BEFORE the extension filter runs (`collect_match_indices`
/// stops at `limit`, then `apply_search_filters` filters the survivors).
/// With 8 `.dll` matches ahead of the single `.pdf`, a limit of 5 fills
/// the result with `.dll`s, then `--ext pdf` removes all of them.
///
/// The unlimited query in the same test proves the filter logic itself
/// is correct — the bug is purely the limit-before-filter ordering.
#[test]
fn search_substring_with_extension_filter_survives_limit() {
    // Diagnostic 1: UNLIMITED — the ext filter alone works, pdf is found.
    let mut backend_unlimited = build_substring_then_extension_fixture();
    let mut filters_unlimited = super::super::filters::SearchFilters {
        extensions: vec!["pdf".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let unlimited = backend_unlimited
        .search(SearchRequest::new("pcl5", &mut filters_unlimited))
        .expect("search runs");
    assert!(
        unlimited
            .rows
            .iter()
            .any(|row| row.name() == "pcl5_report.pdf"),
        "sanity: unlimited 'pcl5 --ext pdf' must find the pdf (got {:?})",
        unlimited
            .rows
            .iter()
            .map(DisplayRow::name)
            .collect::<Vec<_>>()
    );

    // Diagnostic 2: LIMIT 5 — the pdf must STILL be found. On the buggy
    // code the limit truncates the 8 .dll matches before the ext filter,
    // so this returns zero rows.
    let mut backend = build_substring_then_extension_fixture();
    let mut filters_limited = super::super::filters::SearchFilters {
        extensions: vec!["pdf".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let limited = backend
        .search(SearchRequest {
            result_limit: Some(5),
            ..SearchRequest::new("pcl5", &mut filters_limited)
        })
        .expect("search runs");
    assert!(
        limited
            .rows
            .iter()
            .any(|row| row.name() == "pcl5_report.pdf"),
        "'pcl5 --ext pdf --limit 5' must find the pdf — the extension \
         filter must be applied BEFORE the per-drive limit truncation \
         (got {:?})",
        limited
            .rows
            .iter()
            .map(DisplayRow::name)
            .collect::<Vec<_>>()
    );
}

// ── *.ext + `--hide-system` / `--hide-ads` fast-path regression pins ─
//
// 2026-04-19 regression fix: the `is_ext_only()` gate that admits the
// `ExtensionIndex` fast path in `numeric_top_n::collect_global_top_n_numeric`
// used to reject the filter set whenever `hide_system` or `hide_ads`
// was true.  Every `uffs *.<ext> --hide-system --hide-ads` query (the
// default cross-tool bench shape — see
// `scripts/windows/cross-tool-benchmark.rs`) therefore fell back to an
// O(N) scan of every record on every loaded drive, costing ~216 ms on
// Drive D's 7 M-record corpus for an 11-row `*.dbt` result.
//
// The fix pushes both predicates into the fast-path loop: they are
// applied per-candidate after the CSR `ext_index` bucket has narrowed
// the set to O(K).  The regression pins below build a three-record
// fixture where exactly one row must survive both filters, proving:
//
//   1. The fast path now fires under `--hide-system --hide-ads` (otherwise the
//      extension safety-net would not have populated `filters.extensions`).
//   2. The inline `hide_system` check excludes only *true* reserved NTFS
//      metafiles (exact names like `$MFT`), NOT every `$`-prefixed file — see
//      `search_index_hide_system_excludes_true_metafiles_only`.
//   3. The inline `hide_ads` check still excludes names containing `:`
//      (Alternate Data Streams).
//
// Paired unit tests live in `filters/tests.rs::is_ext_only_*`.

/// Build a single C: drive with three `.dbt` records covering the full
/// fast-path hide-filter matrix: a system metafile, a normal file, and
/// an ADS-named file.  Used by the hide-system / hide-ads regression
/// pins immediately below.
fn build_dbt_triple_fixture() -> DriveIndex {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS};

    use crate::compact::build_compact_index;

    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // Three candidates for a `*.dbt` query.  `frs` values are arbitrary
    // but must be distinct.
    //
    // The ADS-shaped entry places the colon *before* the extension dot
    // (`stream:ads.dbt`, not `stream.dbt:ads`).  This is a fixture
    // construction detail, not an NTFS semantic: `MftIndex::intern_extension`
    // does `rfind('.')`, so a trailing `:ads` after the extension would
    // re-assign the record to the `"dbt:ads"` extension bucket and it
    // would never enter the `"dbt"` fast path we are trying to test.
    // With the colon first, the record is genuinely a `.dbt` file whose
    // *name* also contains `:` — exactly the case the inline
    // `hide_ads` check must catch.
    for (frs, name) in [
        // Ordinary `$`-prefixed file (like `$Recycle.Bin` or a WinSxS
        // `$$_*.cdf-ms` filemap): NOT a reserved metafile, so `hide_system`
        // must KEEP it.  Pre-fix the filter wrongly excluded any `$`-name.
        (200_u64, "$secret.dbt"),
        (201, "normal.dbt"),     // plain file    → must survive all filters
        (202, "stream:ads.dbt"), // colon in name → hide_ads must exclude
    ] {
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
        rec.stdinfo.flags = 0x20;
    }

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    DriveIndex {
        drives: vec![Arc::new(drive)],
    }
}

/// Regression pin — 2026-04-19 (perf) + 2026-06-11 (semantics):
/// `*.dbt --hide-system --hide-ads` must return exactly `$secret.dbt` and
/// `normal.dbt`.  `hide_ads` removes the colon-named `stream:ads.dbt`;
/// `hide_system` removes NOTHING here, because `$secret.dbt` is an ordinary
/// `$`-prefixed file, not a reserved NTFS metafile.  (Pre-perf-fix the query
/// ran an O(N) scan; pre-semantics-fix `hide_system` wrongly dropped
/// `$secret.dbt` and the result was just `normal.dbt`.)
#[test]
fn search_index_ext_fast_path_honors_hide_system_and_hide_ads() {
    let index = build_dbt_triple_fixture();
    let mut filters = super::super::filters::SearchFilters {
        hide_system: true,
        hide_ads: true,
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*.dbt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    assert_eq!(
        filters.extensions,
        vec!["dbt".to_owned()],
        "ext-glob safety net must promote *.dbt → extensions=[dbt]"
    );
    let names: std::collections::HashSet<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        names,
        ["$secret.dbt", "normal.dbt"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "hide_ads must drop stream:ads.dbt; hide_system must KEEP the ordinary \
         $-file $secret.dbt; got {names:?}"
    );
}

/// Regression pin — 2026-04-19: with both hide flags OFF the same
/// `*.dbt` query must return all three records.  This proves the
/// hide-system / hide-ads exclusions in the fast-path loop are gated
/// on the flag (not unconditional), keeping the default semantics of
/// "return everything that matches the extension" intact.
#[test]
fn search_index_ext_fast_path_returns_all_when_hide_flags_off() {
    let index = build_dbt_triple_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*.dbt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    let names: std::collections::HashSet<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        names.len(),
        3,
        "all three .dbt records must be returned when no hide flags are set; got {names:?}"
    );
    assert!(
        names.contains("$secret.dbt"),
        "system metafile must be returned by default"
    );
    assert!(names.contains("normal.dbt"), "normal file must be returned");
    assert!(
        names.contains("stream:ads.dbt"),
        "ADS-named record must be returned by default"
    );
}

/// Regression pin — 2026-06-11: `hide_system` must NOT hide ordinary
/// `$`-prefixed files.  A reserved NTFS metafile is an exact name
/// (`$MFT`, `$Boot`, …); `$secret.dbt` is just a file that happens to start
/// with `$`, so all three records survive `hide_system` alone.
#[test]
fn search_index_ext_fast_path_hide_system_keeps_ordinary_dollar_files() {
    let index = build_dbt_triple_fixture();
    let mut filters = super::super::filters::SearchFilters {
        hide_system: true,
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*.dbt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    let names: std::collections::HashSet<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert!(
        names.contains("$secret.dbt"),
        "hide_system must KEEP the ordinary $-file $secret.dbt; got {names:?}"
    );
    assert!(
        names.contains("normal.dbt") && names.contains("stream:ads.dbt"),
        "hide_system alone must keep every non-metafile record; got {names:?}"
    );
}

/// Regression pin — 2026-06-11: `hide_system` MUST still exclude *true*
/// reserved NTFS metafiles (exact names like `$MFT`), while keeping ordinary
/// `$`-prefixed files.  Built at a non-reserved FRS so the build-time
/// `PathResolver` metafile filter does not drop `$MFT` before we can test the
/// runtime filter.
#[test]
fn search_index_hide_system_excludes_true_metafiles_only() {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS};

    use crate::compact::build_compact_index;

    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    for (frs, name) in [
        (300_u64, "$MFT"),     // true metafile      → hide_system excludes
        (301, "$Recycle.Bin"), // ordinary $-dir     → must survive
        (302, "report.txt"),   // plain file         → must survive
    ] {
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
        rec.stdinfo.flags = 0x20;
    }

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    let index = DriveIndex {
        drives: vec![Arc::new(drive)],
    };

    let mut filters = super::super::filters::SearchFilters {
        hide_system: true,
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    let names: std::collections::HashSet<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert!(
        !names.contains("$MFT"),
        "hide_system must exclude the true metafile $MFT; got {names:?}"
    );
    assert!(
        names.contains("$Recycle.Bin") && names.contains("report.txt"),
        "hide_system must keep ordinary $-files and plain files; got {names:?}"
    );
}

/// Regression pin — 2026-04-19: `hide_ads` alone must only exclude
/// colon-containing names, not `$`-prefixed records.
#[test]
fn search_index_ext_fast_path_hide_ads_only_excludes_colon_names() {
    let index = build_dbt_triple_fixture();
    let mut filters = super::super::filters::SearchFilters {
        hide_ads: true,
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*.dbt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    let names: std::collections::HashSet<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert!(
        !names.contains("stream:ads.dbt"),
        "hide_ads must exclude colon-containing names; got {names:?}"
    );
    assert!(
        names.contains("$secret.dbt") && names.contains("normal.dbt"),
        "hide_ads alone must keep $-prefixed and plain records; got {names:?}"
    );
}

// ── ext_rare fast-path short-circuit + dotless fallback tests ─────
//
// Regression pins — 2026-04-21.  Two compounding bugs were observed
// on `*.dbt` against a C: drive that does not index any `.dbt` file
// (benchmark `ext_rare` on the cross-tool harness, see
// `@/Users/rnio/Private/Github/UltraFastFileSearch/LOG/Output_cache_new:323,
// 785`):
//
// Bug A (perf):   the numeric top-N fast path still ran a full scan
//                 even though `resolve_ext_ids_for_drive` resolved to
//                 the empty set, producing ≥ 500 ms of wasted work.
// Bug B (correct): the `matches_record` fallback (exercised by the
//                 scan path when resolved IDs are unpopulated) used
//                 `rsplit('.').next()` which returned the whole name
//                 for dotless inputs, so a directory literally named
//                 `dbt` matched `--ext dbt` and was emitted as a
//                 spurious row.
//
// The fixture below plants a dotless directory called `dbt` next to a
// `notes.txt` file.  Post-fix, `*.dbt` must return zero rows on this
// drive; pre-fix it returned the `dbt` directory.

/// Build a drive that contains NO `.dbt` files — only a dotless
/// directory called `dbt` and a `notes.txt` file.  Used by the
/// extension short-circuit / fallback regression tests.
fn build_no_dbt_fixture() -> DriveIndex {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS};

    use crate::compact::build_compact_index;

    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // Dotless directory literally named `dbt`.  `intern_extension`
    // assigns this `extension_id = 0` (no dot), so the resolved-ID
    // fast path must NOT find it under the `dbt` bucket.  Pre-Bug-B
    // fix the fallback incorrectly extracted `"dbt"` from the name
    // and matched it anyway.
    let dbt_name = "dbt";
    let dbt_off = idx.add_name(dbt_name);
    let dbt_ext = idx.intern_extension(dbt_name);
    let dbt_rec = idx.get_or_create(300.into());
    dbt_rec.first_name.name = IndexNameRef::new(
        dbt_off,
        u16::try_from(dbt_name.len()).expect("name too long"),
        true,
        dbt_ext,
    );
    dbt_rec.first_name.parent_frs = Into::into(ROOT_FRS);
    dbt_rec.stdinfo.flags = 0x10; // DIRECTORY
    dbt_rec.stdinfo.set_directory(true);

    // A regular `.txt` file so the drive isn't empty; its presence
    // also guarantees the scan-path code is reachable.
    let txt_name = "notes.txt";
    let txt_off = idx.add_name(txt_name);
    let txt_ext = idx.intern_extension(txt_name);
    let txt_rec = idx.get_or_create(301.into());
    txt_rec.first_name.name = IndexNameRef::new(
        txt_off,
        u16::try_from(txt_name.len()).expect("name too long"),
        true,
        txt_ext,
    );
    txt_rec.first_name.parent_frs = Into::into(ROOT_FRS);
    txt_rec.stdinfo.flags = 0x20;

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    DriveIndex {
        drives: vec![Arc::new(drive)],
    }
}

/// Bug A + B end-to-end: `*.dbt` on a drive with NO `.dbt` files
/// (only a dotless `dbt` directory) must return zero rows.  Pre-fix
/// this returned the `dbt` directory via the fallback extraction
/// bug, and also ran a full scan before filtering (the perf half of
/// the regression).
#[test]
fn search_index_ext_rare_zero_results_when_drive_lacks_extension() {
    let index = build_no_dbt_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*.dbt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    assert_eq!(
        filters.extensions,
        vec!["dbt".to_owned()],
        "ext-glob safety net must promote *.dbt → extensions=[dbt]"
    );
    assert!(
        result.rows.is_empty(),
        "no rows may survive *.dbt on a drive without any .dbt extension; \
         got {:?}",
        result
            .rows
            .iter()
            .map(|row| row.name().to_owned())
            .collect::<Vec<_>>()
    );
}

/// Bug B companion: even with the extension bucket populated by the
/// resolve step, a dotless name that *happens* to equal the ext token
/// must never slip through.  This pins the resolved-ID path (the fast
/// path under Bug A's short-circuit) by asking for `*.txt` on the
/// same fixture — the `dbt` directory must stay excluded and only
/// `notes.txt` should be returned.
#[test]
fn search_index_ext_rare_dotless_dir_does_not_leak_into_other_extension() {
    let index = build_no_dbt_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");

    let names: std::collections::HashSet<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "only the one .txt file may match *.txt on this drive; got {names:?}"
    );
    assert!(
        names.contains("notes.txt"),
        "expected notes.txt, got {names:?}"
    );
    assert!(
        !names.contains("dbt"),
        "dotless directory 'dbt' must never match *.txt"
    );
}

// ── <letter>: → drive-filter promotion safety-net tests ────────────
//
// These pin the dispatch-time rewrite in `search_index` that promotes
// bare drive prefixes (`C:<rest>`) to the drive filter + stripped
// pattern, composing with the ext-glob promotion where applicable.
// Paired with the parse-time rewrite in
// `uffs_client::protocol::cli_args::into_search_params` (see its
// regression tests for shape-acceptance coverage).

/// Composition: `C:*.txt` with empty `drives_filter` must promote both
/// to drive=C AND to ext=txt, finding the single C-side `report.txt`.
#[test]
fn search_index_promotes_drive_prefix_with_ext_glob() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("C:*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        filters.extensions,
        vec!["txt".to_owned()],
        "ext-glob safety net must fire on the stripped rest (*.txt)"
    );
    assert_eq!(
        result.rows.len(),
        1,
        "drive-prefix + ext promotion must find exactly report.txt on C"
    );
    let first = result.rows.first().expect("asserted non-empty above");
    assert_eq!(
        first.drive,
        uffs_mft::platform::DriveLetter::C,
        "result must be from drive C only"
    );
    assert!(
        first.path.ends_with("report.txt"),
        "expected report.txt, got: {}",
        first.path
    );
}

/// Narrowing: `D:*.txt` must promote to drive=D AND ext=txt.  D only
/// has `data.csv` in the fixture, so the result set is empty — proving
/// the drive narrowing actually applied (otherwise we would have found
/// `report.txt` on C via the ext-index).
#[test]
fn search_index_drive_prefix_narrows_away_other_drives() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("D:*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        filters.extensions,
        vec!["txt".to_owned()],
        "ext promotion must still fire after drive strip"
    );
    assert!(
        result.rows.is_empty(),
        "D drive has no .txt files — safety net must have narrowed away from C's report.txt; got {} rows: {:?}",
        result.rows.len(),
        result
            .rows
            .iter()
            .map(|row| (row.drive, row.path.clone()))
            .collect::<Vec<_>>()
    );
}

/// Case-insensitive drive letter: `c:*.txt` (lowercase) must still
/// promote to drive=C (uppercase normalisation) and find `report.txt`.
#[test]
fn search_index_drive_prefix_case_insensitive_letter() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("c:*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        result.rows.len(),
        1,
        "lowercase drive letter must still match drive C"
    );
    assert_eq!(
        result.rows.first().expect("one row").drive,
        uffs_mft::platform::DriveLetter::C,
        "result must be from drive C"
    );
}

/// Explicit `drives_filter` must NOT be clobbered by the safety net.
/// Passing `drives_filter=['D']` with pattern `C:*.txt` keeps the D
/// filter and leaves the pattern unchanged — the trigram path then
/// runs on D with needle `C:*.txt`, finding nothing (file names on
/// NTFS cannot contain `:`).
#[test]
fn search_index_drive_prefix_explicit_filter_not_clobbered() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            drives_filter: &[uffs_mft::platform::DriveLetter::D],
            ..SearchRequest::new("C:*.txt", &mut filters)
        },
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert!(
        filters.extensions.is_empty(),
        "ext promotion must NOT fire (pattern still contains `C:` prefix because drives_filter was non-empty)"
    );
    assert!(
        result.rows.is_empty(),
        "explicit drives_filter=['D'] must win; pattern `C:*.txt` cannot match names on D; got {:?}",
        result
            .rows
            .iter()
            .map(|row| (row.drive, row.path.clone()))
            .collect::<Vec<_>>()
    );
}

/// Regression: `pattern="*"` with non-empty `extensions` and
/// `sort=PathOnly` must return ONLY rows matching the extension
/// filter.  Pre-fix, the path-sort tree walk in
/// `collect_global_top_n` (`FieldId::Path` | `FieldId::PathOnly`
/// branch) pushed every record it visited without consulting
/// `search_filters` or `filter_mode`, so requests like
/// `uffs *.exe --sort path_only` (after Run 9's `*.<ext>` rewrite to
/// `pattern="*" + extensions=["exe"]`) returned every record
/// including non-`.exe` directories and files.
///
/// Fixture has `report.txt` on C and `data.csv` on D.  Filtering by
/// `extensions=["txt"]` must yield exactly the C-side `report.txt`,
/// not every record from the C+D tree walk.
#[test]
fn search_index_path_only_sort_honors_extension_filter() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters {
        extensions: vec!["txt".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(10),
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false, // ASC
        &[],
    )
    .expect("search runs");
    // Extension filter is case-folded at the daemon, but the fixture
    // writes lowercase names so a case-sensitive `ends_with` is fine
    // here — we explicitly want to detect the bug-case where records
    // with no extension or a different extension slip through.
    let ends_with_txt = |row: &DisplayRow| {
        let name = row.name();
        name.len() >= 4
            && name
                .get(name.len() - 4..)
                .is_some_and(|ext| ext.eq_ignore_ascii_case(".txt"))
    };
    assert!(
        result.rows.iter().all(ends_with_txt),
        "path-sort tree walk must respect search_filters.extensions; \
         got non-.txt rows: {:?}",
        result
            .rows
            .iter()
            .filter(|row| !ends_with_txt(row))
            .map(|row| row.name().to_owned())
            .collect::<Vec<_>>()
    );
    // Sanity: we actually got the expected .txt row.
    assert!(
        result.rows.iter().any(|row| row.name() == "report.txt"),
        "expected report.txt in results"
    );
}

/// Regression: `pattern="*"` with `sort=PathOnly` must return rows
/// actually sorted by parent-directory (`path_only`) ASC, not by
/// tree-walk order.  Tree-walk order is full-path-ASC, which is NOT
/// equivalent to `path_only`-ASC when records interleave across
/// depths.
///
/// The ASC-pair assertion below mirrors the api-validation `T67f`
/// check (strip the filename suffix, trim trailing `\`, compare
/// adjacent pairs).  Any ordering violation fails the test
/// deterministically.
#[test]
fn search_index_path_only_sort_returns_path_only_asc_order() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(100),
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false, // ASC
        &[],
    )
    .expect("search runs");
    // Verify every adjacent pair is in path_only-ASC order.
    //
    // Mirrors the api-validation T67f test's per-row `_path_only`
    // computation: strip filename from path, trim trailing `\`.
    let path_only_of = |row: &DisplayRow| -> String {
        let name = row.name();
        row.path
            .strip_suffix(name)
            .unwrap_or(&row.path)
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    let keys: Vec<String> = result.rows.iter().map(path_only_of).collect();
    for window in keys.windows(2) {
        if let [prev, next] = window {
            assert!(
                prev <= next,
                "path_only ASC violation: '{prev}' > '{next}' in {keys:?}"
            );
        }
    }
}

/// Regression: files in the same folder must sort name-ASC as the
/// tiebreaker after `path_only` (Windows Explorer `Folder` column
/// convention).  Fixture inserts three files `gamma.txt`, `alpha.txt`,
/// `beta.txt` in that non-alphabetical order into the root directory.
/// All three share `path_only="C:\"`, so the primary sort key is
/// equal and the Name tiebreaker in `sort_rows` must kick in and
/// order them `alpha`, `beta`, `gamma`.
///
/// This pins the contract declared in §`PathOnly` sort docs that
/// intra-folder ordering matches Windows Explorer (name-ASC).  If a
/// future refactor of `sort_rows` drops the Name tiebreaker this test
/// will fail deterministically.
#[test]
fn search_index_path_only_sort_name_asc_within_same_folder() {
    let index = build_siblings_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false, // ASC
        &[],
    )
    .expect("search runs");
    let file_names: Vec<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        file_names,
        vec![
            "alpha.txt".to_owned(),
            "beta.txt".to_owned(),
            "gamma.txt".to_owned(),
        ],
        "files in same folder must sort name-ASC after path_only \
         (Windows Folder-column convention)"
    );
}

/// Regression pin — 2026-04-20: `PathOnly` sort + `--ext <target>`
/// must go through the ext-index fast path
/// (`path_only_top_n::collect_path_only_via_ext_index`), which
/// short-circuits the full-tree walk entirely.  That fast path must
/// still preserve the same name-ASC tiebreaker semantics as the
/// tree-walk variant: all three siblings share `path_only="C:\"`
/// so output order is determined purely by the Name tiebreaker.
///
/// Before the fast path existed, the tree walk achieved this by
/// `sort_indices_by_name` on each directory's children.  The fast
/// path achieves it by calling `backend::sort_rows(.., PathOnly, ..)`
/// on the materialised `DisplayRow`s, which encodes the same
/// name-ASC tiebreaker contract.  This test pins the parity.
///
/// If a future refactor of the fast path drops the final
/// `sort_rows` call (or swaps it for a bare `path_only` compare that
/// omits the Name tiebreaker) this assertion will fail
/// deterministically.
#[test]
fn search_index_path_only_ext_fast_path_preserves_name_asc_tiebreaker() {
    let index = build_siblings_fixture();
    let mut filters = super::super::filters::SearchFilters {
        extensions: vec!["txt".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false, // ASC
        &[],
    )
    .expect("search runs");
    let file_names: Vec<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        file_names,
        vec![
            "alpha.txt".to_owned(),
            "beta.txt".to_owned(),
            "gamma.txt".to_owned(),
        ],
        "ext-fast-path output must still honour the name-ASC tiebreaker \
         (intra-folder order = Windows Explorer Folder column convention)"
    );
}

/// Regression: `pattern="*"` with `filter_mode=FilesOnly` and
/// `sort=PathOnly` must exclude directory rows.  Same root cause as
/// the extensions test above — the path-sort tree walk was pushing
/// every record including directories.
#[test]
fn search_index_path_only_sort_honors_filter_mode_files_only() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(10),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false,
        &[],
    )
    .expect("search runs");
    assert!(
        result.rows.iter().all(|row| !row.is_directory),
        "path-sort tree walk with FilesOnly must exclude directories; \
         got directory rows: {:?}",
        result
            .rows
            .iter()
            .filter(|row| row.is_directory)
            .map(|row| row.path.clone())
            .collect::<Vec<_>>()
    );
}

/// Build a single-drive `DriveIndex` with a three-level nested tree,
/// used by the `path_only` two-phase walk regression tests below.
///
/// Tree (insertion order is deliberately non-alphabetical so
/// `sort_indices_by_name` has real work to do):
/// ```text
/// C:\
///   ├── alpha.txt       path_only = C:\
///   ├── beta\           path_only = C:\   (directory)
///   │   ├── a.txt       path_only = C:\beta\
///   │   ├── b.txt       path_only = C:\beta\
///   │   └── c\          path_only = C:\beta\   (directory)
///   │       └── x.txt   path_only = C:\beta\c\
///   └── zeta.txt        path_only = C:\
/// ```
fn build_nested_fixture() -> DriveIndex {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    fn make_file(idx: &mut MftIndex, frs: u64, name: &str, parent: u64) {
        let n_off = idx.add_name(name);
        let n_ext = idx.intern_extension(name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            n_off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            n_ext,
        );
        rec.first_name.parent_frs = Into::into(parent);
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
    }

    fn make_dir(idx: &mut MftIndex, frs: u64, name: &str, parent: u64) {
        let n_off = idx.add_name(name);
        let n_ext = idx.intern_extension(name);
        let rec = idx.get_or_create(frs.into());
        rec.first_name.name = IndexNameRef::new(
            n_off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            n_ext,
        );
        rec.first_name.parent_frs = Into::into(parent);
        rec.stdinfo.set_directory(true);
    }

    let mut idx = MftIndex::new(uffs_mft::platform::DriveLetter::C);
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS.into());
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = Into::into(ROOT_FRS);

    // Root-level entries (non-alphabetical FRS order).
    make_file(&mut idx, 202, "zeta.txt", ROOT_FRS);
    make_file(&mut idx, 200, "alpha.txt", ROOT_FRS);
    make_dir(&mut idx, 201, "beta", ROOT_FRS);

    // `beta` contents (non-alphabetical FRS order).
    make_file(&mut idx, 211, "b.txt", 201);
    make_file(&mut idx, 210, "a.txt", 201);
    make_dir(&mut idx, 212, "c", 201);

    // `beta/c` contents.
    make_file(&mut idx, 220, "x.txt", 212);

    let (drive, _, _) = build_compact_index(uffs_mft::platform::DriveLetter::C, &idx);
    DriveIndex {
        drives: vec![Arc::new(drive)],
    }
}

/// Regression: `sort=PathOnly` ASC with a nested tree must emit rows
/// grouped by `path_only` lexicographically, with a name-ASC
/// tiebreaker inside each group.
///
/// Pre-fix the walker used naive pre-order DFS, which is equivalent
/// to full-path-ASC — *not* `path_only`-ASC.  For the fixture below
/// DFS yields `alpha, beta, a, b, c, x, zeta` (zeta buried at the
/// end), whereas the correct `path_only`-ASC order groups all C:\
/// entries first (`alpha, beta, zeta`), then C:\beta\ entries
/// (`a, b, c`), then C:\beta\c\ (`x`).
#[test]
fn search_index_path_only_sort_asc_nested_tree() {
    let index = build_nested_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false, // ASC
        &[],
    )
    .expect("search runs");
    let names: Vec<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            // path_only = C:\   (name-ASC)
            "alpha.txt".to_owned(),
            "zeta.txt".to_owned(),
            // path_only = C:\beta\   (name-ASC)
            "a.txt".to_owned(),
            "b.txt".to_owned(),
            // path_only = C:\beta\c\
            "x.txt".to_owned(),
        ],
        "path_only ASC must group by parent-directory with name-ASC tiebreaker"
    );
}

/// Regression: `sort=PathOnly` DESC must emit rows in reverse
/// `path_only` order — deepest `path_only` first — but with a
/// name-ASC tiebreaker *inside* each group.  The Name tiebreaker is
/// NOT flipped by `sort_desc` (see `sort_rows` contract).
#[test]
fn search_index_path_only_sort_desc_nested_tree() {
    let index = build_nested_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(100),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        true, // DESC
        &[],
    )
    .expect("search runs");
    let names: Vec<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            // path_only = C:\beta\c\   (deepest group first in DESC)
            "x.txt".to_owned(),
            // path_only = C:\beta\   (still name-ASC within group)
            "a.txt".to_owned(),
            "b.txt".to_owned(),
            // path_only = C:\   (shallowest group last)
            "alpha.txt".to_owned(),
            "zeta.txt".to_owned(),
        ],
        "path_only DESC groups deepest first; name tiebreaker stays ASC"
    );
}

/// Regression: `sort=PathOnly` with a small `limit` must stop walking
/// as soon as the limit is reached.
///
/// This is the whole point of the two-phase walk — the earlier
/// implementation did `collect_path_sorted_top_n(drives, usize::MAX,
/// ..)` + full sort + truncate, which resolved every matching
/// record's path even when only the top-N were needed.
///
/// The test only pins behavioural correctness (exact truncated
/// result).  The timing win is verified end-to-end by the
/// `api-validation` / `mcp-validation` scenario suite.
#[test]
fn search_index_path_only_sort_respects_limit() {
    let index = build_nested_fixture();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            result_limit: Some(2),
            filter_mode: FilterMode::FilesOnly,
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::PathOnly,
        false, // ASC
        &[],
    )
    .expect("search runs");
    let names: Vec<String> = result
        .rows
        .iter()
        .map(|row| row.name().to_owned())
        .collect();
    assert_eq!(
        names,
        vec!["alpha.txt".to_owned(), "zeta.txt".to_owned()],
        "limit=2 must emit only the first two path_only-ASC rows"
    );
}

/// `C:\*.txt` now scopes to drive C and globs.  The drive prefix is
/// split off, the single `*.txt` segment collapses to a name glob, and
/// the ext-glob safety net promotes it to `ext=["txt"]`.  (Previously
/// the `c:` token was mistaken for a directory segment by the tree
/// walker and the search returned nothing.)
#[test]
fn search_index_drive_root_glob_scopes_drive_and_promotes_ext() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("C:\\*.txt", &mut filters),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        filters.extensions,
        vec!["txt".to_owned()],
        "drive-prefix split + ext-glob promotion must fire on C:\\*.txt"
    );
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.drive == uffs_mft::platform::DriveLetter::C),
        "results must be scoped to drive C"
    );
    assert!(
        result
            .rows
            .iter()
            .any(|row| row.path.ends_with("report.txt")),
        "C:\\*.txt must find report.txt on drive C"
    );
}

#[test]
fn search_index_concurrent_calls_do_not_interfere() {
    use alloc::sync::Arc;

    let index = Arc::new(build_two_drive_index());
    let idx1 = Arc::clone(&index);
    let idx2 = Arc::clone(&index);

    let (r1, r2) = rayon::join(
        || {
            let mut filters = super::super::filters::SearchFilters::default();
            search_index(
                &idx1,
                SearchRequest::new("*", &mut filters),
                FieldId::Size,
                true,
                &[],
            )
            .expect("search runs")
        },
        || {
            let mut filters = super::super::filters::SearchFilters::default();
            search_index(
                &idx2,
                SearchRequest::new("*", &mut filters),
                FieldId::Modified,
                false,
                &[],
            )
            .expect("search runs")
        },
    );
    assert!(!r1.rows.is_empty(), "concurrent search 1 must return rows");
    assert!(!r2.rows.is_empty(), "concurrent search 2 must return rows");
}

// ═══════════════════════════════════════════════════════════════════════
// Boolean flag sorting — compare_by_column, sort_rows, field_to_attr_bit
// ═══════════════════════════════════════════════════════════════════════

/// Build a `DisplayRow` with explicit NTFS flags.
///
/// `is_directory` is derived from `flags & 0x0010`.  All rows share
/// identical timestamps (`modified = 5000`) so any sort that falls back
/// to `modified` instead of the flag value will produce unstable order.
fn flagged_row(name: &str, flags: u32) -> DisplayRow {
    DisplayRow::new(
        0,
        uffs_mft::platform::DriveLetter::C,
        format!("C:\\{name}"),
        100,
        flags & 0x0010 != 0,
        5000,
        5000,
        5000,
        flags,
        512,
        0,
        0,
        0,
    )
}

// ── field_to_attr_bit ────────────────────────────────────────────────

/// Verify every boolean `FieldId` maps to the documented NTFS
/// `FILE_ATTRIBUTE_*` constant.  A wrong value here silently breaks
/// sorting for that flag.
#[test]
fn field_to_attr_bit_matches_ntfs_constants() {
    use crate::search::sorting::field_to_attr_bit;

    let expected: &[(FieldId, u32)] = &[
        (FieldId::ReadOnly, 0x0001),
        (FieldId::Hidden, 0x0002),
        (FieldId::System, 0x0004),
        (FieldId::DirectoryFlag, 0x0010),
        (FieldId::Archive, 0x0020),
        (FieldId::Temporary, 0x0100),
        (FieldId::Sparse, 0x0200),
        (FieldId::Reparse, 0x0400),
        (FieldId::Compressed, 0x0800),
        (FieldId::Offline, 0x1000),
        (FieldId::NotIndexed, 0x2000),
        (FieldId::Encrypted, 0x4000),
        (FieldId::Integrity, 0x8000),
        (FieldId::Virtual, 0x0001_0000),
        (FieldId::NoScrub, 0x0002_0000),
        (FieldId::RecallOnOpen, 0x0004_0000),
        (FieldId::Pinned, 0x0008_0000),
        (FieldId::Unpinned, 0x0010_0000),
        (FieldId::RecallOnDataAccess, 0x0040_0000),
    ];
    for &(field, ntfs_bit) in expected {
        assert_eq!(
            field_to_attr_bit(field),
            ntfs_bit,
            "{field:?}: expected 0x{ntfs_bit:08X}, got 0x{:08X}",
            field_to_attr_bit(field)
        );
    }
}

/// Non-boolean fields must return 0 (no attribute bit).
#[test]
fn field_to_attr_bit_returns_zero_for_non_boolean_fields() {
    use crate::search::sorting::field_to_attr_bit;

    let non_boolean = [
        FieldId::Name,
        FieldId::Path,
        FieldId::PathOnly,
        FieldId::Size,
        FieldId::SizeOnDisk,
        FieldId::Created,
        FieldId::Modified,
        FieldId::Accessed,
        FieldId::Extension,
        FieldId::Type,
        FieldId::Descendants,
        FieldId::TreeSize,
    ];
    for field in non_boolean {
        assert_eq!(
            field_to_attr_bit(field),
            0,
            "{field:?} should return 0 (non-boolean)"
        );
    }
}

// ── sort_rows: boolean flags ─────────────────────────────────────────

/// Parametric helper: build two rows that differ only by a single flag
/// bit, verify `sort_rows` puts the flagged row first (desc) / last (asc).
///
/// Both rows have identical timestamps to catch any fallback to `modified`.
fn assert_sort_rows_boolean(field: SortColumn, flag_bit: u32) {
    // Use a base of 0 (no flags) so that `plain` never has the tested bit.
    // Add archive (0x20) only when NOT testing archive itself.
    let base = if flag_bit == 0x0020 { 0_u32 } else { 0x20 };
    let flagged = flagged_row("flagged.dat", flag_bit | base);
    let plain = flagged_row("plain.dat", base);

    // ── Descending: flagged first ──
    let mut rows_desc = vec![plain.clone(), flagged.clone()]; // wrong order
    sort_rows(&mut rows_desc, field, true, &[]);
    assert_eq!(
        rows_desc.first().expect("first").name(),
        "flagged.dat",
        "{field:?} desc: flagged row must sort first"
    );
    assert_eq!(
        rows_desc.last().expect("last").name(),
        "plain.dat",
        "{field:?} desc: plain row must sort last"
    );

    // ── Ascending: flagged last ──
    let mut rows_asc = vec![flagged, plain];
    sort_rows(&mut rows_asc, field, false, &[]);
    assert_eq!(
        rows_asc.first().expect("first").name(),
        "plain.dat",
        "{field:?} asc: plain row must sort first"
    );
    assert_eq!(
        rows_asc.last().expect("last").name(),
        "flagged.dat",
        "{field:?} asc: flagged row must sort last"
    );
}

#[test]
fn sort_rows_directory_flag() {
    // DirectoryFlag needs special handling: is_directory derives from flags.
    let dir = flagged_row("mydir", 0x0010);
    let file = flagged_row("myfile.dat", 0x0020);

    let mut rows_desc = vec![file.clone(), dir.clone()];
    sort_rows(&mut rows_desc, SortColumn::DirectoryFlag, true, &[]);
    assert_eq!(
        rows_desc.first().expect("first").name(),
        "mydir",
        "DirectoryFlag desc: directory must sort first"
    );
    assert_eq!(
        rows_desc.last().expect("last").name(),
        "myfile.dat",
        "DirectoryFlag desc: file must sort last"
    );

    let mut rows_asc = vec![dir, file];
    sort_rows(&mut rows_asc, SortColumn::DirectoryFlag, false, &[]);
    assert_eq!(
        rows_asc.first().expect("first").name(),
        "myfile.dat",
        "DirectoryFlag asc: file must sort first"
    );
    assert_eq!(
        rows_asc.last().expect("last").name(),
        "mydir",
        "DirectoryFlag asc: directory must sort last"
    );
}

#[test]
fn sort_rows_hidden_flag() {
    assert_sort_rows_boolean(SortColumn::Hidden, 0x0002);
}
#[test]
fn sort_rows_system_flag() {
    assert_sort_rows_boolean(SortColumn::System, 0x0004);
}
#[test]
fn sort_rows_readonly_flag() {
    assert_sort_rows_boolean(SortColumn::ReadOnly, 0x0001);
}
#[test]
fn sort_rows_archive_flag() {
    assert_sort_rows_boolean(SortColumn::Archive, 0x0020);
}
#[test]
fn sort_rows_compressed_flag() {
    assert_sort_rows_boolean(SortColumn::Compressed, 0x0800);
}
#[test]
fn sort_rows_encrypted_flag() {
    assert_sort_rows_boolean(SortColumn::Encrypted, 0x4000);
}
#[test]
fn sort_rows_sparse_flag() {
    assert_sort_rows_boolean(SortColumn::Sparse, 0x0200);
}
#[test]
fn sort_rows_reparse_flag() {
    assert_sort_rows_boolean(SortColumn::Reparse, 0x0400);
}
#[test]
fn sort_rows_offline_flag() {
    assert_sort_rows_boolean(SortColumn::Offline, 0x1000);
}
#[test]
fn sort_rows_not_indexed_flag() {
    assert_sort_rows_boolean(SortColumn::NotIndexed, 0x2000);
}
#[test]
fn sort_rows_integrity_flag() {
    assert_sort_rows_boolean(SortColumn::Integrity, 0x8000);
}
#[test]
fn sort_rows_no_scrub_flag() {
    assert_sort_rows_boolean(SortColumn::NoScrub, 0x0002_0000);
}
#[test]
fn sort_rows_pinned_flag() {
    assert_sort_rows_boolean(SortColumn::Pinned, 0x0008_0000);
}
#[test]
fn sort_rows_unpinned_flag() {
    assert_sort_rows_boolean(SortColumn::Unpinned, 0x0010_0000);
}

// ── sort_rows: multi-row boolean stability ───────────────────────────

/// With 5 rows (3 flagged, 2 unflagged), verify sort produces a clean
/// partition: all flagged records in one block, all unflagged in the other,
/// with name-based tiebreaking within each block.
#[test]
fn sort_rows_directory_flag_multi_row_stability() {
    let mut rows = vec![
        flagged_row("delta.txt", 0x0020),   // file
        flagged_row("alpha_dir", 0x0010),   // directory
        flagged_row("gamma.txt", 0x0020),   // file
        flagged_row("beta_dir", 0x0010),    // directory
        flagged_row("epsilon_dir", 0x0010), // directory
    ];

    sort_rows(&mut rows, SortColumn::DirectoryFlag, true, &[]);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    // Desc: all dirs first, then all files.
    // Within each group: name tiebreaker is also reversed (desc),
    // so names appear Z→A within each flag partition.
    assert_eq!(
        names,
        &[
            "epsilon_dir",
            "beta_dir",
            "alpha_dir",
            "gamma.txt",
            "delta.txt",
        ],
        "DirectoryFlag desc: dirs first, then files, name-desc within each"
    );
}

/// Ascending: files first, then directories, alphabetical within each block.
#[test]
fn sort_rows_directory_flag_multi_row_ascending() {
    let mut rows = vec![
        flagged_row("delta.txt", 0x0020), // file
        flagged_row("alpha_dir", 0x0010), // directory
        flagged_row("gamma.txt", 0x0020), // file
        flagged_row("beta_dir", 0x0010),  // directory
    ];

    sort_rows(&mut rows, SortColumn::DirectoryFlag, false, &[]);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    assert_eq!(
        names,
        &["delta.txt", "gamma.txt", "alpha_dir", "beta_dir",],
        "DirectoryFlag asc: files first, then dirs, alphabetical within each"
    );
}

/// Regression pin for the zero-alloc numeric fast path (v0.5.55+).
///
/// Strict-numeric sorts (`Modified`, `Size`, `Created`, etc.) with no
/// extra tiers take the `sort_rows_numeric_fast` path, which skips the
/// Schwartzian decorate entirely and uses a **raw-slice** name tiebreaker
/// for deterministic ordering on ties.  This is a documented behavior
/// change from the pre-fast-path Schwartzian sort (which used a folded /
/// case-insensitive tiebreaker) — uppercase letters now sort before
/// lowercase letters in the tiebreaker block.  Ties are rare in practice
/// (100 ns FILETIME resolution for Modified/Created/Accessed) and the
/// ordering is deterministic and matches the Everything baseline.
#[test]
fn sort_rows_numeric_fast_path_tiebreaker_is_raw_slice_cmp() {
    // All three rows share modified=42 → primary cmp is a tie, so the
    // tiebreaker fires.  Raw codepoint order puts 'B' (0x42) before 'a'
    // (0x61) before 'z' (0x7A): Beta.dll < alpha.dll < zeta.dll.
    let mut rows = vec![
        row("zeta.dll", uffs_mft::platform::DriveLetter::C, 100, 42, 0),
        row("Beta.dll", uffs_mft::platform::DriveLetter::C, 100, 42, 0),
        row("alpha.dll", uffs_mft::platform::DriveLetter::C, 100, 42, 0),
    ];

    sort_rows(&mut rows, SortColumn::Modified, true, &[]);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    assert_eq!(
        names,
        &["Beta.dll", "alpha.dll", "zeta.dll"],
        "Modified-DESC numeric fast path must use raw-slice name tiebreaker"
    );
}

/// Regression pin: the zero-alloc fast path must *only* activate when the
/// primary column and every tier are strictly numeric / flag columns.  If
/// a tier includes a string-based column (`Extension`, `Name`, etc.), the
/// sort must fall back to the Schwartzian path with case-folded keys.
#[test]
fn sort_rows_mixed_tier_falls_back_to_schwartzian() {
    let mut rows = vec![
        row("FILE.TXT", uffs_mft::platform::DriveLetter::C, 100, 42, 0), // size=100, ext=TXT
        row("file.log", uffs_mft::platform::DriveLetter::C, 100, 42, 0), // size=100, ext=log
        row("file.bin", uffs_mft::platform::DriveLetter::C, 100, 42, 0), // size=100, ext=bin
    ];

    // Primary: Size (numeric, all tied at 100).
    // Tier:    Extension (string) — must force Schwartzian.
    let tiers = vec![SortSpec {
        column: SortColumn::Extension,
        descending: false,
    }];
    sort_rows(&mut rows, SortColumn::Size, false, &tiers);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    // Folded extension order: "bin" < "log" < "txt".
    assert_eq!(
        names,
        &["file.bin", "file.log", "FILE.TXT"],
        "Size tie with Extension tier must use case-folded Schwartzian path"
    );
}

/// Regression pin: the lazy-key optimisation only populates `ext` when
/// `FieldId::Extension` is the sort column or a tier.  Exercising the
/// Extension path confirms the needs-analyzer still flags it correctly
/// and the fold+alloc still happens for this case.
#[test]
fn sort_rows_extension_column_still_folds_ext_key() {
    let mut rows = vec![
        row("file.TXT", uffs_mft::platform::DriveLetter::C, 1, 0, 0),
        row("file.log", uffs_mft::platform::DriveLetter::C, 1, 0, 0),
        row("file.Bin", uffs_mft::platform::DriveLetter::C, 1, 0, 0),
    ];

    // Sort by extension ascending: folded order is "bin" < "log" < "txt".
    sort_rows(&mut rows, SortColumn::Extension, false, &[]);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    assert_eq!(
        names,
        &["file.Bin", "file.log", "file.TXT"],
        "Extension sort must use case-folded key order (bin < log < txt)"
    );
}

/// Regression pin for the v0.5.58 Option C parallel-sort fast path.
///
/// `sort_rows_numeric_fast` switches to `par_sort_unstable_by` once the
/// input exceeds `PARALLEL_SORT_THRESHOLD` (= 16 K rows).  The parallel
/// comparator path must preserve the same ordering contract as the
/// sequential path: strict-numeric primary + raw-slice name tiebreaker.
/// This test feeds 20 K synthetic rows with two distinct modified
/// timestamps to force the parallel branch, then asserts the output is
/// monotonically non-increasing in `modified` (DESC) and strictly
/// ordered by raw name within each tie block.
#[test]
fn sort_rows_numeric_fast_parallel_branch_preserves_order() {
    // 20 K rows: half at modified=1000, half at modified=2000.  Names
    // are zero-padded so raw-slice order is deterministic regardless
    // of insertion order.
    let mut rows: Vec<DisplayRow> = (0..20_000_u32)
        .map(|idx| {
            let modified = if idx % 2 == 0 { 2000 } else { 1000 };
            row(
                &format!("file_{idx:05}.dll"),
                uffs_mft::platform::DriveLetter::C,
                u64::from(idx),
                modified,
                0,
            )
        })
        .collect();

    sort_rows(&mut rows, SortColumn::Modified, true, &[]);

    // First 10 K rows must all be modified=2000, last 10 K must be
    // modified=1000 (primary DESC).
    for row in rows.iter().take(10_000) {
        assert_eq!(
            row.modified, 2000,
            "top 10 K must be modified=2000 under DESC sort"
        );
    }
    for row in rows.iter().skip(10_000) {
        assert_eq!(
            row.modified, 1000,
            "bottom 10 K must be modified=1000 under DESC sort"
        );
    }

    // Within each tie block names must be strictly ascending by raw
    // codepoint order (the fast-path tiebreaker).
    let (top_half, bottom_half) = rows.split_at(10_000);
    for pair in top_half.windows(2) {
        if let [first, second] = pair {
            assert!(
                first.name() < second.name(),
                "tiebreaker must be raw-slice ASC inside tie: {:?} vs {:?}",
                first.name(),
                second.name()
            );
        }
    }
    for pair in bottom_half.windows(2) {
        if let [first, second] = pair {
            assert!(
                first.name() < second.name(),
                "tiebreaker must be raw-slice ASC inside tie: {:?} vs {:?}",
                first.name(),
                second.name()
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 4 Commit F — bloom pre-check integration in search_index
// ═══════════════════════════════════════════════════════════════════════

/// Build a multi-drive `DriveIndex` for the bloom-skip integration
/// tests with **deterministically separated** blooms on each drive.
///
/// The default `build_compact_index` sizes blooms at the production
/// 1 % FPR (`SHARD_BLOOM_TARGET_FPR`), which on tiny test fixtures
/// produces flaky outcomes for novel-term probes (a single 1 %
/// false positive turns a "skip" into a "keep" and breaks the
/// `records_scanned` assertion).
///
/// To pin the test contract without weakening the production
/// constant, this fixture builds the drives normally, then
/// **overwrites** each drive's bloom with a tighter (0.1 % FPR)
/// re-build over the same source records + `ext_names`.  The bloom
/// *contents* are unchanged (still folded basenames + extensions);
/// only the FPR margin is tightened so novel-term probes reliably
/// miss in tests.
fn build_bloom_skip_fixture() -> DriveIndex {
    use alloc::format;
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::bloom::Bloom;
    use crate::compact::build_compact_index;

    const FILES_PER_DRIVE: u32 = 50;
    /// Tighter than `SHARD_BLOOM_TARGET_FPR` (1 %) so the
    /// 4 novel-term probes in this suite reliably miss.
    const TEST_FPR: f64 = 0.001;

    let mut drives = Vec::new();
    for (letter, ext) in [
        (uffs_mft::platform::DriveLetter::C, "txt"),
        (uffs_mft::platform::DriveLetter::D, "csv"),
    ] {
        let mut idx = MftIndex::new(letter);
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS.into());
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = Into::into(ROOT_FRS);

        for i in 0_u32..FILES_PER_DRIVE {
            let file_name = format!("file_{i:03}.{ext}");
            let n_off = idx.add_name(&file_name);
            let n_ext = idx.intern_extension(&file_name);
            let frs = u64::from(200_u32.saturating_add(i));
            let rec = idx.get_or_create(frs.into());
            rec.first_name.name = IndexNameRef::new(
                n_off,
                u16::try_from(file_name.len()).expect("name too long"),
                true,
                n_ext,
            );
            rec.first_name.parent_frs = Into::into(ROOT_FRS);
            rec.first_stream.size = SizeInfo {
                length: 100,
                allocated: 512,
            };
            rec.stdinfo.flags = 0x20;
        }

        let (mut drive, _, _) = build_compact_index(letter, &idx);

        // Overwrite the auto-built (1 %-FPR) bloom with a tighter
        // (0.1 %-FPR) one over the same inputs so the test's novel-
        // term probes don't suffer flaky FPR collisions.  The
        // insertion contract mirrors `compact_filters::build_bloom`
        // exactly — folded basenames + as-is extensions.
        let n_items = drive
            .records
            .len()
            .saturating_add(drive.ext_names.len())
            .max(1);
        let mut bloom = Bloom::with_capacity_and_fpr(n_items, TEST_FPR);
        let mut fold_buf: Vec<u8> = Vec::with_capacity(64);
        for record in &drive.records {
            let start = record.name_offset as usize;
            let end = start.saturating_add(record.name_len as usize);
            if let Some(name_bytes) = drive.names.get(start..end)
                && let Ok(name_str) = core::str::from_utf8(name_bytes)
            {
                let folded = drive.fold.fold_into(name_str, &mut fold_buf);
                bloom.insert(folded.as_bytes());
            }
        }
        for ext_name in &drive.ext_names {
            let bytes = ext_name.as_bytes();
            if !bytes.is_empty() {
                bloom.insert(bytes);
            }
        }
        drive.bloom = Some(bloom);

        drives.push(Arc::new(drive));
    }
    DriveIndex { drives }
}

/// Sanity guard for the fixture: each drive's bloom must hit its
/// own extension and miss the other drive's.  Without this contract
/// the behavioural tests below are meaningless — pin it explicitly
/// so a future fixture tweak surfaces an FPR regression cleanly.
#[test]
fn bloom_skip_fixture_has_well_separated_blooms() {
    let index = build_bloom_skip_fixture();
    let c_bloom = index
        .drives
        .first()
        .expect("C")
        .bloom
        .as_ref()
        .expect("C bloom populated");
    let d_bloom = index
        .drives
        .get(1)
        .expect("D")
        .bloom
        .as_ref()
        .expect("D bloom populated");

    assert!(c_bloom.contains(b"txt"), "C must contain its own ext");
    assert!(d_bloom.contains(b"csv"), "D must contain its own ext");
    assert!(
        !c_bloom.contains(b"csv"),
        "C must not have a false positive on `csv` (FPR regression)"
    );
    assert!(
        !d_bloom.contains(b"txt"),
        "D must not have a false positive on `txt` (FPR regression)"
    );
}

/// `search_index` with `extensions = ["csv"]` must bloom-skip drive
/// C (no `.csv` files).  Verified via `records_scanned`, which after
/// `apply_bloom_pre_check` reflects only the surviving drives.
#[test]
fn search_index_bloom_skips_c_when_filter_is_csv_only() {
    let index = build_bloom_skip_fixture();
    let drive_d_records = index.drives.get(1).expect("D").records.len();

    let mut filters_csv = super::super::filters::SearchFilters {
        extensions: vec!["csv".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters_csv),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        result.records_scanned, drive_d_records,
        "csv-only filter must bloom-skip C; only D scanned"
    );
}

/// Mirror on the opposite drive: `extensions = ["txt"]` must
/// bloom-skip D and scan only C.
#[test]
fn search_index_bloom_skips_d_when_filter_is_txt_only() {
    let index = build_bloom_skip_fixture();
    let drive_c_records = index.drives.first().expect("C").records.len();

    let mut filters_txt = super::super::filters::SearchFilters {
        extensions: vec!["txt".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters_txt),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        result.records_scanned, drive_c_records,
        "txt-only filter must bloom-skip D; only C scanned"
    );
}

/// Multi-term filter where every drive matches at least one term —
/// the bloom pre-check must keep both drives.
#[test]
fn search_index_bloom_keeps_all_drives_when_any_ext_hits() {
    let index = build_bloom_skip_fixture();
    let total_records: usize = index.drives.iter().map(|dr| dr.records.len()).sum();

    let mut filters_both = super::super::filters::SearchFilters {
        extensions: vec!["txt".to_owned(), "csv".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters_both),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        result.records_scanned, total_records,
        "any-hit filter must keep every drive in the active subset"
    );
}

/// Empty `extensions` short-circuits the bloom pre-check entirely —
/// the no-ext-filter path must scan every drive (substring queries
/// can't bloom-skip; see `crate::search::bloom_skip` for the
/// correctness contract).
#[test]
fn search_index_no_bloom_skip_when_extensions_empty() {
    let index = build_bloom_skip_fixture();
    let total_records: usize = index.drives.iter().map(|dr| dr.records.len()).sum();

    let mut filters_empty = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters_empty),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        result.records_scanned, total_records,
        "empty-ext filter must keep every drive (no bloom-skip)"
    );
}

/// Filter with a never-indexed extension: every drive's bloom
/// misses, every drive is skipped.  Records-scanned drops to zero,
/// pinning the "all-miss → empty active subset" edge case.
#[test]
fn search_index_bloom_skips_all_drives_when_no_ext_matches_any() {
    let index = build_bloom_skip_fixture();
    let mut filters_novel = super::super::filters::SearchFilters {
        extensions: vec!["thisextisneverindexedanywhereonpurpose".to_owned()],
        ..super::super::filters::SearchFilters::default()
    };
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters_novel),
        FieldId::Modified,
        true,
        &[],
    )
    .expect("search runs");
    assert_eq!(
        result.records_scanned, 0,
        "novel-ext filter must bloom-skip every drive — zero records scanned"
    );
    assert!(
        result.rows.is_empty(),
        "no drives scanned → no matching rows"
    );
}

// ── Cooperative cancellation ────────────────────────────────────────

/// A search whose `cancel` flag is raised must come back with **no**
/// rows on every dispatch path — never a partial subset dressed up as
/// the answer.  The daemon raises the flag when its scan budget
/// expires and discards whatever comes back; what this pins is that
/// the scan honours the flag and that nothing half-collected leaks.
/// The uncancelled twin of each query is asserted non-empty first so
/// an empty result cannot pass by accident.
#[test]
fn cancelled_search_returns_no_rows_on_every_dispatch_path() {
    let index = build_two_drive_index();
    // (pattern, sort) — match-all numeric, match-all tree (Path sort),
    // match-all path_only, regex, trigram/substring, prefix.
    let cases: [(&str, FieldId); 6] = [
        ("*", FieldId::Modified),
        ("*", FieldId::Path),
        ("*", FieldId::PathOnly),
        (">.*", FieldId::Modified),
        ("report", FieldId::Modified),
        ("repo*", FieldId::Modified),
    ];
    for (pattern, sort) in cases {
        let mut live = super::super::filters::SearchFilters::default();
        let live_rows = search_index(
            &index,
            SearchRequest::new(pattern, &mut live),
            sort,
            true,
            &[],
        )
        .expect("search runs")
        .rows;
        assert!(
            !live_rows.is_empty(),
            "fixture must match `{pattern}` (sort {sort:?}) when not cancelled"
        );

        let cancel = Arc::new(core::sync::atomic::AtomicBool::new(true));
        let mut cancelled = super::super::filters::SearchFilters {
            cancel: Some(cancel),
            ..Default::default()
        };
        let result = search_index(
            &index,
            SearchRequest::new(pattern, &mut cancelled),
            sort,
            true,
            &[],
        )
        .expect("search runs");
        assert!(
            result.rows.is_empty(),
            "`{pattern}` (sort {sort:?}) must return no rows once cancelled; got {}",
            result.rows.len()
        );
    }
}

/// The cancel handle is search-time state, not a filter: a filter set
/// that carries only a cancel flag is still "empty" for every
/// fast-path decision, and the flag survives the per-drive clone the
/// rayon workers take.
#[test]
fn cancel_handle_is_not_a_filter_and_survives_clone() {
    let flag = Arc::new(core::sync::atomic::AtomicBool::new(false));
    let filters = super::super::filters::SearchFilters {
        cancel: Some(Arc::clone(&flag)),
        ..Default::default()
    };
    assert!(
        filters.is_empty(),
        "a cancel handle alone must not count as a filter"
    );
    assert!(!filters.is_cancelled());
    let worker_copy = filters.clone();
    flag.store(true, core::sync::atomic::Ordering::Relaxed);
    assert!(filters.is_cancelled(), "the original must observe the flag");
    assert!(
        worker_copy.is_cancelled(),
        "the clone must observe the shared flag"
    );
    assert!(
        worker_copy.cancelled_at(0) && !worker_copy.cancelled_at(1),
        "cancelled_at polls only on the stride boundary"
    );
}
