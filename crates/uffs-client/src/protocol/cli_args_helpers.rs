// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Stateless parsing helpers shared by [`super::cli_args`].
//!
//! Lifted out of `cli_args.rs` purely to keep that file under the
//! 800-line policy ceiling.  None of these helpers depend on
//! [`super::SearchParams`] state — they are pure conversions between
//! CLI token strings and typed values, so living in a sibling module
//! costs nothing at call sites and buys generous head-room in the
//! main file.

use core::num::ParseIntError;

use uffs_mft::platform::{DriveLetter, DriveLetterError};

use crate::format::ParseSizeError;
// `parse_size` is re-exported so the importer can glob-import every
// parsing helper the main file needs in a single `use` statement.
pub(super) use crate::format::parse_size;

/// Typed error produced by every CLI-argument parsing helper in this
/// module and by [`crate::protocol::SearchParams::from_cli_args`].
///
/// Phase 5d migration of the previous `Result<_, String>` return
/// types: every [`core::fmt::Display`] string stays byte-identical
/// with the pre-migration `format!()` payloads so any operator-facing
/// CLI error output is unchanged, while callers can now match on
/// variants and walk the [`core::error::Error::source`] chain.
///
/// `#[non_exhaustive]` per Phase 5c discipline so future CLI flags
/// can grow a variant without a semver bump on the
/// (workspace-internal) consumers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CliArgsError {
    /// `take_next` / `flag_val` ran out of tokens before consuming a
    /// value for `flag`.
    #[error("Missing value for {flag}")]
    MissingValue {
        /// The CLI flag name that was awaiting a value.
        flag: String,
    },
    /// `parse_u16` / `parse_u32` / `parse_u64` / `parse_i32` failed
    /// to convert `flag`'s text to the target integer type.  The
    /// underlying [`ParseIntError`] is exposed via
    /// [`core::error::Error::source`] for callers that want the typed
    /// chain.
    #[error("Bad {flag}: {source}")]
    BadInt {
        /// The CLI flag name whose value failed to parse.
        flag: String,
        /// The underlying `u*::from_str` / `i*::from_str` failure.
        #[source]
        source: ParseIntError,
    },
    /// `parse_bool` rejected `value` for `flag` — accepted forms are
    /// `true|1|yes` and `false|0|no`.
    #[error("Bad bool for {flag}: '{value}'")]
    BadBool {
        /// The CLI flag name whose value failed to parse.
        flag: String,
        /// The offending value as supplied by the operator.
        value: String,
    },
    /// `drives_csv` encountered an empty segment (e.g. `--drives ,C`
    /// or `--drives C,`).
    #[error("empty drive")]
    EmptyDrive,
    /// `drives_csv` saw a segment whose shape is not a single ASCII
    /// alphabetic letter (optionally with a trailing `:`).  Examples:
    /// `12`, `CD`, `??`.
    #[error("Bad drive: '{input}'")]
    BadDrive {
        /// The offending segment (original, untrimmed).
        input: String,
    },
    /// `drives_csv` accepted the shape but [`DriveLetter::parse`]
    /// rejected the letter — this branch is currently unreachable
    /// (the `is_ascii_alphabetic` guard makes it impossible), but we
    /// keep the variant so the error path stays expressible if the
    /// invariant ever changes.  The underlying [`DriveLetterError`]
    /// is exposed via [`core::error::Error::source`].
    #[error("Bad drive: '{input}' ({source})")]
    BadDriveParse {
        /// The offending segment (original, untrimmed).
        input: String,
        /// The underlying [`DriveLetter::parse`] failure.
        #[source]
        source: DriveLetterError,
    },
    /// [`crate::format::parse_size`] rejected one of the
    /// `--{min,max,exact}-size*` / `--{min,max}-treesize` /
    /// `--{min,max}-tree-allocated` operands.  The underlying
    /// [`ParseSizeError`] is exposed via
    /// [`core::error::Error::source`] AND `#[from]` so call sites can
    /// keep the `?` propagation shape.
    #[error("{source}")]
    BadSize {
        /// The underlying [`crate::format::parse_size`] failure.
        #[source]
        #[from]
        source: ParseSizeError,
    },
    /// `from_cli_args` encountered a leading-dash token that did not
    /// match any known flag.
    #[error("Unknown flag: '{flag}'")]
    UnknownFlag {
        /// The unrecognised flag (as supplied on the command line).
        flag: String,
    },
    /// `from_cli_args` encountered a second positional argument after
    /// the pattern slot was already filled.
    #[error("Unexpected argument: '{arg}'")]
    UnexpectedArgument {
        /// The extra positional argument.
        arg: String,
    },
    /// `into_search_params` saw `--name-only` combined with a pattern
    /// that contains `\` or `/` (without the `>` regex prefix), which
    /// is semantically inconsistent — name-only matches against the
    /// file name, not the path.
    #[error("--name-only cannot be used with path patterns containing '\\' or '/'")]
    NameOnlyWithPathPattern,
}

