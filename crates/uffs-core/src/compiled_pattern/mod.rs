// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Compiled pattern IR for optimized Polars expression lowering.
//!
//! This module provides a Pattern Intermediate Representation (IR) that enables
//! specialized Polars string kernels instead of regex-everything approach.
//!
//! # Performance Benefits
//!
//! | Pattern Type | Regex Approach | Optimized Approach | Speedup |
//! |--------------|----------------|-------------------|---------|
//! | `*.txt`      | `contains("^.*\\.txt$")` | `ends_with(".txt")` | 5-10x |
//! | `foo*`       | `contains("^foo.*$")` | `starts_with("foo")` | 5-10x |
//! | `*needle*`   | `contains(".*needle.*")` | `contains_literal("needle")` | 2-5x |
//!
//! # Example
//!
//! ```rust,ignore
//! use uffs_core::compiled_pattern::{CompiledPattern, compile_pattern};
//! use uffs_core::pattern::ParsedPattern;
//!
//! let parsed = ParsedPattern::parse("*.txt")?;
//! let compiled = compile_pattern(&parsed)?;
//!
//! // compiled is CompiledPattern::Suffix(".txt")
//! // which lowers to col("name").str().ends_with(lit(".txt"))
//! let expr = compiled.to_expr("name", true);
//! ```

// Helper functions separated for testability and code clarity
#![expect(
    clippy::single_call_fn,
    reason = "helper functions extracted for testability and code clarity"
)]

mod glob;

pub use glob::{GlobKind, classify_glob, compile_pattern};
use uffs_polars::{Expr, NamedFrom as _, PlSmallStr, Series, col, lit};

// ============================================================================
// CompiledPattern - The Pattern IR
// ============================================================================

/// Compiled pattern ready for Polars expression lowering.
///
/// Each variant maps to the most efficient Polars expression for that pattern
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledPattern {
    /// Always matches (e.g., `*`).
    /// Lowers to: `lit(true)`
    Any,

    /// Exact string match: `col == "value"`.
    /// Lowers to: `col.eq(lit(value))`
    Exact(String),

    /// Prefix match: `col.str().starts_with("prefix")`.
    /// Lowers to: `col.str().starts_with(lit(prefix))`
    Prefix(String),

    /// Suffix match: `col.str().ends_with("suffix")`.
    /// Lowers to: `col.str().ends_with(lit(suffix))`
    Suffix(String),

    /// Literal substring: `col.str().contains_literal("needle")`.
    /// Lowers to: `col.str().contains_literal(lit(needle))`
    Contains(String),

    /// Prefix AND suffix: `starts_with(p) & ends_with(s)`.
    /// Lowers to:
    /// `col.str().starts_with(lit(p)).and(col.str().ends_with(lit(s)))`
    PrefixSuffix {
        /// The prefix to match.
        prefix: String,
        /// The suffix to match.
        suffix: String,
    },

    /// Multiple exact matches: `col.is_in([...])`.
    /// Lowers to: `col.is_in(lit(series).implode(true), false)` (Polars 2.0+)
    ExactSet(Vec<String>),

    /// Multiple literal substrings: `col.str().contains_any([...])`.
    /// Lowers to: `col.str().contains_any(lit(series).implode(true),
    /// ascii_case_insensitive)` (Polars 2.0+)
    ContainsAny(Vec<String>),

    /// Multiple suffixes (extensions): OR of `ends_with` calls.
    /// Lowers to: `ends_with(s1) | ends_with(s2) | ...`
    SuffixSet(Vec<String>),

    /// Fallback to regex: `col.str().contains(regex, strict)`.
    /// Lowers to: `col.str().contains(lit(pattern), true)`
    Regex {
        /// The regex pattern.
        pattern: String,
        /// Whether the regex is anchored (^...$).
        anchored: bool,
    },
}

// ============================================================================
// Expression Lowering
// ============================================================================

