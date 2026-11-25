// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tracing-subscriber bootstrap for the daemon.
//!
//! Extracted from `lib.rs` so the daemon's startup graph + `spawn_*`
//! cluster can stay focused on lifecycle wiring without the log-file
//! parent-directory normalisation noise.  No collision risk with the
//! `tracing` crate because the module is named `log_init`.

use std::path::PathBuf;

/// Default log file location: `<native-log-dir>/uffsd.log`.
///
/// The directory is the shared per-platform native log location resolved
/// by [`uffs_security::log_dir::log_dir`] (macOS `~/Library/Logs/uffs`,
/// Windows `%LOCALAPPDATA%\uffs\logs`, Linux `$XDG_STATE_HOME/uffs/logs`),
/// overridable via `UFFS_LOG_DIR`.  When that resolution falls back to a
/// relative `logs` (no home dir), the parent-dir normalisation in
/// [`init_tracing`] still produces a usable `logs/uffsd.log`.
#[must_use]
pub(crate) fn default_log_file() -> PathBuf {
    uffs_security::log_dir::log_dir().join("uffsd.log")
}

/// Daily log files kept before the oldest is pruned.
///
/// A week of history is enough to investigate an incident reported the
/// morning after a weekend, and short enough that the logs cannot quietly
/// consume a disk.
const KEPT_LOG_FILES: usize = 7;

/// What [`init_tracing`] set up: the file writer's guard, and what went
/// differently from what was asked.
///
/// Tracing is not running yet when these things happen, so they cannot be
/// logged; the caller writes [`Self::warnings`] to stderr (the only honest
/// channel at that moment) and keeps [`Self::guard`] until it exits.
#[must_use]
pub struct TracingSetup {
    /// The non-blocking file writer's guard, when logging goes to a file.
    /// It **must** be held until the daemon exits: dropping it flushes the
    /// writer.
    pub guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// Each way the setup fell short of the request (a log filter that did
    /// not parse, a log directory that could not be made or opened, a
    /// subscriber that was already installed), one line each, for stderr.
    pub warnings: Vec<String>,
}

/// Initialise tracing for the daemon process.
///
/// * `log_file = Some(path)` — write to that file (append mode). A path of
///   `"-"` or empty string uses `default_log_file`.
/// * `log_file = None` **and** the effective log level is `debug` or `trace` —
///   automatically write to `default_log_file` so that diagnostic output is
///   never lost to `/dev/null`.
/// * `log_file = None` with a higher level — write to stdout.
///
/// Nothing here stops the daemon: a log filter that does not parse falls
/// back to `info`, and a log file that cannot be opened falls back to stdout
/// logging (WI-6.2). Each fallback is reported in
/// [`TracingSetup::warnings`].
pub fn init_tracing(log_spec: &str, log_file: Option<&std::path::Path>) -> TracingSetup {
    let mut warnings = Vec::new();
    let filter = tracing_subscriber::EnvFilter::try_new(log_spec).unwrap_or_else(|err| {
        warnings.push(format!(
            "uffsd: log filter `{log_spec}` is not valid ({err}); logging at info"
        ));
        tracing_subscriber::EnvFilter::new("info")
    });

    // Decide whether to use a file writer.
    let is_verbose = {
        let lower = log_spec.to_ascii_lowercase();
        lower.contains("debug") || lower.contains("trace")
    };
    let effective_file: Option<PathBuf> = match log_file {
        Some(path) => {
            let resolved = if path.as_os_str().is_empty() || path == std::path::Path::new("-") {
                default_log_file()
            } else {
                path.to_path_buf()
            };
            Some(resolved)
        }
        None if is_verbose => Some(default_log_file()),
        None => None,
    };

    let stdout = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    let (guard, installed) = match effective_file.map(|path| file_writer(&path)) {
        Some(Ok((non_blocking, guard))) => (
            Some(guard),
            stdout.with_ansi(false).with_writer(non_blocking).try_init(),
        ),
        Some(Err(reason)) => {
            warnings.push(format!("uffsd: {reason}; falling back to stdout logging"));
            (None, stdout.try_init())
        }
        None => (None, stdout.try_init()),
    };
    // `try_init` fails only when a subscriber is already installed (the
    // embedded `uffs --daemon run` path): that one is kept.
    if let Err(err) = installed {
        warnings.push(format!(
            "uffsd: a tracing subscriber was already installed; keeping it ({err})"
        ));
    }
    TracingSetup { guard, warnings }
}

/// Open the daily-rotated log file for `resolved`: its directory is created
/// first, and an error says which step failed.
///
/// # Errors
///
/// The reason, as a phrase: the log directory could not be created, or the
/// rolling log could not be opened in it.
fn file_writer(
    resolved: &std::path::Path,
) -> Result<
    (
        tracing_appender::non_blocking::NonBlocking,
        tracing_appender::non_blocking::WorkerGuard,
    ),
    String,
> {
    // Compute a *safe* parent directory.
    //
    // `PathBuf::from("uffsd.log").parent()` returns `Some(Path::new(""))`,
    // not `None`, and `create_dir_all("")` errors on Windows ("The system
    // cannot find the path specified"); the appender constructor then
    // panicked and killed the detached daemon before it bound IPC.  Coerce
    // both `None` and `Some("")` to the current directory so relative
    // `--log-file` paths work the same everywhere.
    let parent_dir = match resolved.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => std::path::Path::new("."),
    };
    // WI-6.2: a log dir that cannot be created must not reach the appender
    // constructor, which PANICS when the parent directory is absent.
    std::fs::create_dir_all(parent_dir)
        .map_err(|err| format!("could not create log dir {} ({err})", parent_dir.display()))?;

    // Daily rotation with bounded retention — NOT `rolling::never`.
    //
    // `never` is append-forever with no size cap and no retention. One
    // production `uffsd.log` reached **305 MB** that way. The daemon runs
    // for weeks and nothing else ever truncates.
    //
    // The caller's directory and file stem are preserved, so a
    // `--log-file /var/log/uffsd.log` becomes `/var/log/uffsd.<date>.log`.
    let stem = resolved
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("uffsd");
    let extension = resolved
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("log");
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(stem)
        .filename_suffix(extension)
        .max_log_files(KEPT_LOG_FILES)
        .build(parent_dir)
        .map_err(|err| {
            format!(
                "could not open a rolling log in {} ({err})",
                parent_dir.display()
            )
        })?;
    Ok(tracing_appender::non_blocking(file_appender))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WI-6.2: when the log dir cannot be created (its parent is a regular
    /// file), `init_tracing` must degrade gracefully — return without
    /// panicking so daemon startup continues — rather than aborting — and
    /// report the fallback for the caller to print.
    #[test]
    fn init_tracing_degrades_when_log_dir_uncreatable() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A regular file where a directory component is expected, so
        // create_dir_all(parent) fails.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").expect("write blocker file");
        let bogus_log = blocker.join("sub").join("uffsd.log");

        // Must not panic. With an uncreatable log dir, init degrades to
        // stdout logging and returns None (no file guard) — proving the
        // daemon would continue startup rather than crash in the rolling
        // appender constructor.
        let setup = init_tracing("info", Some(&bogus_log));
        assert!(
            setup.guard.is_none(),
            "uncreatable log dir must degrade to stdout (None), not a file guard"
        );
        assert!(
            setup
                .warnings
                .iter()
                .any(|note| note.contains("could not create log dir")),
            "the fallback is reported, not silent: {:?}",
            setup.warnings
        );
    }
}