/// Returns `Some(val)` if `val` is non-empty, otherwise `None`.
/// Used for optional output-config fields that should fall back to
/// `OutputConfig` defaults when the user did not supply them.
pub(super) fn non_empty(val: String) -> Option<String> {
    if val.is_empty() { None } else { Some(val) }
}

/// Consume next token or report missing value.
pub(super) fn take_next(
    flag: &str,
    iter: &mut impl Iterator<Item = String>,
) -> Result<String, CliArgsError> {
    iter.next().ok_or_else(|| CliArgsError::MissingValue {
        flag: flag.to_owned(),
    })
}

/// Handle `--flag=val` or `--flag <val>`.
pub(super) fn flag_val(
    cur: &str,
    flag: &str,
    iter: &mut impl Iterator<Item = String>,
) -> Result<String, CliArgsError> {
    cur.strip_prefix(&format!("{flag}="))
        .map_or_else(|| take_next(flag, iter), |rest| Ok(rest.to_owned()))
}

/// Parse comma-separated drive letters.
pub(super) fn drives_csv(input: &str) -> Result<Vec<DriveLetter>, CliArgsError> {
    input
        .split(',')
        .map(|part| {
            let stripped = part.trim();
            let trimmed = stripped.strip_suffix(':').unwrap_or(stripped);
            let ch = trimmed.chars().next().ok_or(CliArgsError::EmptyDrive)?;
            if trimmed.len() != 1 || !ch.is_ascii_alphabetic() {
                return Err(CliArgsError::BadDrive {
                    input: part.to_owned(),
                });
            }
            // `is_ascii_alphabetic` proves the parse succeeds; the
            // `BadDriveParse` variant chains the typed
            // `DriveLetterError` for the (unreachable today) failure
            // case so the error path is still expressible if the
            // invariant ever changes.
            DriveLetter::parse(ch).map_err(|source| CliArgsError::BadDriveParse {
                input: part.to_owned(),
                source,
            })
        })
        .collect()
}

/// Parse string to `u16`.
pub(super) fn parse_u16(flag: &str, text: &str) -> Result<u16, CliArgsError> {
    text.parse().map_err(|source| CliArgsError::BadInt {
        flag: flag.to_owned(),
        source,
    })
}
/// Parse string to `u32`.
pub(super) fn parse_u32(flag: &str, text: &str) -> Result<u32, CliArgsError> {
    text.parse().map_err(|source| CliArgsError::BadInt {
        flag: flag.to_owned(),
        source,
    })
}
/// Parse string to `u64`.
pub(super) fn parse_u64(flag: &str, text: &str) -> Result<u64, CliArgsError> {
    text.parse().map_err(|source| CliArgsError::BadInt {
        flag: flag.to_owned(),
        source,
    })
}
/// Parse string to `i32`.
pub(super) fn parse_i32(flag: &str, text: &str) -> Result<i32, CliArgsError> {
    text.parse().map_err(|source| CliArgsError::BadInt {
        flag: flag.to_owned(),
        source,
    })
}

/// Parse boolean value.
pub(super) fn parse_bool(flag: &str, text: &str) -> Result<bool, CliArgsError> {
    match text {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(CliArgsError::BadBool {
            flag: flag.to_owned(),
            value: text.to_owned(),
        }),
    }
}

