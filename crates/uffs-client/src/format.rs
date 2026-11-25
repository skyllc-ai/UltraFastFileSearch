// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Shared display formatting utilities.
//!
//! Lightweight formatters for numbers, bytes, timestamps, and durations.
//! Used by the thin CLI client and MCP server — no heavy dependencies.
//!
//! These are intentionally duplicated from `uffs-core::format` so the CLI
//! binary does not need to link `uffs-core` (and transitively, polars).

/// Formats a number with comma separators for readability.
///
/// Examples: `1234567` → `"1,234,567"`, `1000` → `"1,000"`
#[must_use]
pub fn format_number_commas(num: u64) -> String {
    let num_str = num.to_string();
    let mut result = String::with_capacity(num_str.len() + num_str.len() / 3);
    for (idx, ch) in num_str.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Formats a byte count in human-readable form based on magnitude.
///
/// - < 1 KB: `1234 B`
/// - < 1 MB: `123.45 KB`
/// - < 1 GB: `123.45 MB`
/// - < 1 TB: `123.45 GB`
/// - >= 1 TB: `123.45 TB`
#[must_use]
#[expect(
    clippy::float_arithmetic,
    reason = "floating-point arithmetic required for human-readable byte formatting"
)]
pub fn format_bytes(bytes: u64) -> String {
    let bytes_f64 = u64_to_f64(bytes);
    if bytes < 1024 {
        format!("{bytes:>4} B")
    } else if bytes < 1024 * 1024 {
        format!("{:>7.2} KB", bytes_f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:>7.2} MB", bytes_f64 / (1024.0 * 1024.0))
    } else if bytes < 1024 * 1024 * 1024 * 1024 {
        format!("{:>7.2} GB", bytes_f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!(
            "{:>7.2} TB",
            bytes_f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0)
        )
    }
}

/// Formats a duration intelligently based on magnitude.
///
/// - Days+: `2d 3h 5m 10s`
/// - Hours+: `3h 5m 10s`
/// - Minutes+: `5 m 10 s`
/// - Seconds+: `10 s 500 ms`
/// - Milliseconds+: `500 ms 250 μs`
/// - Sub-ms: `250 μs 100 ns`
#[must_use]
pub fn format_duration(duration: core::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = (total_seconds / 3600) % 24;
    let days = total_seconds / 86400;
    let milliseconds = duration.subsec_millis();
    let microseconds = duration.subsec_micros() % 1_000;
    let nanoseconds = duration.subsec_nanos() % 1_000;

    if days > 0 {
        format!("{days:>2}d {hours:>2}h {minutes:>2}m {seconds:>2}s")
    } else if hours > 0 {
        format!("{hours:>2}h {minutes:>2}m {seconds:>2}s")
    } else if minutes > 0 {
        format!("{minutes:>3} m  {seconds:>3} s ")
    } else if seconds > 0 {
        format!("{seconds:>3} s  {milliseconds:>3} ms")
    } else if milliseconds > 0 {
        format!("{milliseconds:>3} ms {microseconds:>3} μs")
    } else if microseconds > 0 {
        format!("{microseconds:>3} μs {nanoseconds:>3} ns")
    } else {
        format!("{nanoseconds:>3} ns")
    }
}

/// Convert a non-negative `f64` to `u64`, clamping negatives to 0.
///
/// Used for converting floating-point statistics (averages, etc.) back to
/// integer representations for formatting.
#[inline]
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "centralized f64→u64 conversion; precision loss at u64::MAX boundary is acceptable"
)]
pub fn f64_to_u64(val: f64) -> u64 {
    if val <= 0.0 {
        0
    } else if val >= u64::MAX as f64 {
        u64::MAX
    } else {
        val as u64
    }
}

/// Convert a `u64` to `f64` for display ratios and percentages.
///
/// Precision loss beyond 2^53 is acceptable for display.
#[inline]
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "display-only: sub-unit precision irrelevant for ratios"
)]
pub const fn u64_to_f64(val: u64) -> f64 {
    val as f64
}

/// Returns the local UTC offset in seconds (e.g. `-25200` for PDT / UTC−7).
///
/// Matches the C++ behavior where `FileTimeToLocalFileTime()` applies the
/// CURRENT timezone offset to ALL timestamps, ignoring historical DST
/// transitions.  Computed once at startup via platform APIs — no `chrono`
/// dependency required.
///
/// Falls back to `0` (UTC) on any platform error.
#[must_use]
pub fn local_utc_offset_secs() -> i32 {
    platform_tz::utc_offset_secs()
}