impl CompiledPattern {
    /// Lower this pattern to an optimized Polars expression.
    ///
    /// This method generates the most efficient Polars expression for each
    /// pattern variant, using specialized string kernels where possible.
    ///
    /// # Arguments
    ///
    /// * `column` - The column name to match against (e.g., "name", "path")
    /// * `case_sensitive` - Whether matching should be case-sensitive
    ///
    /// # Returns
    ///
    /// A Polars `Expr` that evaluates to a boolean mask for matching rows.
    ///
    /// # Polars 2.0+ API Notes
    ///
    /// - `is_in` requires `.implode(true)` to wrap values as `List` type
    /// - `contains_any` requires `.implode(true)` for the patterns series
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use uffs_core::compiled_pattern::CompiledPattern;
    ///
    /// let pattern = CompiledPattern::Suffix(".txt".to_string());
    /// let expr = pattern.to_expr("name", true);
    /// // expr is: col("name").str().ends_with(lit(".txt"))
    /// ```
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive match over 9 pattern variants × case-sensitive/insensitive — \
                  each arm produces a distinct Polars expression; splitting would scatter \
                  the pattern→expr mapping across functions"
    )]
    pub(crate) fn to_expr(&self, column: &str, case_sensitive: bool) -> Expr {
        // For case-sensitive matching, use optimized string operations
        // directly. For case-insensitive matching, we use regex with
        // (?i) flag which is more efficient than calling to_lowercase()
        // on every row.
        let col_expr = col(column);

        match self {
            // Any: always matches
            Self::Any => lit(true),

            // Exact: direct equality check (case-sensitive) or regex
            // (case-insensitive)
            Self::Exact(value) => {
                if case_sensitive {
                    col_expr.eq(lit(value.clone()))
                } else {
                    // Use regex for case-insensitive exact match
                    let escaped = regex_escape(value);
                    col_expr
                        .str()
                        .contains(lit(format!("(?i)^{escaped}$")), true)
                }
            }

            // Prefix: starts_with (case-sensitive) or regex (case-insensitive)
            Self::Prefix(prefix) => {
                if case_sensitive {
                    col_expr.str().starts_with(lit(prefix.clone()))
                } else {
                    let escaped = regex_escape(prefix);
                    col_expr
                        .str()
                        .contains(lit(format!("(?i)^{escaped}")), true)
                }
            }

            // Suffix: ends_with (case-sensitive) or regex (case-insensitive)
            Self::Suffix(suffix) => {
                if case_sensitive {
                    col_expr.str().ends_with(lit(suffix.clone()))
                } else {
                    let escaped = regex_escape(suffix);
                    col_expr
                        .str()
                        .contains(lit(format!("(?i){escaped}$")), true)
                }
            }

            // Contains: contains_literal (case-sensitive) or regex
            // (case-insensitive)
            Self::Contains(needle) => {
                if case_sensitive {
                    col_expr.str().contains_literal(lit(needle.clone()))
                } else {
                    let escaped = regex_escape(needle);
                    col_expr.str().contains(lit(format!("(?i){escaped}")), true)
                }
            }

            // PrefixSuffix: combine starts_with AND ends_with
            Self::PrefixSuffix { prefix, suffix } => {
                if case_sensitive {
                    col_expr
                        .clone()
                        .str()
                        .starts_with(lit(prefix.clone()))
                        .and(col_expr.str().ends_with(lit(suffix.clone())))
                } else {
                    let escaped_prefix = regex_escape(prefix);
                    let escaped_suffix = regex_escape(suffix);
                    col_expr.str().contains(
                        lit(format!("(?i)^{escaped_prefix}.*{escaped_suffix}$")),
                        true,
                    )
                }
            }

            // ExactSet: is_in with hash lookup
            // For case-insensitive, we lowercase both column and values
            Self::ExactSet(values) => {
                if case_sensitive {
                    let series = Series::new(PlSmallStr::EMPTY, values);
                    col_expr.is_in(lit(series).implode(true), false)
                } else {
                    // For case-insensitive, lowercase both column and values
                    let lower_values: Vec<String> =
                        values.iter().map(|val| val.to_lowercase()).collect();
                    let series = Series::new(PlSmallStr::EMPTY, &lower_values);
                    col_expr
                        .str()
                        .to_lowercase()
                        .is_in(lit(series).implode(true), false)
                }
            }

            // ContainsAny: Aho-Corasick multi-pattern matching
            Self::ContainsAny(patterns) => {
                if case_sensitive {
                    let series = Series::new(PlSmallStr::EMPTY, patterns);
                    col_expr
                        .str()
                        .contains_any(lit(series).implode(true), false)
                } else {
                    // Use ascii_case_insensitive=true for case-insensitive
                    // matching
                    let series = Series::new(PlSmallStr::EMPTY, patterns);
                    col_expr.str().contains_any(lit(series).implode(true), true)
                }
            }

            // SuffixSet: OR of ends_with calls
            Self::SuffixSet(suffixes) => {
                if suffixes.is_empty() {
                    return lit(false);
                }
                if case_sensitive {
                    suffixes
                        .iter()
                        .map(|suf| col_expr.clone().str().ends_with(lit(suf.clone())))
                        .reduce(Expr::or)
                        .unwrap_or_else(|| lit(false))
                } else {
                    // Use regex with alternation for case-insensitive
                    let escaped: Vec<String> =
                        suffixes.iter().map(|suf| regex_escape(suf)).collect();
                    let pattern = format!("(?i)(?:{})$", escaped.join("|"));
                    col_expr.str().contains(lit(pattern), true)
                }
            }

            // Regex: fallback to regex engine
            Self::Regex { pattern, anchored } => {
                let regex_pattern = if *anchored && !pattern.starts_with('^') {
                    format!("^(?:{pattern})$")
                } else if case_sensitive {
                    pattern.clone()
                } else {
                    format!("(?i){pattern}")
                };
                // strict=true means invalid regex is an error
                col_expr.str().contains(lit(regex_pattern), true)
            }
        }
    }
}

/// Escape special regex characters in a string.
#[inline]
fn regex_escape(input: &str) -> String {
    let mut result = String::with_capacity(input.len() * 2);
    for ch in input.chars() {
        match ch {
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
}

#[cfg(test)]
mod tests;
