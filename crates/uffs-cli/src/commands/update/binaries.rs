// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Known UFFS binaries, per-root vicinity enumeration, and on-disk
//! version probing (Phase A.2 + A.4 of the self-update design).
//!
//! A root is updated as a *coherent set*: every UFFS binary present in
//! it is a target, not only the one that happened to be running. We
//! never assume a binary is present — we enumerate what is actually on
//! disk and record each one's `--version`.

use std::path::Path;
use std::process::Command;

use super::model::BinaryInfo;

/// Logical stems of every UFFS binary the updater knows about — i.e. the
/// engine binaries this repo builds and publishes as release assets, so
/// each can be acquired + swapped.
///
/// **This is the single source of truth for the UFFS core binary set.**
/// Every flow honours it: detection, self-update completeness, and tooling
/// (the `just` deploy recipes query it via `uffs --update bins` rather than
/// keeping their own list). Do not hardcode this set anywhere else — add a
/// stem here and all flows pick it up.
///
/// The platform `.exe` suffix is added by
/// [`exe_file_name`].
///
/// `uffs-tui` is deliberately **excluded**: it ships from the separate
/// `uffs-products` / `uffs-demo` repo with its own versioning and is not a
/// release asset here, so the engine updater must not chase it.
///
/// `uffs-broker` is **Windows-only**: the release publishes no macOS/Linux
/// broker asset, so off Windows it must NOT be a self-update target — else
/// `acquire` would try to download a non-existent `uffs-broker-<plat>` asset
/// (e.g. a stale broker stub left in `~/bin`). It is therefore included only
/// when compiled for Windows.
///
/// `uffs-watchdog` (the supervisor `resident on` arms) is Windows-only for the
/// same reason: launchd / systemd supervise the daemon elsewhere, and the
/// release ships it only in the Windows zips.
#[cfg(windows)]
pub(crate) const KNOWN_BINARIES: [&str; 7] = [
    "uffs",          // CLI
    "uffsd",         // daemon
    "uffsmcp",       // MCP server
    "uffs-broker",   // elevated handle broker (Windows service)
    "uffs-update",   // the self-update helper itself
    "uffs-mft",      // MFT diagnostics binary (optional)
    "uffs-watchdog", // resident-services supervisor (Windows)
];

/// Non-Windows: the same set minus the Windows-only broker.
#[cfg(not(windows))]
pub(crate) const KNOWN_BINARIES: [&str; 5] = [
    "uffs",        // CLI
    "uffsd",       // daemon
    "uffsmcp",     // MCP server
    "uffs-update", // the self-update helper itself
    "uffs-mft",    // MFT diagnostics binary (optional)
];

/// Append the platform executable suffix to a binary stem
/// (`uffsd` → `uffsd.exe` on Windows, `uffsd` elsewhere).
pub(crate) fn exe_file_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// Print the canonical core binary stems, one per line (platform-aware:
/// `uffs-broker` only on Windows). The machine-readable accessor behind
/// `uffs --update bins`, so shell tooling (the `just` deploy recipes) reads
/// the set from here instead of keeping a hardcoded copy that can drift.
#[expect(clippy::print_stdout, reason = "machine-readable list for scripts")]
pub(crate) fn print_core_stems() {
    for stem in KNOWN_BINARIES {
        println!("{stem}");
    }
}

/// Enumerate the UFFS binaries actually present in `dir`, probing each
/// one's version. Binaries that are not present are simply absent from
/// the result (honor-what-is-installed).
pub(crate) fn enumerate(dir: &Path) -> Vec<BinaryInfo> {
    KNOWN_BINARIES
        .iter()
        .filter_map(|stem| {
            let path = dir.join(exe_file_name(stem));
            path.is_file().then(|| BinaryInfo {
                name: (*stem).to_owned(),
                version: probe_version(&path),
            })
        })
        .collect()
}