/// Platform-specific UTC offset detection.
#[cfg(unix)]
mod platform_tz {
    /// Get local UTC offset via `libc::localtime_r` → `tm_gmtoff`.
    #[expect(unsafe_code, reason = "FFI call to libc localtime_r")]
    pub(super) fn utc_offset_secs() -> i32 {
        use std::time::{SystemTime, UNIX_EPOCH};

        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs());

        // Use `try_from` instead of `as` so we neither trigger nor need to
        // suppress `clippy::cast_possible_wrap`.  Nightly clippy's handling
        // of `u64 as libc::time_t` differs between macOS (`time_t = c_long`,
        // lint fires) and Linux (`time_t = i64`, lint no longer fires),
        // which made a static `#[expect(cast_possible_wrap)]` platform-
        // dependently stale.  `try_from` is portable and the saturating
        // fallback is unreachable for any realistic Unix epoch.
        let epoch = libc::time_t::try_from(secs).unwrap_or(libc::time_t::MAX);

        // Safety: `core::mem::zeroed()` produces a valid `libc::tm` —
        // it is a plain-old-data struct with no invariants.
        let mut tm_buf: libc::tm = unsafe { core::mem::zeroed() };
        // Safety: `localtime_r` is the thread-safe variant; we provide
        // a valid `time_t` pointer and a valid output buffer pointer.
        let result = unsafe { libc::localtime_r(&raw const epoch, &raw mut tm_buf) };
        if result.is_null() {
            return 0; // fallback to UTC
        }

        // `tm_gmtoff` is at most ±50400 (±14h) by POSIX timezone spec,
        // so the saturating `try_from` fallbacks are unreachable.  This
        // replaces the previous truncating `as i32` cast.
        i32::try_from(tm_buf.tm_gmtoff).unwrap_or(0)
    }
}

/// Platform-specific UTC offset detection.
#[cfg(windows)]
mod platform_tz {
    /// `GetTimeZoneInformation` returns `2` when daylight saving is active.
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

    /// Get local UTC offset via `GetTimeZoneInformation`.
    #[expect(unsafe_code, reason = "FFI call to Win32 GetTimeZoneInformation")]
    pub(super) fn utc_offset_secs() -> i32 {
        use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

        let mut tz_info = TIME_ZONE_INFORMATION::default();
        // SAFETY: passing a valid mutable pointer to a stack-allocated
        // struct that outlives the call.
        let result = unsafe { GetTimeZoneInformation(core::ptr::from_mut(&mut tz_info)) };

        // `Bias` is in minutes, UTC = LocalTime + Bias.
        // `DaylightBias` is typically −60 (summer), 0 otherwise.
        let total_bias_minutes = if result == TIME_ZONE_ID_DAYLIGHT {
            tz_info.Bias + tz_info.DaylightBias
        } else {
            tz_info.Bias
        };

        // Convert from "bias" (UTC = Local + Bias) to "offset" (Local = UTC +
        // offset). offset = −Bias.  `Bias` is documented as
        // signed-minutes in range ±14 hours, so `× 60` stays well
        // within `i32`.
        -(total_bias_minutes * 60)
    }
}

/// Fallback for non-Unix, non-Windows platforms.
#[cfg(not(any(unix, windows)))]
mod platform_tz {
    /// Returns 0 (UTC) — no platform API available.
    pub(super) const fn utc_offset_secs() -> i32 {
        0
    }
}

/// Parse a month/quarter spec into a vector of allowed months (1-12).
///
/// Accepts:
/// - Month names: `january`, `jan`, `february`, `feb`, … , `december`, `dec`
/// - Quarter names: `Q1`, `Q2`, `Q3`, `Q4`
/// - Comma-separated combinations: `jan,feb`, `Q1,Q3`
///
/// ```
/// # use uffs_client::format::parse_month_spec;
/// assert_eq!(parse_month_spec("january"), vec![1]);
/// assert_eq!(parse_month_spec("Q1"), vec![1, 2, 3]);
/// assert_eq!(parse_month_spec("jan,feb"), vec![1, 2]);
/// ```
#[must_use]
pub fn parse_month_spec(spec: &str) -> Vec<u32> {
    let mut months = Vec::new();
    for token in spec.split(',') {
        let lower = token.trim().to_ascii_lowercase();
        match lower.as_str() {
            "january" | "jan" => months.push(1),
            "february" | "feb" => months.push(2),
            "march" | "mar" => months.push(3),
            "april" | "apr" => months.push(4),
            "may" => months.push(5),
            "june" | "jun" => months.push(6),
            "july" | "jul" => months.push(7),
            "august" | "aug" => months.push(8),
            "september" | "sep" => months.push(9),
            "october" | "oct" => months.push(10),
            "november" | "nov" => months.push(11),
            "december" | "dec" => months.push(12),
            "q1" => months.extend_from_slice(&[1, 2, 3]),
            "q2" => months.extend_from_slice(&[4, 5, 6]),
            "q3" => months.extend_from_slice(&[7, 8, 9]),
            "q4" => months.extend_from_slice(&[10, 11, 12]),
            _ => {} // silently ignore unknown tokens
        }
    }
    months.sort_unstable();
    months.dedup();
    months
}

