// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! CLI argument transforms for the `search` subcommand.
//!
//! These helpers run between raw `std::env::args()` and the daemon's
//! `search_cli` RPC.  They are pure (string-in, string-out) so the
//! `main` dispatcher can stay focused on flow control while each
//! transform owns its own contract and unit tests:
//!
//! * `extract_spawn_args` — pick the subset of flags that need to travel to a
//!   freshly spawned daemon (e.g. `--data-dir`, `--mft-file`, `--no-cache`,
//!   `--drive`, `--log-level`).
//! * `resolve_out_path` — rewrite `--out` / `--out=<path>` so relative paths
//!   resolve against the CLI's `current_dir`, not the daemon's.
//! * `inject_no_output_for_null_stdout` — append `--no-output` when stdout is
//!   the platform's null device, skipping the IPC row transfer entirely.
//! * `maybe_inject_no_output` — pure decision logic backing the above so it can
//!   be unit-tested without piping the test harness to NUL.
//! * `resolve_to_absolute` — shared path-resolution primitive used by
//!   `resolve_out_path`.
//! * `with_default_format` — name the default `--format` explicitly, so the
//!   daemon renders what the CLI prints.
//!
//! Every transform reads and writes the **options** only: the tokens before
//! an `--` (`uffs -- --out` searches for `--out`; it does not write a file).
//! Operands after `--` pass through untouched.

use uffs_client::protocol::cli_args::{END_OF_OPTIONS, split_end_of_options};

/// The options of a search argument vector (the tokens before `--`).
pub(crate) fn options(args: &[String]) -> &[String] {
    split_end_of_options(args).0
}

/// Insert `tokens` at the end of the options of `args`, before an `--` if
/// there is one, so they are read as options and not as the pattern.
fn insert_options(args: &mut Vec<String>, tokens: &[&str]) {
    let at = args
        .iter()
        .position(|arg| arg == END_OF_OPTIONS)
        .unwrap_or(args.len());
    args.splice(at..at, tokens.iter().map(|token| (*token).to_owned()));
}

/// Name the default `--format` explicitly when the user gave none.
///
/// The CLI decides the default (`table` on an interactive terminal, `csv`
/// when piped, redirected or written with `--out`, see
/// [`super::dispatch::default_format`]) and sends it to the daemon, so the
/// daemon's renderer and the CLI's printer always work from the same format.
/// Without it the daemon assumed CSV and pre-rendered a CSV blob, which the
/// CLI printed verbatim even on a terminal.
pub(crate) fn with_default_format(mut args: Vec<String>, terminal: bool) -> Vec<String> {
    let opts = options(&args);
    let named = opts.iter().any(|arg| {
        let flag = arg.split('=').next().unwrap_or(arg.as_str());
        flag == "--format" || flag == "-f"
    });
    if named {
        return args;
    }
    let to_console = !opts
        .iter()
        .any(|arg| arg == "--out" || arg.starts_with("--out="));
    let format = super::dispatch::default_format(to_console && terminal);
    insert_options(&mut args, &["--format", format]);
    args
}

