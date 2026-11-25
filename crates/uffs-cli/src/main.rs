// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS (Ultra Fast File Search) CLI — thin synchronous client.
//!
//! All heavy lifting (including CLI arg parsing) happens in the daemon.
//! This binary detects subcommands and forwards raw search args via
//! `search_cli` RPC.  Argument transforms specific to the search
//! subcommand live in [`commands::search::args`].
//!
//! # Features
//!
//! Documented per the workspace dependency policy
//! (`docs/architecture/code-quality/dependency_policy.md`, playbook §988).
//!
//! | Feature | Default? | Enables | Adds deps | Binary-size impact | Semver |
//! |---|:---:|---|---|---|---|
//! | `mcp-http-probe` | **no** | Active probing of the MCP HTTP gateway's `/status` endpoint inside `uffs --status` (see [`commands::system_status`]).  Without it, `uffs --status` still reports the configured HTTP bind address but does not actively probe. | None on the crate graph.  Uses `std::net::TcpStream` (libstd) — but on Windows targets that drag in `ws2_32.dll`, adding ~50 ms to cold-start process launch. | Disabling drops the only `std::net` user from the CLI: `ws2_32.dll` is left unlinked.  This is **the** reason the feature is default-off — the CLI is the thin / fast-path binary. | Removing the probe behaviour or its observable output behind `mcp-http-probe` is breaking; adding richer probe output is not. |
//!
//! ## Why `mcp-http-probe` is default-off
//!
//! `uffs-cli` is the thin synchronous fast-path binary.  Every byte
//! and every millisecond of cold-start matter (the CLI typically
//! runs once per shell invocation; the daemon is the long-lived
//! reactor).  `std::net::TcpStream` is the lone reason `ws2_32.dll`
//! would be linked on Windows; gating its use behind a non-default
//! feature lets package builds opt in only when probe data is worth
//! the launch-time hit.
//!
//! # Environment
//!
//! Runtime env vars read by this binary (registry:
//! `docs/architecture/code-quality/build_codegen_policy.md` §5, playbook
//! §1049-1056).  Build-time env vars (`CARGO_CFG_TARGET_OS` /
//! `CARGO_CFG_TARGET_ENV`) are documented in [`build.rs`](../../build.rs).
//!
//! | Env var | Type | Default | Notes |
//! |---|---|---|---|
//! | `CARGO_PKG_VERSION` | `string` | (set by Cargo) | Read via `env!()` for `--version` output + log preludes.  CARGO semver class. |
//! | `RUST_LOG` | `string` | `info` | `tracing-subscriber` filter directive; consulted as a fallback when `UFFS_LOG` is unset.  STANDARD semver class (tracing convention). |
//! | `UFFS_LOG` | `string` | `info` | UFFS-specific log level override (preferred over `RUST_LOG` for UFFS binaries).  INTERNAL semver class. |
//! | `UFFS_LOG_DIR` | `path` | platform default (`%LOCALAPPDATA%\UFFS\logs` / `$XDG_CACHE_HOME/uffs/logs`) | Log directory override for `uffs --daemon start` and `uffs --search`.  Mirrors the `--log-dir` CLI flag.  INTERNAL semver class. |
//! | `UFFS_LOG_FILE` | `path` | (none — auto-generated under `UFFS_LOG_DIR`) | Log-file path override.  Mirrors the `--log-file` CLI flag.  INTERNAL semver class. |

use anyhow::Result;
#[cfg(test)]
use assert_cmd as _;

pub mod args;
mod client_profile;
pub mod commands;
mod dispatch;
mod search_retry;

use commands::search::run_search;

/// Run the CLI and return a result.
fn run() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();

    // Global fast paths — ONLY the `--`/`-` forms (bare `help` / `version`
    // are now ordinary search patterns, not commands).
    match raw_args.get(1).map(String::as_str) {
        None | Some("--help" | "-h") => {
            args::print_help();
            return Ok(());
        }
        Some("--version" | "-V") => {
            let verbose = raw_args
                .iter()
                .skip(2)
                .any(|arg| arg == "--verbose" || arg == "-v");
            args::print_version(verbose);
            return Ok(());
        }
        _ => {}
    }

    // Phase H self-heal: if a prior update crashed mid-flight, finish or
    // roll it back in the background. Costs one `stat` in steady state.
    commands::update::maybe_self_heal();

    let first = raw_args.get(1).map_or("", String::as_str);

    // Bare `--` separator → force a search of everything after it (the escape
    // hatch for a pattern that literally begins with `--`).
    // `uffs -- <PATTERN>`: the `--` stays in the vector so the search
    // parser reads every token after it as the pattern, even `--foo`.
    if first == "--" {
        return run_search(raw_args.get(1..).unwrap_or_default());
    }

    // The first token decides the mode: a known `--command` runs that
    // command; ANYTHING else (bare word, glob, single dash, or a search flag)
    // is a search — so `uffs --update` searches for "update".
    match dispatch::Command::from_token(first) {
        Some(command) => {
            dispatch::dispatch_command(command, raw_args.get(2..).unwrap_or_default())?;
        }
        None => {
            // Default: search — forward ALL args after `uffs` to the daemon.
            run_search(raw_args.get(1..).unwrap_or_default())?;
        }
    }

    Ok(())
}