/// Typed error returned by [`parse_size`].
///
/// Phase 5d migration of the previous `Result<u64, String>` return
/// type: the [`core::fmt::Display`] strings stay byte-identical with
/// the pre-migration messages so any operator-facing CLI error output
/// is unchanged, while callers can now match on variants instead of
/// parsing the string.
///
/// `#[non_exhaustive]` per Phase 5c discipline so a future failure
/// mode (e.g. overflow on the saturating-mul, or a negative-prefix
/// rejection) can grow a variant without a semver bump.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseSizeError {
    /// Input was empty after trimming.
    #[error("empty size specification")]
    Empty,
    /// The numeric portion failed `u64::from_str`.  The original
    /// untrimmed spec is echoed back so the operator sees what they
    /// typed.
    #[error("invalid size: {spec}")]
    InvalidNumber {
        /// The original, untrimmed input.
        spec: String,
    },
}

/// Parse a human-readable size spec into bytes.
///
/// Accepts: `1024`, `10KB`, `5MB`, `2GB`, `1TB`, `100B`.
/// Case-insensitive suffixes.
///
/// # Errors
///
/// Returns [`ParseSizeError::Empty`] when the input is empty after
/// trimming, or [`ParseSizeError::InvalidNumber`] when the digit
/// portion fails `u64::from_str`.  The Display strings stay
/// byte-identical with the pre-Phase-5d output so any operator-facing
/// CLI output is unchanged.
pub fn parse_size(spec: &str) -> Result<u64, ParseSizeError> {
    const SUFFIXES: &[(&str, u64)] = &[
        ("TB", 1024 * 1024 * 1024 * 1024),
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("B", 1),
    ];

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(ParseSizeError::Empty);
    }

    let upper = trimmed.to_ascii_uppercase();

    let (digits, multiplier) = SUFFIXES
        .iter()
        .find_map(|(suffix, mult)| upper.strip_suffix(suffix).map(|rest| (rest, *mult)))
        .unwrap_or((upper.as_str(), 1));

    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_parse_err| ParseSizeError::InvalidNumber {
            spec: spec.to_owned(),
        })?;

    Ok(count.saturating_mul(multiplier))
}

/// One aggregate preset as the command line names it: its name and the
/// other spellings accepted for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregatePresetName {
    /// The preset's name (`by_extension`), as help text lists it.
    pub name: &'static str,
    /// Other accepted spellings (`byextension`, `extension`, `by_ext`, `ext`).
    pub aliases: &'static [&'static str],
}

/// Every aggregate preset, in the order help text lists them.
///
/// The one list the client side reads: `is_aggregate_preset` validates
/// against it and `uffs --agg --help` prints it, so the two cannot drift.
/// The daemon expands presets with `uffs_core::aggregate::AggregatePreset`;
/// a test in `uffs-daemon` holds that parser to this list.
pub const AGGREGATE_PRESETS: &[AggregatePresetName] = &[
    AggregatePresetName {
        name: "overview",
        aliases: &[],
    },
    AggregatePresetName {
        name: "by_type",
        aliases: &["bytype", "type"],
    },
    AggregatePresetName {
        name: "by_extension",
        aliases: &["byextension", "extension", "by_ext", "ext"],
    },
    AggregatePresetName {
        name: "by_drive",
        aliases: &["bydrive", "drive"],
    },
    AggregatePresetName {
        name: "by_size",
        aliases: &["bysize", "size"],
    },
    AggregatePresetName {
        name: "by_age",
        aliases: &["byage", "age"],
    },
    AggregatePresetName {
        name: "storage",
        aliases: &[],
    },
    AggregatePresetName {
        name: "activity",
        aliases: &[],
    },
    AggregatePresetName {
        name: "top_folders",
        aliases: &["topfolders", "folders"],
    },
    AggregatePresetName {
        name: "duplicates",
        aliases: &["dups"],
    },
    AggregatePresetName {
        name: "media",
        aliases: &[],
    },
    AggregatePresetName {
        name: "cleanup",
        aliases: &[],
    },
];

/// Check whether a string is a recognized aggregate preset name (any case,
/// any spelling in [`AGGREGATE_PRESETS`]).
///
/// The full expansion and execution of presets happens on the daemon side;
/// this is a lightweight check so the thin CLI can validate user input
/// before sending it over the wire.
#[must_use]
pub(crate) fn is_aggregate_preset(spec: &str) -> bool {
    AGGREGATE_PRESETS.iter().any(|preset| {
        preset.name.eq_ignore_ascii_case(spec)
            || preset
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(spec))
    })
}