/// Run `<path> --version` and parse a semantic-version token from its
/// output. Returns `None` if the binary cannot be launched or no
/// version token is found.
pub(crate) fn probe_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    // AUDIT-OK(bytes): ASCII version-token extraction from a probed binary's
    // output. A lossy decode cannot fabricate a `MAJOR.MINOR.PATCH` token
    // (U+FFFD is not a digit), only fail to yield one -> `None`.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        // Some tools print `--version` to stderr; fall back to it.
        // AUDIT-OK(bytes): same ASCII-token argument as stdout above.
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    parse_version(&text)
}

/// Extract the first `MAJOR.MINOR.PATCH` token from arbitrary
/// `--version` output (e.g. `"uffs 0.6.2"` → `"0.6.2"`).
///
/// Kept pure (no I/O) so it is unit-testable without launching a
/// process.
pub(crate) fn parse_version(text: &str) -> Option<String> {
    text.split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')' || ch == ',')
        .find_map(|token| {
            let trimmed = token.trim_start_matches('v');
            is_dotted_triple(trimmed).then(|| trimmed.to_owned())
        })
}

/// Return `true` when `token` looks like `N.N.N` where each `N` is a
/// non-empty run of ASCII digits.
fn is_dotted_triple(token: &str) -> bool {
    let mut parts = token.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch]
        .iter()
        .all(|seg| !seg.is_empty() && seg.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{KNOWN_BINARIES, parse_version};

    #[test]
    fn parses_plain_semver() {
        assert_eq!(parse_version("0.6.2").as_deref(), Some("0.6.2"));
    }

    #[test]
    fn parses_named_version_line() {
        assert_eq!(parse_version("uffs 0.6.2").as_deref(), Some("0.6.2"));
        assert_eq!(parse_version("uffsd 0.6.10\n").as_deref(), Some("0.6.10"));
        // The broker prints a hyphenated name; the hyphen must not be mistaken
        // for part of the version token (else its version probes back as `?`).
        assert_eq!(parse_version("uffs-broker 0.6.9").as_deref(), Some("0.6.9"));
    }

    #[test]
    fn parses_v_prefixed_and_parenthesised() {
        assert_eq!(
            parse_version("uffs v0.6.2 (abc123)").as_deref(),
            Some("0.6.2")
        );
    }

    #[test]
    fn rejects_non_triples() {
        assert_eq!(parse_version("uffs"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("a.b.c"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn known_set_contains_engine_binaries_and_the_helper() {
        // These are self-update targets on every platform.
        for stem in ["uffs", "uffsd", "uffsmcp", "uffs-update", "uffs-mft"] {
            assert!(
                KNOWN_BINARIES.contains(&stem),
                "missing engine binary {stem}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn known_set_includes_broker_on_windows() {
        assert!(KNOWN_BINARIES.contains(&"uffs-broker"));
    }

    /// The watchdog `resident on` arms ships in every Windows zip, so the
    /// updater acquires and replaces it and uninstall removes it.
    #[cfg(windows)]
    #[test]
    fn known_set_includes_the_watchdog_on_windows() {
        assert!(KNOWN_BINARIES.contains(&"uffs-watchdog"));
    }

    /// Off Windows launchd / systemd supervise; no watchdog asset exists.
    #[cfg(not(windows))]
    #[test]
    fn known_set_excludes_the_watchdog_off_windows() {
        assert!(!KNOWN_BINARIES.contains(&"uffs-watchdog"));
    }

    #[cfg(not(windows))]
    #[test]
    fn known_set_excludes_broker_off_windows() {
        // The broker is Windows-only — off Windows it must not be a
        // self-update target (no macOS/Linux broker release asset exists).
        assert!(
            !KNOWN_BINARIES.contains(&"uffs-broker"),
            "uffs-broker is Windows-only and must not be acquired off Windows"
        );
    }

    #[test]
    fn known_set_excludes_the_demo_tui() {
        // `uffs-tui` ships from the separate uffs-demo repo with its own
        // versioning; the engine updater must never try to acquire it.
        assert!(
            !KNOWN_BINARIES.contains(&"uffs-tui"),
            "uffs-tui is not an engine release asset and must not be auto-updated"
        );
    }
}