/// Entry point — synchronous, no runtime.
#[expect(
    clippy::print_stderr,
    reason = "intentional user-facing error output to stderr"
)]
fn main() {
    if let Err(err) = run() {
        // Special-case DaemonNeedsElevation: render a multi-option help
        // message instead of the generic `Error: ... Caused by: ...`
        // chain, so a UAC failure reads like advice and not a crash.
        if let Some(needs) = find_needs_elevation(&err) {
            eprintln!("{}", format_elevation_help(needs));
            std::process::exit(1);
        }

        for (idx, cause) in err.chain().enumerate() {
            if idx == 0 {
                eprintln!("Error: {cause}");
            } else {
                eprintln!("  Caused by: {cause}");
            }
        }
        std::process::exit(1);
    }
}

/// Walk an [`anyhow::Error`] chain looking for
/// [`uffs_client::error::ClientError::DaemonNeedsElevation`].
///
/// Returns the daemon path that would have been spawned, so the
/// formatter can quote it back to the user verbatim.  Returns `None`
/// if no elevation error is present in the chain.
fn find_needs_elevation(err: &anyhow::Error) -> Option<&str> {
    for cause in err.chain() {
        if let Some(uffs_client::error::ClientError::DaemonNeedsElevation { daemon_path }) =
            cause.downcast_ref::<uffs_client::error::ClientError>()
        {
            return Some(daemon_path.as_str());
        }
    }
    None
}

/// Render the "daemon needs admin" help message.
///
/// Lists three independent recovery paths so users can pick whichever
/// fits their workflow — scripted, interactive one-off, or permanent.
fn format_elevation_help(daemon_path: &str) -> String {
    format!(
        "Error: UFFS daemon needs admin privileges to read NTFS Master File Tables.\n\
         \n\
         The daemon is not running, and this shell is not elevated.  To start it, pick one:\n\
         \n  \
         1. Relaunch in an elevated shell (PowerShell/cmd \"Run as administrator\"),\n     \
            then retry the command.\n\
         \n  \
         2. Explicitly request a UAC prompt for this invocation:\n       \
               uffs --daemon start --elevate\n     \
            Or set it as the default for the current session:\n       \
               set UFFS_ELEVATE=1     (cmd)\n       \
               $env:UFFS_ELEVATE = '1'  (PowerShell)\n\
         \n  \
         3. Install the broker service — one-time setup, no future UAC prompts:\n       \
               uffs-broker --install\n\
         \n\
         Daemon binary that would have been spawned:\n  \
           {daemon_path}"
    )
}

#[cfg(test)]
mod tests {
    use super::{find_needs_elevation, format_elevation_help};

    /// The elevation help must name every recovery path the user has,
    /// so a UAC-blocked invocation becomes actionable advice rather
    /// than a dead-end crash.  Locks the contract in place.
    #[test]
    fn elevation_help_lists_all_recovery_paths() {
        let help = format_elevation_help(r"C:\Program Files\uffs\uffsd.exe");
        assert!(help.contains("admin"), "help must mention admin: {help}");
        assert!(
            help.contains("--elevate"),
            "help must document --elevate: {help}"
        );
        assert!(
            help.contains("UFFS_ELEVATE"),
            "help must document the env var: {help}"
        );
        assert!(
            help.contains("uffs-broker --install"),
            "help must document the broker install path: {help}"
        );
        assert!(
            help.contains(r"C:\Program Files\uffs\uffsd.exe"),
            "help must quote the daemon path: {help}"
        );
    }

    /// `find_needs_elevation` must walk through any `.with_context`
    /// layers that the CLI adds on top of the raw `ClientError`.
    #[test]
    fn find_needs_elevation_walks_anyhow_context() {
        let base = anyhow::Error::from(uffs_client::error::ClientError::DaemonNeedsElevation {
            daemon_path: "uffsd-test".to_owned(),
        });
        let wrapped: anyhow::Error = base.context("while connecting");
        assert_eq!(find_needs_elevation(&wrapped), Some("uffsd-test"));
    }

    /// Unrelated errors must not be mistaken for an elevation problem,
    /// so the default `Error: ... / Caused by:` chain is preserved for
    /// everything else.
    #[test]
    fn find_needs_elevation_returns_none_for_other_errors() {
        let other = anyhow::Error::from(uffs_client::error::ClientError::ConnectionFailed(
            "nope".to_owned(),
        ));
        assert!(find_needs_elevation(&other).is_none());
    }
}