/// Return `true` when `s` is exactly `*.<alnum+underscore>+` — a pure
/// extension glob that can be safely promoted to an `ExtensionIndex` lookup.
///
/// Examples that return `true`:  `*.dll`, `*.rs`, `*.tar_gz`, `*.7z`, `*.1`.
///
/// Examples that return `false` (must stay on the trigram path):
/// - `*.*`      — rest `*` is not alnum (matches ANY extension).
/// - `*.d??`    — question-mark not alnum.
/// - `*.[ch]`   — character class not alnum.
/// - `*.tar.gz` — dot in rest (multi-segment).
/// - `*.dll*`   — trailing star not alnum.
/// - `**/*.dll` — leading doublestar not `*.` prefix.
/// - `*.`       — empty extension.
///
/// Mirrored by `uffs_core::search::backend::is_pure_ext_glob` (the
/// daemon's belt-and-suspenders safety net at dispatch time).  Keep the
/// two definitions in sync.
pub(super) fn is_pure_ext_glob(pattern: &str) -> bool {
    pattern.strip_prefix("*.").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// Extract a pure trailing-extension alternation from a regex pattern.
///
/// Returns `Some(exts)` when `pattern` matches a narrow shape that is
/// semantically equivalent to `.*\.(e1|e2|...)$` — i.e. the extensions
/// can be routed through the `ExtensionIndex` fast path without
/// changing the result set.  Returns `None` for any more complex shape
/// so the regex stays on the full-scan path.
///
/// All accepted forms **require a trailing `$` anchor**: without it,
/// `\.jpg` matches `.jpg` anywhere in the name (e.g. `foo.jpg.txt`),
/// which the ext-index cannot replicate — it matches by the
/// `extension_id` on the trailing dot-segment only.  Requiring `$`
/// keeps the rewrite semantically lossless.
///
/// Examples that parse:
/// - `>.*\.(jpg|png|heic)$` → `Some(["jpg", "png", "heic"])`
/// - `>\.rs$`                → `Some(["rs"])`
/// - `>^\.(a|b)$`            → `Some(["a", "b"])`
/// - `>(?i).*\.(DLL|EXE)$`   → `Some(["dll", "exe"])` (lower-cased)
///
/// Examples that return `None`:
/// - `>.*\.jpg`              — missing `$`
/// - `>.*\.(tar\.gz|zip)$`   — dot inside alternation
/// - `>.*\.(jp.?)$`          — wildcard inside alternation
/// - `>C:\\Users\\.*\.dll$`  — literal prefix (must keep path-anchor semantics)
///
/// Mirrored by `uffs_core::search::dispatch::extract_extensions_from_regex`
/// (the daemon's belt-and-suspenders safety net at dispatch time).
/// Keep the two definitions in sync.
pub(super) fn extract_extensions_from_regex(pattern: &str) -> Option<Vec<String>> {
    let mut body = pattern.strip_prefix('>')?;
    if body.is_empty() {
        return None;
    }

    // Strip optional inline case-insensitive flag group.
    body = body.strip_prefix("(?i)").unwrap_or(body);
    // Strip optional start-of-string anchor.
    body = body.strip_prefix('^').unwrap_or(body);
    // Strip optional `.*` prefix (match-any-prefix).
    body = body.strip_prefix(".*").unwrap_or(body);
    // Must start with a literal dot `\.`.
    body = body.strip_prefix("\\.")?;
    // Required trailing `$` anchor.
    body = body.strip_suffix('$')?;

    let exts: Vec<String> = body
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .map_or_else(
            || vec![body.to_ascii_lowercase()],
            |group| group.split('|').map(str::to_ascii_lowercase).collect(),
        );

    exts.iter()
        .all(|ext| {
            !ext.is_empty()
                && ext
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
        .then_some(exts)
}

// NOTE: the bare-drive-prefix parser used to live here as
// `parse_bare_drive_prefix`.  It moved to the single canonical
// `uffs_mft::platform::split_drive_prefix`, shared with the daemon
// dispatch safety net, so both layers agree on what a leading `X:`
// means (including the previously-broken `C:`, `C:\`, and
// `C:\path\…` forms).

/// The token that ends option parsing: every argument after it is an
/// operand (the search pattern), even one that begins with `-`.
pub const END_OF_OPTIONS: &str = "--";

/// Split a search argument vector at the first [`END_OF_OPTIONS`] token.
///
/// Returns the options before it and the operands after it (the `--` itself
/// belongs to neither). Without a `--` every token is in the first slice.
/// Every reader of a search argument vector — this parser and the CLI's own
/// pre-scans for `--out`, `--format` and the daemon-spawn flags — reads
/// options from the first slice only, so a pattern such as `--out` after
/// `--` is never mistaken for a flag.
#[must_use]
pub fn split_end_of_options(args: &[String]) -> (&[String], &[String]) {
    args.iter()
        .position(|arg| arg == END_OF_OPTIONS)
        .map_or((args, &[]), |pos| {
            let (options, rest) = args.split_at(pos);
            (options, rest.get(1..).unwrap_or_default())
        })
}

/// The separator between alternatives in a name pattern: `*.tmp|*.bak`
/// matches a name that matches either glob (the name-pattern grammar of
/// `uffs_core::search::tree::name_matches`, which `--exclude` uses).
const NAME_PATTERN_ALTERNATIVE: &str = "|";

/// The alternatives an `--exclude` is built from: the user's own pattern and
/// the `*needle*` glob a `--not-contains` adds. A name is excluded when it
/// matches **any** of them, so they are joined with the pattern grammar's
/// [`NAME_PATTERN_ALTERNATIVE`], never another separator.
#[derive(Default)]
pub(super) struct ExcludeAlternatives(Vec<String>);

impl ExcludeAlternatives {
    /// Add one alternative when present.
    pub(super) fn with(mut self, alternative: Option<String>) -> Self {
        self.0.extend(alternative);
        self
    }

    /// The single pattern the wire carries, or `None` when nothing is
    /// excluded.
    pub(super) fn into_pattern(self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.join(NAME_PATTERN_ALTERNATIVE))
    }
}

#[cfg(test)]
mod cli_args_error_tests {
    //! Phase 5d regression tests for [`CliArgsError`] and the typed
    //! return types of every helper in this module.
    //!
    //! Each test locks one variant of [`CliArgsError`] at the
    //! byte-identical Display string the pre-Phase-5d
    //! `Result<_, String>` produced, so operator-facing CLI error
    //! output is unchanged through the migration.  Variants that
    //! chain an underlying error (`BadInt`, `BadDriveParse`,
    //! `BadSize`) additionally walk [`core::error::Error::source`]
    //! to assert the typed chain is intact — the real improvement
    //! over the previous flattened `String`.

    use core::error::Error as _;

    use super::{
        CliArgsError, drives_csv, parse_bool, parse_i32, parse_size, parse_u16, parse_u32,
        parse_u64, take_next,
    };
    use crate::format::ParseSizeError;

    #[test]
    fn take_next_missing_value_locks_display() {
        let mut empty = core::iter::empty::<String>();
        let err = take_next("--drive", &mut empty).expect_err("missing value must error");
        assert_eq!(err, CliArgsError::MissingValue {
            flag: "--drive".to_owned(),
        },);
        assert_eq!(err.to_string(), "Missing value for --drive");
        assert!(err.source().is_none(), "MissingValue has no chained source");
    }

    #[test]
    fn parse_u16_bad_int_locks_display_and_chains_source() {
        let err =
            parse_u16("--agg-page-size", "not-a-number").expect_err("non-numeric input must error");
        let CliArgsError::BadInt { flag, source } = &err else {
            panic!("expected BadInt variant, got {err:?}");
        };
        assert_eq!(flag, "--agg-page-size");
        // Display matches the pre-Phase-5d `"Bad {flag}: {err}"` shape.
        assert_eq!(err.to_string(), format!("Bad --agg-page-size: {source}"));
        // The typed source chain is the migration's actual improvement
        // over the flattened `String`.
        let chained = err.source().expect("BadInt exposes its ParseIntError");
        assert_eq!(chained.to_string(), source.to_string());
    }

    #[test]
    fn parse_u32_bad_int_locks_display() {
        let err = parse_u32("--max-descendants", "abc").expect_err("must error");
        assert!(matches!(&err, CliArgsError::BadInt { flag, .. } if flag == "--max-descendants"));
        assert!(err.to_string().starts_with("Bad --max-descendants: "));
    }

    #[test]
    fn parse_u64_bad_int_locks_display() {
        let err = parse_u64("--min-bulkiness", "xyz").expect_err("must error");
        assert!(matches!(&err, CliArgsError::BadInt { flag, .. } if flag == "--min-bulkiness"));
        assert!(err.to_string().starts_with("Bad --min-bulkiness: "));
    }

    #[test]
    fn parse_i32_bad_int_locks_display() {
        let err = parse_i32("--tz-offset", "qux").expect_err("must error");
        assert!(matches!(&err, CliArgsError::BadInt { flag, .. } if flag == "--tz-offset"));
        assert!(err.to_string().starts_with("Bad --tz-offset: "));
    }

    #[test]
    fn parse_bool_locks_display() {
        let err = parse_bool("--header", "maybe").expect_err("must error");
        assert_eq!(err, CliArgsError::BadBool {
            flag: "--header".to_owned(),
            value: "maybe".to_owned(),
        },);
        assert_eq!(err.to_string(), "Bad bool for --header: 'maybe'");
    }

    #[test]
    fn parse_bool_accepts_canonical_forms() {
        assert_eq!(parse_bool("--header", "true"), Ok(true));
        assert_eq!(parse_bool("--header", "1"), Ok(true));
        assert_eq!(parse_bool("--header", "yes"), Ok(true));
        assert_eq!(parse_bool("--header", "false"), Ok(false));
        assert_eq!(parse_bool("--header", "0"), Ok(false));
        assert_eq!(parse_bool("--header", "no"), Ok(false));
    }

    /// Empty segment in the CSV (e.g. `,C`) takes the `EmptyDrive`
    /// branch.  Locks the Display at `"empty drive"`.
    #[test]
    fn drives_csv_empty_segment_locks_display() {
        let err = drives_csv(",C").expect_err("empty leading segment must error");
        assert_eq!(err, CliArgsError::EmptyDrive);
        assert_eq!(err.to_string(), "empty drive");
    }

    /// Multi-character segment walks the `BadDrive` branch (it's not
    /// a single ASCII letter).  Display echoes the original part.
    #[test]
    fn drives_csv_multi_char_segment_locks_display() {
        let err = drives_csv("CD").expect_err("multi-char segment must error");
        assert_eq!(err, CliArgsError::BadDrive {
            input: "CD".to_owned(),
        },);
        assert_eq!(err.to_string(), "Bad drive: 'CD'");
    }

    /// Non-alphabetic segment walks `BadDrive` (digits, punctuation).
    #[test]
    fn drives_csv_non_alpha_segment_locks_display() {
        let err = drives_csv("1").expect_err("digit segment must error");
        assert_eq!(err, CliArgsError::BadDrive {
            input: "1".to_owned(),
        },);
        assert_eq!(err.to_string(), "Bad drive: '1'");
    }

    /// Happy-path lock — `drives_csv` must accept comma-separated
    /// letters with optional `:` suffix and surrounding whitespace.
    #[test]
    fn drives_csv_accepts_canonical_forms() {
        let parsed = drives_csv("C,D:,E").expect("canonical CSV must parse");
        assert_eq!(parsed.len(), 3);
    }

    /// `?`-propagation via `#[from]` lifts a `ParseSizeError` into
    /// `CliArgsError::BadSize` without losing the chain.  This is the
    /// path `from_cli_args` takes for `--min-size` / etc.
    #[test]
    fn parse_size_propagates_via_from_into_bad_size() {
        // Direct construction of the error path the `?` operator
        // would synthesise in `from_cli_args`.
        let inner = parse_size("abc").expect_err("must error");
        let outer: CliArgsError = inner.clone().into();
        assert_eq!(outer, CliArgsError::BadSize {
            source: inner.clone(),
        },);
        // Display delegates to the inner `ParseSizeError` ("invalid
        // size: abc"), matching the pre-Phase-5d byte sequence which
        // came directly from `parse_size`'s String return.
        assert_eq!(outer.to_string(), inner.to_string());
        // Chained source is the typed inner.
        let chained = outer.source().expect("BadSize exposes its ParseSizeError");
        let chained_size: &ParseSizeError = chained
            .downcast_ref()
            .expect("source must downcast to ParseSizeError");
        assert_eq!(chained_size, &inner);
    }

    #[test]
    fn unknown_flag_display_locked() {
        let err = CliArgsError::UnknownFlag {
            flag: "--nope".to_owned(),
        };
        assert_eq!(err.to_string(), "Unknown flag: '--nope'");
    }

    #[test]
    fn unexpected_argument_display_locked() {
        let err = CliArgsError::UnexpectedArgument {
            arg: "extra".to_owned(),
        };
        assert_eq!(err.to_string(), "Unexpected argument: 'extra'");
    }

    #[test]
    fn name_only_with_path_pattern_display_locked() {
        let err = CliArgsError::NameOnlyWithPathPattern;
        assert_eq!(
            err.to_string(),
            "--name-only cannot be used with path patterns containing '\\' or '/'",
        );
    }

    /// End-to-end: feeding `from_cli_args` an unknown flag must
    /// surface as the typed `UnknownFlag` variant.  This is the
    /// boundary where `cli_args.rs` raises the inline error.
    #[test]
    fn from_cli_args_surfaces_unknown_flag() {
        use crate::protocol::SearchParams;
        let args = vec!["--bogus-flag".to_owned()];
        let err = SearchParams::from_cli_args(&args).expect_err("unknown flag must error");
        assert_eq!(err, CliArgsError::UnknownFlag {
            flag: "--bogus-flag".to_owned(),
        },);
    }

    /// `--diff <baseline>` sets `diff_baseline` and composes with a plain
    /// pattern + the `*.ext` ext-glob sugar, so a snapshot-diff is just a
    /// search over the deleted set.
    #[test]
    fn from_cli_args_diff_baseline_composes_with_filters() {
        use crate::protocol::SearchParams;
        // `*.txt` is UFFS ext-glob sugar → pattern `*` + an ext filter, so the
        // diff's deleted set is filtered to `.txt`; a plain `report` pattern
        // would survive verbatim. Use the latter to pin pattern composition.
        let args = vec![
            "report".to_owned(),
            "--diff".to_owned(),
            "C_old.bin".to_owned(),
            "--drive".to_owned(),
            "C".to_owned(),
        ];
        let params = SearchParams::from_cli_args(&args).expect("valid diff search");
        assert_eq!(params.diff_baseline.as_deref(), Some("C_old.bin"));
        assert_eq!(
            params.pattern, "report",
            "the pattern still filters the deleted set"
        );
    }

    /// A bare `--diff <baseline>` (no pattern) defaults the pattern to `*` so
    /// it lists every deleted file.
    #[test]
    fn from_cli_args_bare_diff_defaults_pattern_to_star() {
        use crate::protocol::SearchParams;
        let args = vec![
            "--diff".to_owned(),
            "C_old.bin".to_owned(),
            "--drive".to_owned(),
            "C".to_owned(),
        ];
        let params = SearchParams::from_cli_args(&args).expect("valid bare diff");
        assert_eq!(params.diff_baseline.as_deref(), Some("C_old.bin"));
        assert_eq!(params.pattern, "*", "a bare diff lists all deleted files");
    }

    /// End-to-end: a second positional argument after the pattern
    /// surfaces as `UnexpectedArgument`.
    #[test]
    fn from_cli_args_surfaces_unexpected_argument() {
        use crate::protocol::SearchParams;
        let args = vec!["pattern1".to_owned(), "pattern2".to_owned()];
        let err = SearchParams::from_cli_args(&args).expect_err("second positional must error");
        assert_eq!(err, CliArgsError::UnexpectedArgument {
            arg: "pattern2".to_owned(),
        },);
    }

    /// End-to-end: `--name-only` plus a path pattern surfaces as
    /// `NameOnlyWithPathPattern`.
    #[test]
    fn from_cli_args_surfaces_name_only_with_path_pattern() {
        use crate::protocol::SearchParams;
        let args = vec!["foo/bar".to_owned(), "--name-only".to_owned()];
        let err = SearchParams::from_cli_args(&args).expect_err("name-only+path must error");
        assert_eq!(err, CliArgsError::NameOnlyWithPathPattern);
    }

    /// End-to-end: `--min-size abc` surfaces a typed `BadSize` after
    /// the `?` From-conversion in `from_cli_args`.  Display matches
    /// the pre-Phase-5d `"invalid size: abc"` text.
    #[test]
    fn from_cli_args_surfaces_bad_size() {
        use crate::protocol::SearchParams;
        let args = vec!["*".to_owned(), "--min-size".to_owned(), "abc".to_owned()];
        let err = SearchParams::from_cli_args(&args).expect_err("bad size must error");
        let CliArgsError::BadSize { source } = &err else {
            panic!("expected BadSize variant, got {err:?}");
        };
        assert_eq!(source, &ParseSizeError::InvalidNumber {
            spec: "abc".to_owned(),
        },);
        assert_eq!(err.to_string(), "invalid size: abc");
    }
}
