// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Integration tests for the `uffs` CLI binary.

// This target links its package's full dependency list but uses only
// part of it.  Naming the rest keeps `unused_crate_dependencies` exact on
// every toolchain (newer nightlies stopped reporting it for bench, bin and
// test targets; the pinned one still does), with no lint suppression.
use anyhow as _;
use dirs_next as _;
use serde_json as _;
use strsim as _;
use tempfile as _;
use uffs_broker_protocol as _;
use uffs_client as _;
use uffs_format as _;
use uffs_mft as _;
use uffs_security as _;
use uffs_statusfmt as _;
use uffs_time as _;
use uffs_version as _;
use uffs_winsvc as _;
use which as _;

#[cfg(test)]
mod tests {
    use std::process::{self, Output};
    use std::time::{SystemTime, UNIX_EPOCH};

    use assert_cmd::Command;

    fn run_cli(test_name: &str, args: &[&str]) -> Output {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let log_dir = std::env::temp_dir().join(format!(
            "uffs-cli-tests-{test_name}-{}-{nanos}",
            process::id()
        ));

        Command::cargo_bin("uffs")
            .expect("uffs test binary should build")
            .env("NO_COLOR", "1")
            // Never auto-spawn a real `uffsd` from a test: the workspace
            // build puts one right next to the test binary, and a spawned
            // daemon is up to 120 s of readiness-polling nondeterminism
            // plus a leaked process per run (CI flake, issue #610). With
            // the kill-switch, daemonless failures are immediate and
            // their message is stable.
            .env("UFFS_NO_AUTOSTART", "1")
            .env("UFFS_LOG_DIR", log_dir)
            .env_remove("RUST_LOG")
            .env_remove("RUST_LOG_FILE")
            .args(args)
            .output()
            .expect("CLI command should run")
    }

    fn assert_success(test_name: &str, args: &[&str], stdout_snippets: &[&str]) {
        let output = run_cli(test_name, args);
        assert!(
            output.status.success(),
            "expected success for {args:?}; stderr was: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        for snippet in stdout_snippets {
            assert!(
                stdout.contains(snippet),
                "stdout for {args:?} did not contain {snippet:?}: {stdout}"
            );
        }
    }

    fn assert_failure(test_name: &str, args: &[&str], stderr_snippets: &[&str]) {
        let output = run_cli(test_name, args);
        assert!(
            !output.status.success(),
            "expected failure for {args:?}; stdout was: {}",
            String::from_utf8_lossy(&output.stdout)
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        for snippet in stderr_snippets {
            assert!(
                stderr.contains(snippet),
                "stderr for {args:?} did not contain {snippet:?}: {stderr}"
            );
        }
    }

    #[test]
    fn no_args_prints_top_level_help() {
        assert_success("no_args_help", &[], &[
            "uffs - Ultra Fast File Search",
            "USAGE:",
        ]);
    }

    #[test]
    fn help_flag_prints_examples() {
        assert_success("help_flag", &["--help"], &[
            // Search-first grammar note + an example + a `--command`.
            "Search-first",
            "uffs '*.txt'",
            "--update",
        ]);
    }

    #[test]
    fn version_flag_prints_binary_version() {
        assert_success("version_flag", &["--version"], &[
            "uffs",
            env!("CARGO_PKG_VERSION"),
        ]);
    }

    // ── The two reserved single-dash exceptions (`-h` / `-V`) ────────
    //
    // Per docs/architecture/cli-grammar.md §3.4, `-h` and `-V` are the
    // ONLY single-dash tokens that are not search patterns. Pin both that
    // they work AND that no other single-dash token is treated as help.

    #[test]
    fn short_help_flag_prints_help() {
        assert_success("short_help", &["-h"], &[
            "uffs - Ultra Fast File Search",
            "USAGE:",
        ]);
    }

    #[test]
    fn short_version_flag_prints_binary_version() {
        assert_success("short_version", &["-V"], &[
            "uffs",
            env!("CARGO_PKG_VERSION"),
        ]);
    }

    #[test]
    fn version_short_form_carries_the_build_sha() {
        // `uffs[.exe] <semver> (<sha>)` — the parenthesised build sha ties a
        // running binary to its exact source (shared shape across all
        // binaries).
        assert_success("version_sha", &["--version"], &["uffs", "("]);
    }

    #[test]
    fn version_verbose_prints_the_build_fingerprint() {
        // `--version -v` → the multi-line build fingerprint for bug reports.
        assert_success("version_verbose", &["--version", "-v"], &[
            "commit:", "rustc:", "target:", "profile:",
        ]);
    }

    #[test]
    fn other_single_dash_token_is_a_pattern_not_help() {
        // `-x` must NOT print help/version — it is a search pattern, so with
        // no daemon it fails trying to connect (it never exits 0 with help).
        assert_failure("single_dash_pattern", &["-x"], &["daemon"]);
    }

    // ── Command-typo hint (cli-grammar.md §6) ────────────────────────
    //
    // A `--`-flag the shared parser rejects that is a near-miss of a
    // management command surfaces a "did you mean" hint up front — without
    // the daemon (the CLI suggests over its own command set). Deterministic.

    #[test]
    fn flag_typo_near_a_command_suggests_it() {
        assert_failure("flag_typo_command", &["--updat"], &[
            "not a known search flag",
            "Did you mean the command",
            "uffs --update",
        ]);
    }

    #[test]
    fn unrelated_unknown_flag_gets_no_command_hint() {
        // `--bogus` is not near any command, so it must NOT get a command
        // hint — it falls through to search (here: a daemon-connect error,
        // since there is no daemon in the test environment).
        let output = run_cli("unrelated_unknown_flag", &["--bogus"]);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !combined.contains("Did you mean the command"),
            "`--bogus` must not produce a command suggestion: {combined}"
        );
    }