/// Extract a drive letter from a search pattern, if present.
///
/// Patterns like `c:/*.txt` or `D:\folder` start with a drive prefix.
/// Returns the typed `DriveLetter` if found, `None` otherwise.
///
/// ```
/// # use uffs_client::format::extract_drive_letter;
/// # use uffs_mft::platform::DriveLetter;
/// assert_eq!(extract_drive_letter("c:/*.txt"), Some(DriveLetter::C));
/// assert_eq!(extract_drive_letter("D:\\folder"), Some(DriveLetter::D));
/// assert_eq!(extract_drive_letter("*.txt"), None);
/// assert_eq!(extract_drive_letter(">regex"), None);
/// ```
#[must_use]
pub fn extract_drive_letter(pattern: &str) -> Option<uffs_mft::platform::DriveLetter> {
    let bytes = pattern.as_bytes();
    let first = *bytes.first()?;
    let second = *bytes.get(1)?;
    if second != b':' || !first.is_ascii_alphabetic() {
        return None;
    }
    uffs_mft::platform::DriveLetter::parse(first as char).ok()
}

#[cfg(test)]
mod parse_size_error_tests {
    //! Phase 5d regression tests for [`ParseSizeError`].
    //!
    //! Locks each variant's Display string at the byte-identical text
    //! the pre-Phase-5d `Result<u64, String>` returns produced, so any
    //! operator-facing CLI error output stays unchanged through the
    //! migration.

    use super::{ParseSizeError, parse_size};

    /// Empty input is rejected with the `Empty` variant whose Display
    /// is byte-identical to the pre-Phase-5d `"empty size specification"`
    /// message.
    #[test]
    fn empty_input_returns_empty_variant_with_locked_display() {
        let err = parse_size("").expect_err("empty input must error");
        assert_eq!(err, ParseSizeError::Empty);
        assert_eq!(err.to_string(), "empty size specification");
    }

    /// Whitespace-only input also walks the `Empty` branch (the
    /// pre-trim guard catches it).  Locks the operator-friendly
    /// behaviour rather than a stricter
    /// "looks-like-no-digits" alternative.
    #[test]
    fn whitespace_only_input_returns_empty_variant() {
        let err = parse_size("   ").expect_err("whitespace-only input must error");
        assert_eq!(err, ParseSizeError::Empty);
    }

    /// Non-numeric input is rejected with `InvalidNumber` whose
    /// Display interpolates the original (untrimmed) spec verbatim —
    /// byte-identical to the pre-Phase-5d
    /// `"invalid size: {spec}"` message.
    #[test]
    fn non_numeric_input_returns_invalid_number_with_locked_display() {
        let err = parse_size("abc").expect_err("non-numeric input must error");
        assert_eq!(err, ParseSizeError::InvalidNumber {
            spec: "abc".to_owned(),
        },);
        assert_eq!(err.to_string(), "invalid size: abc");
    }

    /// A bare suffix with no digits (e.g. `MB`) is also rejected via
    /// `InvalidNumber` — the suffix-strip leaves an empty digit slot,
    /// which fails [`u64::from_str`].  The spec field echoes the
    /// original input untouched.
    #[test]
    fn bare_suffix_returns_invalid_number_echoing_original_spec() {
        let err = parse_size("MB").expect_err("bare suffix input must error");
        assert_eq!(err, ParseSizeError::InvalidNumber {
            spec: "MB".to_owned(),
        },);
        assert_eq!(err.to_string(), "invalid size: MB");
    }

    /// Negative inputs (e.g. `-1KB`) are rejected — `u64::from_str`
    /// refuses the leading minus.  Locks the existing behaviour that
    /// the CLI rejects negative sizes rather than silently treating
    /// them as zero.
    #[test]
    fn negative_input_returns_invalid_number() {
        let err = parse_size("-1KB").expect_err("negative input must error");
        assert_eq!(err, ParseSizeError::InvalidNumber {
            spec: "-1KB".to_owned(),
        },);
    }

    /// Happy-path lock — typed return value must round-trip the
    /// expected byte count for the canonical suffixes.  Guards against
    /// accidental regression of the success arm during the migration.
    #[test]
    fn happy_path_round_trips_canonical_suffixes() {
        assert_eq!(parse_size("0"), Ok(0));
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("1KB"), Ok(1024));
        assert_eq!(parse_size("1MB"), Ok(1024 * 1024));
        assert_eq!(parse_size("1GB"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_size("1TB"), Ok(1024_u64 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("  1MB  "), Ok(1024 * 1024));
    }
}
