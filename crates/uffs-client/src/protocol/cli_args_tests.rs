// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for the search argument parser: the flags whose meaning the parser
//! itself decides (`--word`, `--exclude` with `--not-contains`, `--`).

use super::{CliArgsError, SearchParams, split_end_of_options};

/// Parse an argument vector given as string slices.
fn parse(args: &[&str]) -> Result<SearchParams, CliArgsError> {
    let owned: Vec<String> = args.iter().copied().map(String::from).collect();
    SearchParams::from_cli_args(&owned)
}

#[test]
fn word_flag_turns_on_whole_word_matching() {
    let with = parse(&["test", "--word"]).expect("valid search");
    assert!(
        with.whole_word,
        "--word must reach the daemon as whole_word"
    );
    let without = parse(&["test"]).expect("valid search");
    assert!(!without.whole_word, "whole_word stays off without --word");
}

#[test]
fn exclude_and_not_contains_become_one_alternation() {
    let params = parse(&["*", "--exclude", "*.tmp", "--not-contains", "draft"]).expect("valid");
    assert_eq!(params.exclude.as_deref(), Some("*.tmp|*draft*"));
    let only_nc = parse(&["*", "--not-contains", "draft"]).expect("valid");
    assert_eq!(only_nc.exclude.as_deref(), Some("*draft*"));
    let only_ex = parse(&["*", "--exclude", "*.tmp|*.bak"]).expect("valid");
    assert_eq!(only_ex.exclude.as_deref(), Some("*.tmp|*.bak"));
    let neither = parse(&["*"]).expect("valid");
    assert_eq!(neither.exclude, None);
}

#[test]
fn double_dash_makes_a_leading_dash_pattern_an_operand() {
    let params = parse(&["--", "--draft*"]).expect("pattern after -- must parse");
    assert_eq!(params.pattern, "--draft*");
    let with_flags = parse(&["--files-only", "--", "-x"]).expect("valid");
    assert_eq!(with_flags.pattern, "-x");
    assert_eq!(
        with_flags.filter.as_deref(),
        Some("files"),
        "options before -- still apply"
    );
}

#[test]
fn double_dash_still_takes_one_pattern() {
    let err = parse(&["a", "--", "b"]).expect_err("two patterns must error");
    assert!(
        matches!(err, CliArgsError::UnexpectedArgument { .. }),
        "{err}"
    );
}

#[test]
fn split_end_of_options_separates_options_from_operands() {
    let args: Vec<String> = ["--out", "x", "--", "--out"]
        .into_iter()
        .map(String::from)
        .collect();
    let (options, operands) = split_end_of_options(&args);
    assert_eq!(options, ["--out", "x"]);
    assert_eq!(operands, ["--out"]);
    let plain: Vec<String> = vec!["*.rs".to_owned()];
    assert_eq!(split_end_of_options(&plain), (plain.as_slice(), &[][..]));
}

#[test]
fn removed_no_op_flags_are_unknown() {
    for flag in [
        "--query-mode",
        "--no-bitmap",
        "--debug-tree",
        "--chaos-seed",
        "--reserved-allocated",
    ] {
        let err = parse(&["*", flag]).expect_err("a flag that does nothing is refused");
        assert!(
            matches!(err, CliArgsError::UnknownFlag { .. }),
            "{flag}: {err}"
        );
    }
}