/// Extract daemon-spawn-relevant flags from raw CLI args, written the way
/// `uffsd` reads them.
///
/// The daemon auto-start needs `--data-dir`, `--mft-file`, `--no-cache`,
/// the drives, and the log settings. Everything else is irrelevant for
/// spawn. A search takes lists (`--mft-file C.bin,D.bin`, `--drives C,D`,
/// `--drive C:`) where `uffsd` takes one value per flag (`--mft-file C.bin
/// --mft-file D.bin`, `--drive C --drive D`), so each list is split here;
/// a drive letter that does not parse is left for the search parser to
/// report.
pub(crate) fn extract_spawn_args(args: &[String]) -> Vec<std::ffi::OsString> {
    let mut spawn: Vec<std::ffi::OsString> = Vec::new();
    let mut iter = options(args).iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(flag, val)| (flag, Some(val)));
        let mut value = || inline.or_else(|| iter.next().map(String::as_str));
        match flag {
            "--mft-file" => {
                for part in value().into_iter().flat_map(|list| list.split(',')) {
                    let path = part.trim();
                    if !path.is_empty() {
                        spawn.extend(["--mft-file", path].map(std::ffi::OsString::from));
                    }
                }
            }
            "--drive" | "-d" | "--drives" => {
                for part in value().into_iter().flat_map(|list| list.split(',')) {
                    let trimmed = part.trim();
                    let bare = trimmed.strip_suffix(':').unwrap_or(trimmed);
                    if let Ok(letter) = bare.parse::<uffs_mft::platform::DriveLetter>() {
                        spawn.push("--drive".into());
                        spawn.push(letter.to_string().into());
                    }
                }
            }
            "--data-dir" | "--log-level" | "--log-file" => {
                if let Some(val) = value() {
                    spawn.extend([flag, val].map(std::ffi::OsString::from));
                }
            }
            "--no-cache" => spawn.push(std::ffi::OsString::from(flag)),
            _ => {}
        }
    }

    // Forward log env vars.
    if let Ok(ll) = std::env::var("UFFS_LOG") {
        spawn.push(std::ffi::OsString::from("--log-level"));
        spawn.push(std::ffi::OsString::from(ll));
    }
    if let Ok(lf) = std::env::var("UFFS_LOG_FILE") {
        spawn.push(std::ffi::OsString::from("--log-file"));
        spawn.push(std::ffi::OsString::from(lf));
    }

    spawn
}

/// Resolve a relative `--out` path to absolute using the CLI's working
/// directory.
///
/// The daemon runs in a different working directory, so relative paths in
/// `--out` or `--out=<path>` would resolve against the wrong directory if
/// passed through as-is.
pub(crate) fn resolve_out_path(args: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::with_capacity(args.len());
    let (opts, operands) = split_end_of_options(args);
    let mut iter = opts.iter();
    while let Some(arg) = iter.next() {
        if let Some(val) = arg.strip_prefix("--out=") {
            // `--out=path` form — resolve inline value.
            let resolved = resolve_to_absolute(val);
            result.push(format!("--out={resolved}"));
        } else if arg == "--out" {
            result.push(arg.clone());
            // Next token is the path value.
            if let Some(val) = iter.next() {
                result.push(resolve_to_absolute(val));
            }
        } else {
            result.push(arg.clone());
        }
    }
    if opts.len() < args.len() {
        result.push(END_OF_OPTIONS.to_owned());
        result.extend_from_slice(operands);
    }
    result
}

/// Append `--no-output` to `args` when stdout is redirected to the
/// null device, unless a disqualifying flag is already set.
///
/// Thin wrapper around [`maybe_inject_no_output`] that probes the real
/// stdout via [`uffs_client::stdout_kind::StdoutKind::detect`].  The
/// decision logic itself is in `maybe_inject_no_output` so it can be
/// unit-tested without fighting the test harness's stdout wiring.
pub(crate) fn inject_no_output_for_null_stdout(args: Vec<String>) -> Vec<String> {
    let stdout_is_null = uffs_client::stdout_kind::StdoutKind::detect().is_null();
    maybe_inject_no_output(args, stdout_is_null)
}

/// Pure decision logic for the NUL fast-path injection.
///
/// Returns `args` unchanged when `stdout_is_null == false` or when any
/// disqualifying flag is already present:
///
/// - `--no-output` already set: nothing to add.
/// - `--rows`: the user asked to force rows on even for aggregate queries —
///   honour that intent regardless of where stdout goes.
/// - `--out`: stdout is not the result destination; NUL on stdout is a benign
///   quirk, not the output target.
/// - `--agg` / `--facet` / `--stats` / `--histogram` / `--count`: any
///   aggregation flag already controls `include_rows` via its own sugar; adding
///   `--no-output` would be redundant at best.
pub(crate) fn maybe_inject_no_output(mut args: Vec<String>, stdout_is_null: bool) -> Vec<String> {
    if !stdout_is_null {
        return args;
    }
    let is_aggregate_flag = |flag: &str| {
        matches!(
            flag,
            "--agg" | "--facet" | "--stats" | "--histogram" | "--count"
        )
    };
    let disqualified = options(&args).iter().any(|raw| {
        let flag = raw.split('=').next().unwrap_or(raw.as_str());
        flag == "--no-output" || flag == "--rows" || flag == "--out" || is_aggregate_flag(flag)
    });
    if disqualified {
        return args;
    }
    insert_options(&mut args, &["--no-output"]);
    args
}

