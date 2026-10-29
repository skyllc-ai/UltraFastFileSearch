// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Row → CSV writer.
//!
//! This is the one place the canonical CSV bytes are emitted.  Both
//! `uffs-core::output::OutputConfig::write_display_rows` (daemon
//! `--out=file` path) and `uffs-client::output::write_search_rows`
//! (CLI stdout path after receiving `SearchPayload::InlineRows`)
//! delegate to [`write_rows`].

use std::io::{self, Write};

use rayon::prelude::*;

use crate::attr;
use crate::column::{BASELINE_COLUMN_ORDER, OutputColumn};
use crate::config::OutputConfig;
use crate::datetime::append_datetime_native;
use crate::derived::{bulkiness_for_row, extension_from_name, semantic_type_for_row};
use crate::row::FormatRow;

/// Row-count cutover between the sequential writer and the rayon
/// parallel writer.
///
/// Measured on a 168 K-row path export: below ~16 K the per-chunk
/// allocation + channel overhead of rayon outweighs the formatting
/// cost.  Above it, the 8-core parallel writer roughly halves the
/// wall-clock (~24 ms → ~10 ms).
pub(crate) const PARALLEL_WRITE_THRESHOLD: usize = 16_384;

/// Convert a `Vec::len()` (`usize`) to a `u16` count, saturating on overflow.
///
/// Used by the `NameLength` and `PathLength` output columns where the
/// CSV writer needs a fixed-width integer rendering.  In practice the
/// inputs are NTFS filename / path lengths (max 255 chars for a single
/// name; up to 32 KB for a full path under Unicode long-path support),
/// well below the `u16::MAX` saturation point.
///
/// This is a 3-line duplicate of `uffs_mft::len_to_u16` (see
/// `crates/uffs-mft/src/index/types.rs:51-61`).  The duplication is
/// deliberate (Option B from issue #216): it eliminates the
/// `uffs-format → uffs-mft` Cargo dep edge that previously existed
/// solely for this one utility, removing the architectural smell of
/// "single function across multiple homes".  If a future refactor
/// extracts utilities into a Layer-0 shared crate (e.g. `uffs-text`
/// or a new `uffs-numerics`), both call-sites should migrate to the
/// shared definition.
#[inline]
fn len_to_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// Rows per parallel chunk.  Sized for ~4096 rows × ~128 bytes/row =
/// ~512 KB — large enough to amortise the per-chunk fixed cost,
/// small enough that 8 workers get at least a few chunks each.
pub(crate) const PARALLEL_WRITE_CHUNK: usize = 4096;

/// Write `rows` through `writer` using the columns + formatting
/// specified by `cfg`.
///
/// # Errors
///
/// Propagates any `io::Error` the underlying writer returns.  The
/// parallel branch writes chunk-by-chunk in input order, so a
/// mid-stream error produces a prefix of the expected output rather
/// than interleaved partial rows.
pub fn write_rows<R, W>(cfg: &OutputConfig, rows: &[R], mut writer: W) -> io::Result<()>
where
    R: FormatRow + Sync,
    W: Write,
{
    let output_cols: &[OutputColumn] = cfg
        .columns
        .as_ref()
        .map_or(BASELINE_COLUMN_ORDER, |cols| cols.as_slice());

    if cfg.header {
        let mut header = String::with_capacity(output_cols.len() * 24);
        for (idx, col) in output_cols.iter().enumerate() {
            if idx > 0 {
                header.push_str(&cfg.separator);
            }
            header.push_str(&cfg.quote);
            header.push_str(col.display_name());
            header.push_str(&cfg.quote);
        }
        // Two newlines: one to terminate the header row, one blank
        // line before the data.  Matches the legacy baseline the
        // parity tests pin.
        header.push('\n');
        header.push('\n');
        writer.write_all(header.as_bytes())?;
    }

    if rows.len() >= PARALLEL_WRITE_THRESHOLD {
        // Parallel path — per-chunk rendering, serial output.
        let chunk_bufs: Vec<Vec<u8>> = rows
            .par_chunks(PARALLEL_WRITE_CHUNK)
            .map(|chunk| {
                let mut chunk_buf: Vec<u8> = Vec::with_capacity(chunk.len() * 128);
                let mut scratch = String::with_capacity(output_cols.len() * 32);
                let mut itoa_buf = itoa::Buffer::new();
                for row in chunk {
                    scratch.clear();
                    write_row(&mut scratch, &mut itoa_buf, output_cols, cfg, row);
                    scratch.push('\n');
                    chunk_buf.extend_from_slice(scratch.as_bytes());
                }
                chunk_buf
            })
            .collect();
        for chunk_buf in &chunk_bufs {
            writer.write_all(chunk_buf)?;
        }
    } else {
        // Sequential path — tiny scratch + single writer pass.
        let mut buf = String::with_capacity(output_cols.len() * 32);
        let mut itoa_buf = itoa::Buffer::new();
        for row in rows {
            buf.clear();
            write_row(&mut buf, &mut itoa_buf, output_cols, cfg, row);
            buf.push('\n');
            writer.write_all(buf.as_bytes())?;
        }
    }

    Ok(())
}

