// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The flags of `uffs --daemon start`, `load` and `restart`.
//!
//! Every flag is either understood or refused: a flag these commands do not
//! know is a usage error, never silently ignored, and a flag that may be
//! repeated (`--mft-file`, `--drive`) adds to what came before instead of
//! replacing it.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use uffs_mft::platform::DriveLetter;

use super::{DaemonAction, parse_drive_letter};

/// The data sources a daemon is started or loaded with: raw MFT files, one
/// data directory, live drives.
#[derive(Debug, Default)]
struct DataSources {
    /// `--mft-file` (repeatable; each value may be a comma-separated list).
    mft_file: Vec<PathBuf>,
    /// `--data-dir` (at most once).
    data_dir: Option<PathBuf>,
    /// `--drive` / `-d` / `--drives` (repeatable; comma-separated lists).
    drives: Vec<DriveLetter>,
}

impl DataSources {
    /// Consume `flag` (and its value from `values`) when it names a data
    /// source. Returns `false` for any other flag.
    ///
    /// # Errors
    ///
    /// A missing value, a second `--data-dir`, or a value that is not a
    /// drive letter.
    fn take<'a>(
        &mut self,
        command: &str,
        flag: &str,
        values: &mut impl Iterator<Item = &'a String>,
    ) -> Result<bool> {
        match flag {
            "--mft-file" => {
                let list = value(command, flag, values)?;
                self.mft_file.extend(
                    list.split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(PathBuf::from),
                );
            }
            "--data-dir" => {
                let dir = value(command, flag, values)?;
                if self.data_dir.replace(PathBuf::from(dir)).is_some() {
                    bail!("`uffs --daemon {command}` takes one --data-dir, not several");
                }
            }
            "--drive" | "-d" | "--drives" => {
                for part in value(command, flag, values)?.split(',') {
                    self.drives.push(parse_drive_letter(part)?);
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// The value after `flag`.
///
/// # Errors
///
/// When `flag` is the last argument.
fn value<'a>(
    command: &str,
    flag: &str,
    values: &mut impl Iterator<Item = &'a String>,
) -> Result<&'a str> {
    values
        .next()
        .map(String::as_str)
        .with_context(|| format!("`uffs --daemon {command} {flag}` needs a value"))
}

/// The usage error for a flag `command` does not take.
fn unknown(command: &str, flag: &str) -> anyhow::Error {
    anyhow::anyhow!("`uffs --daemon {command}` does not take `{flag}` (see `uffs --daemon --help`)")
}

/// Parse `uffs --daemon start [flags...]`.
///
/// # Errors
///
/// A flag `start` does not take, or a malformed value.
pub(super) fn parse_daemon_start(rest: &[String]) -> Result<DaemonAction> {
    let mut sources = DataSources::default();
    let mut no_cache = false;
    let mut log_level = "info".to_owned();
    let mut log_file = None;
    let mut elevate = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        if sources.take("start", arg, &mut iter)? {
            continue;
        }
        match arg.as_str() {
            "--no-cache" => no_cache = true,
            "--log-level" => value("start", arg, &mut iter)?.clone_into(&mut log_level),
            "--log-file" => log_file = Some(PathBuf::from(value("start", arg, &mut iter)?)),
            "--elevate" => elevate = true,
            other => return Err(unknown("start", other)),
        }
    }
    Ok(DaemonAction::Start {
        mft_file: sources.mft_file,
        data_dir: sources.data_dir,
        drives: sources.drives,
        no_cache,
        log_level,
        log_file,
        elevate,
    })
}

/// Parse `uffs --daemon load [flags...]`.
///
/// # Errors
///
/// A flag `load` does not take, or a malformed value.
pub(super) fn parse_daemon_load(rest: &[String]) -> Result<DaemonAction> {
    let mut sources = DataSources::default();
    let mut no_cache = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        if sources.take("load", arg, &mut iter)? {
            continue;
        }
        match arg.as_str() {
            "--no-cache" => no_cache = true,
            other => return Err(unknown("load", other)),
        }
    }
    Ok(DaemonAction::Load {
        mft_file: sources.mft_file,
        data_dir: sources.data_dir,
        drives: sources.drives,
        no_cache,
    })
}

/// Refuse any flag after a daemon action that takes none (`restart`).
///
/// # Errors
///
/// The first argument, when there is one.
pub(super) fn parse_no_flags(command: &str, rest: &[String]) -> Result<()> {
    rest.first()
        .map_or(Ok(()), |flag| Err(unknown(command, flag)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use uffs_mft::platform::DriveLetter;

    use super::super::{DaemonAction, parse_daemon_action};

    /// Owned argument vector from string slices.
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().copied().map(String::from).collect()
    }

    #[test]
    fn start_refuses_unknown_flags() {
        let err = parse_daemon_action(&args(&["start", "--idle-timeout", "0"]))
            .expect_err("an unknown flag is a usage error");
        assert!(err.to_string().contains("--idle-timeout"), "{err}");
        parse_daemon_action(&args(&["start", "--mft-file"])).expect_err("missing value");
        parse_daemon_action(&args(&["start", "--drive", "12"])).expect_err("bad drive");
        parse_daemon_action(&args(&["start", "--data-dir", "a", "--data-dir", "b"]))
            .expect_err("two data dirs");
    }

    #[test]
    fn start_accumulates_mft_files_and_drives() {
        let action = parse_daemon_action(&args(&[
            "start",
            "--mft-file",
            "C.bin,D.bin",
            "--mft-file",
            "E.bin",
            "--drive",
            "F",
            "--drives",
            "G:,H",
        ]))
        .expect("valid start");
        let DaemonAction::Start {
            mft_file, drives, ..
        } = action
        else {
            panic!("expected Start, got {action:?}");
        };
        assert_eq!(
            mft_file,
            ["C.bin", "D.bin", "E.bin"].map(PathBuf::from).to_vec()
        );
        assert_eq!(drives, [DriveLetter::F, DriveLetter::G, DriveLetter::H]);
    }

    #[test]
    fn load_refuses_unknown_flags_and_restart_takes_none() {
        parse_daemon_action(&args(&["load", "--bogus"])).expect_err("unknown load flag");
        parse_daemon_action(&args(&["restart", "--drive", "C"]))
            .expect_err("restart takes no flags");
        assert!(matches!(
            parse_daemon_action(&args(&["restart"])),
            Ok(DaemonAction::Restart)
        ));
    }
}