    // ── `--update` action surface (cli-grammar.md §5) ────────────────
    //
    // Pin the full action set — incl. `recover` — so it can't silently
    // drift from the design doc. Deterministic: no filesystem/network.

    #[test]
    fn update_help_lists_every_action_including_recover() {
        assert_success("update_help", &["--update", "--help"], &[
            "snapshot", "acquire", "apply", "doctor", "recover",
        ]);
    }

    #[test]
    fn update_rejects_unknown_action_and_lists_recover() {
        // An unknown action errors with the accepted set, which must now
        // include `recover` (the foreground self-heal action).
        assert_failure("update_bogus_action", &["--update", "bogus"], &[
            "unknown `--update` action",
            "recover",
        ]);
    }

    // ── Validation tests ────────────────────────────────────────────
    //
    // With the thin-client approach, search-flag validation happens on
    // the daemon side.  Tests that validated clap error messages for
    // search flags (--min-size, --limit, --tz-offset, --drive conflicts)
    // are now daemon-level concerns tested in uffs-client/uffs-daemon.
    //
    // Stats subcommand validation is still client-side.

    #[test]
    fn stats_refuses_the_removed_path_and_top() {
        assert_failure("stats_path", &["--stats", "saved.parquet"], &[
            "does not take `saved.parquet`",
        ]);
        assert_failure("stats_top", &["--stats", "--top", "5"], &[
            "does not take `--top`",
        ]);
    }

    /// `uffs -- <PATTERN>`: after `--` a token that looks like a flag is the
    /// pattern. Without a daemon the search fails at connect, but never with
    /// the parser's unknown-flag / did-you-mean error.
    #[test]
    fn double_dash_pattern_is_not_parsed_as_a_flag() {
        let output = run_cli("double_dash", &["--", "--updat"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("not a known search flag") && !stderr.contains("Unknown flag"),
            "a pattern after -- must not be read as a flag: {stderr}"
        );
    }

    #[test]
    fn daemon_start_refuses_unknown_flags() {
        assert_failure(
            "daemon_start_unknown",
            &["--daemon", "start", "--idle-timeout", "0"],
            &["does not take `--idle-timeout`"],
        );
    }

    // ── --name-only tests ───────────────────────────────────────────
    //
    // These validations now happen daemon-side via search_cli.
    // We keep smoke tests that don't require a running daemon.

    #[test]
    fn name_only_accepts_plain_literal() {
        // Should not error with "--name-only cannot be used with path
        // patterns". The command will fail because no daemon is running,
        // but the validation error should not appear.
        let output = run_cli("name_only_plain", &[
            "hallo",
            "--name-only",
            "--mft-file",
            "nonexistent.bin",
        ]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("--name-only cannot be used with path patterns"),
            "plain literal should be accepted with --name-only"
        );
    }

    #[test]
    fn name_only_accepts_glob_pattern() {
        let output = run_cli("name_only_glob", &[
            "*.txt",
            "--name-only",
            "--mft-file",
            "nonexistent.bin",
        ]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("--name-only cannot be used with path patterns"),
            "glob should be accepted with --name-only"
        );
    }

    #[test]
    fn name_only_accepts_regex_with_backslash_escapes() {
        let output = run_cli("name_only_regex", &[
            r">.*\.(jpg|png)",
            "--name-only",
            "--mft-file",
            "nonexistent.bin",
        ]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("--name-only cannot be used with path patterns"),
            "regex patterns (starting with >) should be accepted with --name-only"
        );
    }
}