/// Render one row into `buf`, appending the configured columns
/// separated by `cfg.separator`.  Does not emit a trailing newline
/// — the caller adds `\n` after invoking this helper.
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive match over ~38 OutputColumn variants; the table is its own readability"
)]
pub(crate) fn write_row<R: FormatRow>(
    buf: &mut String,
    itoa_buf: &mut itoa::Buffer,
    output_cols: &[OutputColumn],
    cfg: &OutputConfig,
    row: &R,
) {
    let flags = row.flags();
    let parity_dir = cfg.parity_compat && row.is_directory();
    let path = row.path();

    for (idx, col) in output_cols.iter().enumerate() {
        if idx > 0 {
            buf.push_str(&cfg.separator);
        }
        match col {
            OutputColumn::Path => {
                buf.push_str(&cfg.quote);
                buf.push_str(path);
                if parity_dir && !path.ends_with('\\') {
                    buf.push('\\');
                }
                buf.push_str(&cfg.quote);
            }
            OutputColumn::Name => {
                buf.push_str(&cfg.quote);
                if !parity_dir {
                    buf.push_str(row.name());
                }
                buf.push_str(&cfg.quote);
            }
            OutputColumn::PathOnly => {
                buf.push_str(&cfg.quote);
                if parity_dir {
                    buf.push_str(path);
                    if !path.ends_with('\\') {
                        buf.push('\\');
                    }
                } else if let Some(pos) = path.rfind('\\') {
                    buf.push_str(path.get(..=pos).unwrap_or(path));
                } else {
                    buf.push_str(path);
                }
                buf.push_str(&cfg.quote);
            }
            OutputColumn::Size => {
                let val = if parity_dir {
                    row.treesize()
                } else {
                    row.size()
                };
                buf.push_str(itoa_buf.format(val));
            }
            OutputColumn::SizeOnDisk => {
                let val = if parity_dir {
                    row.tree_allocated()
                } else {
                    row.allocated()
                };
                buf.push_str(itoa_buf.format(val));
            }
            OutputColumn::Created => {
                append_datetime_native(buf, row.created(), cfg.timezone_offset_secs);
            }
            OutputColumn::Modified => {
                append_datetime_native(buf, row.modified(), cfg.timezone_offset_secs);
            }
            OutputColumn::Accessed => {
                append_datetime_native(buf, row.accessed(), cfg.timezone_offset_secs);
            }
            OutputColumn::Descendants => {
                buf.push_str(itoa_buf.format(row.descendants()));
            }
            OutputColumn::TreeSize => {
                buf.push_str(itoa_buf.format(row.treesize()));
            }
            OutputColumn::TreeAllocated => {
                buf.push_str(itoa_buf.format(row.tree_allocated()));
            }
            OutputColumn::Type => {
                buf.push_str(&cfg.quote);
                buf.push_str(semantic_type_for_row(row));
                buf.push_str(&cfg.quote);
            }
            OutputColumn::Attributes | OutputColumn::AttributeValue => {
                buf.push_str(itoa_buf.format(flags));
            }
            OutputColumn::ParityAttributes => {
                buf.push_str(itoa_buf.format(flags & attr::PARITY_MASK));
            }
            OutputColumn::Hidden => push_flag(buf, cfg, flags, attr::HIDDEN),
            OutputColumn::System => push_flag(buf, cfg, flags, attr::SYSTEM),
            OutputColumn::Archive => push_flag(buf, cfg, flags, attr::ARCHIVE),
            OutputColumn::ReadOnly => push_flag(buf, cfg, flags, attr::READONLY),
            OutputColumn::Compressed => push_flag(buf, cfg, flags, attr::COMPRESSED),
            OutputColumn::Encrypted => push_flag(buf, cfg, flags, attr::ENCRYPTED),
            OutputColumn::Sparse => push_flag(buf, cfg, flags, attr::SPARSE),
            OutputColumn::Reparse => push_flag(buf, cfg, flags, attr::REPARSE),
            OutputColumn::Offline => push_flag(buf, cfg, flags, attr::OFFLINE),
            OutputColumn::NotIndexed => push_flag(buf, cfg, flags, attr::NOT_INDEXED),
            OutputColumn::Temporary => push_flag(buf, cfg, flags, attr::TEMPORARY),
            OutputColumn::Virtual => push_flag(buf, cfg, flags, attr::VIRTUAL),
            OutputColumn::Pinned => push_flag(buf, cfg, flags, attr::PINNED),
            OutputColumn::Unpinned => push_flag(buf, cfg, flags, attr::UNPINNED),
            OutputColumn::DirectoryFlag => push_flag(buf, cfg, flags, attr::DIRECTORY),
            OutputColumn::Integrity => push_flag(buf, cfg, flags, attr::INTEGRITY),
            OutputColumn::NoScrub => push_flag(buf, cfg, flags, attr::NO_SCRUB),
            OutputColumn::RecallOnOpen => push_flag(buf, cfg, flags, attr::RECALL_ON_OPEN),
            OutputColumn::RecallOnDataAccess => push_flag(buf, cfg, flags, attr::RECALL_ON_DATA),
            OutputColumn::Bulkiness => {
                // Per-million → percentage (÷ 10_000).
                let per_million = bulkiness_for_row(row);
                let pct = per_million / 10_000;
                buf.push_str(itoa_buf.format(pct));
            }
            OutputColumn::Drive => {
                buf.push(row.drive());
            }
            OutputColumn::Extension => {
                buf.push_str(&cfg.quote);
                // Dot-gated: dotfiles (`.bash_history`), dotless names
                // (`README`), and trailing-dot names (`foo.`) emit an
                // empty extension so the displayed value matches the
                // sort engine's key
                // (`uffs_core::search::sorting::build_row_sort_key`)
                // and the indexer's `intern_extension` semantics.
                if let Some(ext) = extension_from_name(row.name()) {
                    buf.push_str(ext);
                }
                buf.push_str(&cfg.quote);
            }
            OutputColumn::NameLength => {
                let len = len_to_u16(row.name().chars().count());
                buf.push_str(itoa_buf.format(len));
            }
            OutputColumn::PathLength => {
                let len = len_to_u16(path.chars().count());
                buf.push_str(itoa_buf.format(len));
            }
            // ── WI-4.4 forensic columns ────────────────────────────────
            // Booleans render as pos/neg in their own column (like the
            // attribute flags), never inline with the name.
            OutputColumn::Malformed => push_bool(buf, cfg, row.malformed()),
            OutputColumn::MalformedPath => push_bool(buf, cfg, row.malformed_path()),
            OutputColumn::NameHex => {
                buf.push_str(&cfg.quote);
                if let Some(hex) = row.name_hex() {
                    buf.push_str(hex);
                }
                buf.push_str(&cfg.quote);
            }
        }
    }
}

