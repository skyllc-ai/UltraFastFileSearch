// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Why a search could not run, and the one place a `>regex` pattern is
//! compiled.

/// Why a search could not run at all.
///
/// A search that runs and finds nothing is an empty
/// [`super::backend::SearchResult`]; a pattern that cannot be searched for is
/// this error, so "no match" and "bad pattern" never look the same.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// A `>` pattern is not a valid regular expression.
    #[error("invalid regex `{pattern}`: {reason}")]
    InvalidRegex {
        /// The regex as given (without the leading `>`).
        pattern: String,
        /// The regex engine's explanation.
        reason: String,
    },
}

/// Compile the regex of a `>regex` search pattern, or `None` when `pattern`
/// is not a regex pattern.
///
/// The regex is compiled from the pattern **as typed**: case-insensitive
/// search is the regex engine's own flag, never a case-folded copy of the
/// pattern (folding turned `\d \w \s \b` into `\D \W \S \B`).
///
/// # Errors
///
/// [`SearchError::InvalidRegex`] when the regex does not compile.
pub(crate) fn compile_search_regex(
    pattern: &str,
    case_sensitive: bool,
) -> Result<Option<regex::Regex>, SearchError> {
    let Some(body) = pattern.strip_prefix('>').filter(|body| !body.is_empty()) else {
        return Ok(None);
    };
    regex::RegexBuilder::new(body)
        .case_insensitive(!case_sensitive)
        .build()
        .map(Some)
        .map_err(|err| SearchError::InvalidRegex {
            pattern: body.to_owned(),
            reason: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::{SearchError, compile_search_regex};

    #[test]
    fn non_regex_patterns_compile_to_none() {
        for pattern in ["*.rs", "report", ">", ""] {
            assert_eq!(
                compile_search_regex(pattern, false).map(|re| re.is_some()),
                Ok(false),
                "{pattern}"
            );
        }
    }

    #[test]
    fn case_insensitive_regex_keeps_its_escapes() {
        let re = compile_search_regex(r">^report\d+\.txt$", false)
            .expect("valid regex")
            .expect("a regex pattern");
        assert!(re.is_match("REPORT2024.TXT"), "case-insensitive");
        assert!(re.is_match("report7.txt"));
        assert!(
            !re.is_match("reportX.txt"),
            r"\d must stay a digit class, not become \D"
        );
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let err = compile_search_regex(">(unclosed", false).expect_err("invalid regex");
        assert!(
            matches!(&err, SearchError::InvalidRegex { pattern, .. } if pattern == "(unclosed"),
            "{err}"
        );
    }
}