/// Resolve a potentially relative path to absolute using `current_dir`.
fn resolve_to_absolute(path_str: &str) -> String {
    let path = std::path::Path::new(path_str);
    if path.is_absolute() {
        return path_str.to_owned();
    }
    std::env::current_dir()
        .unwrap_or_default()
        .join(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        extract_spawn_args, maybe_inject_no_output, resolve_out_path, resolve_to_absolute,
        with_default_format,
    };

    /// The spawn arguments of a search, as strings (log env vars aside).
    fn spawn_of(args: &[&str]) -> Vec<String> {
        extract_spawn_args(&owned(args))
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .take_while(|arg| arg != "--log-level" && arg != "--log-file")
            .collect()
    }

    /// A search's list flags reach `uffsd` one value per flag, the way it
    /// reads them.
    #[test]
    fn spawn_args_split_lists_for_uffsd() {
        assert_eq!(
            spawn_of(&["*", "--mft-file", "C.bin, D.bin", "--drives", "E:,f"]),
            [
                "--mft-file",
                "C.bin",
                "--mft-file",
                "D.bin",
                "--drive",
                "E",
                "--drive",
                "F"
            ]
        );
        assert_eq!(
            spawn_of(&["*", "--data-dir=/data", "-d", "C:", "--no-cache"]),
            ["--data-dir", "/data", "--drive", "C", "--no-cache"]
        );
    }

    /// Owned argument vector from string slices.
    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().copied().map(String::from).collect()
    }

    /// Without `--format`, the default is named for the daemon: `table` on
    /// a terminal, `csv` when piped or written with `--out`; an explicit
    /// format is left alone, and the default goes before an `--`.
    #[test]
    fn with_default_format_names_the_default() {
        assert_eq!(with_default_format(owned(&["*.rs"]), true), [
            "*.rs", "--format", "table"
        ]);
        assert_eq!(with_default_format(owned(&["*.rs"]), false), [
            "*.rs", "--format", "csv"
        ]);
        assert_eq!(
            with_default_format(owned(&["*.rs", "--out", "x.csv"]), true),
            ["*.rs", "--out", "x.csv", "--format", "csv"]
        );
        assert_eq!(with_default_format(owned(&["*.rs", "-f", "json"]), true), [
            "*.rs", "-f", "json"
        ]);
        assert_eq!(with_default_format(owned(&["--", "--format"]), false), [
            "--format", "csv", "--", "--format"
        ]);
    }

    /// Operands after `--` are never rewritten or read as flags.
    #[test]
    fn transforms_leave_operands_after_double_dash_alone() {
        let out = resolve_out_path(&owned(&["--", "--out", "rel.csv"]));
        assert_eq!(out, ["--", "--out", "rel.csv"]);
        let injected = maybe_inject_no_output(owned(&["--", "--rows"]), true);
        assert_eq!(injected, ["--no-output", "--", "--rows"]);
        let spawn = extract_spawn_args(&owned(&["--", "--no-cache"]));
        assert!(
            !spawn.iter().any(|arg| arg == "--no-cache"),
            "an operand is not a spawn flag: {spawn:?}"
        );
    }

    // ── maybe_inject_no_output (Phase 3.1 NUL fast path) ─────────

    /// Helper: run [`maybe_inject_no_output`] with `stdout_is_null = true`
    /// and return the resulting args.
    fn inject_null(args: &[&str]) -> Vec<String> {
        let owned: Vec<String> = args.iter().copied().map(String::from).collect();
        maybe_inject_no_output(owned, true)
    }

    /// Baseline: a plain search with NUL stdout gets `--no-output`
    /// appended.  This is the hot path we want for benchmarks and
    /// `uffs *.dll > NUL`-style invocations.
    #[test]
    fn maybe_inject_no_output_appends_on_null_stdout() {
        let out = inject_null(&["*.dll", "--drive", "D"]);
        assert_eq!(out, ["*.dll", "--drive", "D", "--no-output"]);
    }

    /// Non-null stdout (terminal, pipe, file) must leave args alone,
    /// otherwise the user would see no output on their terminal.
    #[test]
    fn maybe_inject_no_output_unchanged_on_non_null_stdout() {
        let owned: Vec<String> = ["*.dll", "--drive", "D"]
            .into_iter()
            .map(String::from)
            .collect();
        let out = maybe_inject_no_output(owned.clone(), false);
        assert_eq!(out, owned);
    }

    /// Explicit `--no-output` already present: do not double up.
    #[test]
    fn maybe_inject_no_output_skips_when_already_present() {
        let out = inject_null(&["*.dll", "--no-output"]);
        // Exactly one occurrence — no double-injection.
        let count = out
            .iter()
            .filter(|arg| arg.as_str() == "--no-output")
            .count();
        assert_eq!(count, 1, "auto-injection must not duplicate user's flag");
    }

    /// `--rows` is the explicit "force rows on" override that wins
    /// over the NUL fast-path: leave `args` untouched even though
    /// stdout is NUL.
    ///
    /// Covers the user who wants `uffs *.rs --rows > NUL` for timing
    /// the full round-trip including IPC transport.
    #[test]
    fn maybe_inject_no_output_respects_rows_flag() {
        let out = inject_null(&["*.rs", "--rows"]);
        assert!(
            !out.iter().any(|arg| arg == "--no-output"),
            "--rows must prevent --no-output auto-injection, got: {out:?}"
        );
    }

    /// `--out file` routes results daemon-direct to disk — NUL on
    /// stdout is a benign quirk, not the output destination.
    #[test]
    fn maybe_inject_no_output_respects_out_flag() {
        let out = inject_null(&["*.rs", "--out", "results.csv"]);
        assert!(
            !out.iter().any(|arg| arg == "--no-output"),
            "--out must prevent --no-output auto-injection, got: {out:?}"
        );
    }

    /// Aggregation flags already control `include_rows` via their
    /// own sugar; auto-injection would be redundant.  Covers `--count`,
    /// `--agg`, `--facet`, `--stats`, `--histogram`.
    #[test]
    fn maybe_inject_no_output_respects_aggregation_flags() {
        for flag in ["--count", "--agg", "--facet", "--stats", "--histogram"] {
            // `--agg` and friends take a value; the parse-time check
            // keys on the flag name only, so a bare `--agg count` suffix
            // still trips the disqualifier exactly the same way as the
            // `--agg=count` form.
            let out_with_value = inject_null(&["*", flag, "stub"]);
            assert!(
                !out_with_value.iter().any(|arg| arg == "--no-output"),
                "{flag} must prevent --no-output auto-injection, got: {out_with_value:?}"
            );
            let out_equals = inject_null(&["*", &format!("{flag}=stub")]);
            assert!(
                !out_equals.iter().any(|arg| arg == "--no-output"),
                "{flag}=… must prevent --no-output auto-injection, got: {out_equals:?}"
            );
        }
    }

    /// `--out=file.csv` (equals form) must also disqualify the
    /// auto-injection.  The parser keys on the flag name before `=`.
    #[test]
    fn maybe_inject_no_output_respects_out_equals_form() {
        let out = inject_null(&["*", "--out=results.csv"]);
        assert!(
            !out.iter().any(|arg| arg == "--no-output"),
            "--out=<path> must prevent --no-output auto-injection, got: {out:?}"
        );
    }

    /// Absolute paths must round-trip unchanged so the daemon receives
    /// exactly what the user typed (no double-resolution against
    /// `current_dir`).
    #[test]
    fn resolve_to_absolute_keeps_absolute_paths() {
        let abs = if cfg!(windows) {
            r"C:\tmp\out.csv"
        } else {
            "/tmp/out.csv"
        };
        assert_eq!(resolve_to_absolute(abs), abs);
    }

    /// Relative paths must be anchored to `current_dir`.  We don't
    /// assert the exact prefix (it depends on where the test
    /// harness was invoked), only that the result is now absolute.
    #[test]
    fn resolve_to_absolute_anchors_relative_paths() {
        let resolved = resolve_to_absolute("relative.csv");
        let path = std::path::Path::new(&resolved);
        assert!(
            path.is_absolute(),
            "relative path must be resolved to absolute, got: {resolved}"
        );
        assert!(
            resolved.ends_with("relative.csv"),
            "resolved path must keep the original filename, got: {resolved}"
        );
    }
}