/// Append `cfg.pos` or `cfg.neg` depending on whether `mask` is set
/// in `flags`.
fn push_flag(buf: &mut String, cfg: &OutputConfig, flags: u32, mask: u32) {
    if flags & mask != 0 {
        buf.push_str(&cfg.pos);
    } else {
        buf.push_str(&cfg.neg);
    }
}

/// Append `cfg.pos` or `cfg.neg` for a precomputed boolean (mirrors
/// [`push_flag`] but for a `bool` not backed by an attribute-flag mask).
fn push_bool(buf: &mut String, cfg: &OutputConfig, value: bool) {
    if value {
        buf.push_str(&cfg.pos);
    } else {
        buf.push_str(&cfg.neg);
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputConfig, write_rows};
    use crate::row::FormatRow;

    /// Minimal `FormatRow` implementation for unit tests.
    #[derive(Clone)]
    struct TestRow {
        drive: char,
        path: String,
        name: String,
        size: u64,
        is_directory: bool,
        modified: i64,
        created: i64,
        accessed: i64,
        flags: u32,
        allocated: u64,
        descendants: u32,
        treesize: u64,
        tree_allocated: u64,
    }

    impl FormatRow for TestRow {
        fn drive(&self) -> char {
            self.drive
        }
        fn path(&self) -> &str {
            &self.path
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn size(&self) -> u64 {
            self.size
        }
        fn is_directory(&self) -> bool {
            self.is_directory
        }
        fn modified(&self) -> i64 {
            self.modified
        }
        fn created(&self) -> i64 {
            self.created
        }
        fn accessed(&self) -> i64 {
            self.accessed
        }
        fn flags(&self) -> u32 {
            self.flags
        }
        fn allocated(&self) -> u64 {
            self.allocated
        }
        fn descendants(&self) -> u32 {
            self.descendants
        }
        fn treesize(&self) -> u64 {
            self.treesize
        }
        fn tree_allocated(&self) -> u64 {
            self.tree_allocated
        }
    }

    fn sample() -> TestRow {
        TestRow {
            drive: 'C',
            path: "C:\\Temp\\file.txt".to_owned(),
            name: "file.txt".to_owned(),
            size: 123,
            is_directory: false,
            modified: 0,
            created: 0,
            accessed: 0,
            flags: 0,
            allocated: 128,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
        }
    }

    /// Canonical header format: display-name wrapped in the quote
    /// character, separated by the separator, terminated by `\n\n`
    /// (header row + blank separator row before data).
    #[test]
    fn header_has_double_newline_blank_separator() {
        let cfg = OutputConfig::new()
            .with_columns("path,name")
            .with_separator(",")
            .with_quote("\"");
        let rows = vec![sample()];
        let mut out = Vec::new();
        write_rows(&cfg, &rows, &mut out).expect("write");
        let text = String::from_utf8(out).expect("utf8");
        // `\"Path\",\"Name\"\n` then blank line, then the data row.
        assert!(
            text.starts_with("\"Path\",\"Name\"\n\n"),
            "header must end with a blank separator line; got: {text:?}"
        );
    }

    /// Numeric columns are unquoted — the formatter emits raw numbers
    /// regardless of `cfg.quote`.
    #[test]
    fn numeric_columns_are_unquoted() {
        let cfg = OutputConfig::new()
            .with_columns("name,size")
            .with_header(false)
            .with_quote("'");
        let rows = vec![sample()];
        let mut out = Vec::new();
        write_rows(&cfg, &rows, &mut out).expect("write");
        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(text, "'file.txt',123\n");
    }

    /// `--pos` / `--neg` drive flag-column rendering.
    #[test]
    fn flag_columns_honour_pos_neg() {
        let cfg = OutputConfig::new()
            .with_columns("hidden,system")
            .with_header(false)
            .with_quote("")
            .with_pos("+")
            .with_neg("-");
        let rows = vec![TestRow {
            flags: 0x0002, // HIDDEN set, SYSTEM clear
            ..sample()
        }];
        let mut out = Vec::new();
        write_rows(&cfg, &rows, &mut out).expect("write");
        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(text, "+,-\n");
    }

    /// Regression: the `Extension` column must use dot-gated extraction so
    /// the displayed value matches the sort engine's key
    /// (`uffs_core::search::sorting::build_row_sort_key` →
    /// `extract_extension_after_dot`) and the indexer's `intern_extension`
    /// semantics.  Pre-fix, naive `rfind('.')` produced
    /// `ext = "bash_history"` for `.bash_history`, while the sort key was
    /// `""`.  Caught by Windows MCP T62
    /// (`scripts/tests/definitions/03-sort.toml`).
    #[test]
    fn extension_column_dot_gated_for_dotfiles_dotless_and_trailing_dot() {
        let cfg = OutputConfig::new()
            .with_columns("name,ext")
            .with_header(false)
            .with_quote("\"");

        let cases = [
            // (path,                                     name,
            // expected_ext)
            ("F:\\Ch\\.bash_history", ".bash_history", ""),
            ("C:\\Users\\rnio\\.gitignore", ".gitignore", ""),
            ("C:\\tmp\\README", "README", ""),
            ("C:\\tmp\\foo.", "foo.", ""),
            (
                "C:\\Win\\amd64_x_10.0.26100.8115_none_xyz",
                "amd64_x_10.0.26100.8115_none_xyz",
                "8115_none_xyz",
            ),
            ("C:\\Projects\\report.txt", "report.txt", "txt"),
        ];

        for (path, name, expected_ext) in cases {
            let row = TestRow {
                path: path.to_owned(),
                name: name.to_owned(),
                ..sample()
            };
            let mut out = Vec::new();
            write_rows(&cfg, &[row], &mut out).expect("write");
            let text = String::from_utf8(out).expect("utf8");
            let expected = format!("\"{name}\",\"{expected_ext}\"\n");
            assert_eq!(
                text, expected,
                "Extension column mismatch for name {name:?}: got {text:?}"
            );
        }
    }
}
