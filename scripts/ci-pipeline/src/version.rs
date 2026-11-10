// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Version discovery helpers for the UFFS ship pipeline.
//!
//! * [`get_current_version`] — read `[workspace.package].version` out of the
//!   root `Cargo.toml` (simple whole-file scan).
//! * [`extract_version_from_cargo_toml`] — same, but strict: only considers the
//!   `[workspace.package]` section.

use anyhow::{Context as _, Result, bail};

/// Read the workspace root `Cargo.toml` and return the first
/// `version = "..."` string found.  Used by the push step to build
/// `origin/release/vX.Y.Z`.
///
/// # Errors
///
/// Returns an error if `Cargo.toml` cannot be read, or if no `version`
/// line is present.
pub(crate) fn get_current_version() -> Result<String> {
    let cargo_toml = std::fs::read_to_string("Cargo.toml").context("Failed to read Cargo.toml")?;
    for line in cargo_toml.lines() {
        if line.trim().starts_with("version = ")
            && let Some(version) = line.split('"').nth(1)
        {
            return Ok(version.to_owned());
        }
    }
    bail!("Could not find version in Cargo.toml")
}

/// Bump the lockstep `[workspace.package].version` (plus the internal
/// `[workspace.dependencies]` version requirements and the lockfile) by
/// `level` — `"patch"`, `"minor"`, or `"major"` — via `cargo set-version`.
///
/// This restores the Phase-2 version-increment step retired in R5.  It shells
/// out to `cargo-edit`'s `set-version` rather than re-implementing semver math
/// (the ~1430 LOC R5 deleted): `set-version` already handles workspace
/// inheritance and the internal dep-requirement updates correctly.  `just ship`
/// is a maintainer-only release command, so requiring `cargo-edit` on the
/// release machine is acceptable.
///
/// # Errors
///
/// Returns an error if `cargo set-version` is not installed or the bump fails.
pub(crate) fn bump_workspace_version(level: &str) -> Result<()> {
    let available = std::process::Command::new("cargo")
        .args(["set-version", "--help"])
        .output()
        .is_ok_and(|out| out.status.success());
    if !available {
        bail!(
            "`cargo set-version` not found — install it with `cargo install cargo-edit` \
             (required by `just ship` to bump the release version)."
        );
    }
    let status = std::process::Command::new("cargo")
        .args(["set-version", "--bump", level])
        .status()
        .context("running `cargo set-version`")?;
    if !status.success() {
        bail!("`cargo set-version --bump {level}` failed");
    }
    // Refresh the lockfile so the release commit is byte-reproducible.
    std::process::Command::new("cargo")
        .args(["update", "--workspace", "--quiet"])
        .status()
        .context("refreshing Cargo.lock after version bump")?;
    Ok(())
}

/// Set `version:` and `date-released:` in `CITATION.cff` to the release
/// being cut, so the citation metadata names the version it ships with.
///
/// The file was meant to be bumped by the R5-retired version tooling and was
/// left at 0.5.120 for forty releases; this keeps it in the release commit.
/// Only the two top-level keys are rewritten; the rest of the file (comments
/// included) is kept byte for byte.  A missing `CITATION.cff` is not an error.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or written, or if
/// it lacks a top-level `version:` key.
pub(crate) fn sync_citation_file(version: &str, date: &str) -> Result<()> {
    let path = std::path::Path::new("CITATION.cff");
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(path).context("Failed to read CITATION.cff")?;
    let updated = sync_citation(&content, version, date)?;
    if updated != content {
        std::fs::write(path, updated).context("Failed to write CITATION.cff")?;
    }
    Ok(())
}

/// Rewrite the top-level `version:` and `date-released:` lines of a
/// `CITATION.cff` text (see [`sync_citation_file`]).
///
/// # Errors
///
/// Returns an error if `content` has no top-level `version:` key.
fn sync_citation(content: &str, version: &str, date: &str) -> Result<String> {
    let mut found_version = false;
    let mut out = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        let ending = if line.ends_with('\n') { "\n" } else { "" };
        if line.starts_with("version:") {
            found_version = true;
            out.push_str("version: \"");
            out.push_str(version);
            out.push('"');
            out.push_str(ending);
        } else if line.starts_with("date-released:") {
            out.push_str("date-released: \"");
            out.push_str(date);
            out.push('"');
            out.push_str(ending);
        } else {
            out.push_str(line);
        }
    }
    if !found_version {
        bail!("CITATION.cff has no top-level `version:` key");
    }
    Ok(out)
}

/// Parse `content` (the text of a workspace root `Cargo.toml`) and
/// extract the `version = "..."` entry from the `[workspace.package]`
/// table specifically — ignores any unrelated `version = ...` lines in
/// `[dependencies]` or per-crate overrides.
///
/// # Errors
///
/// Returns an error if `[workspace.package]` is missing, or if it does
/// not contain a parseable `version` entry.
pub(crate) fn extract_version_from_cargo_toml(content: &str) -> Result<String> {
    let mut in_workspace_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[workspace.package]" {
            in_workspace_package = true;
            continue;
        }
        if in_workspace_package {
            if trimmed.starts_with('[') && trimmed != "[workspace.package]" {
                break;
            }
            if trimmed.starts_with("version")
                && let Some((_, after_eq)) = trimmed.split_once('=')
            {
                let version = after_eq.trim().trim_matches('"').trim_matches('\'');
                return Ok(version.to_owned());
            }
        }
    }
    bail!("Version extraction failed - no version found in [workspace.package]")
}

#[cfg(test)]
mod tests {
    use super::sync_citation;

    #[test]
    fn citation_version_and_date_are_rewritten_and_the_rest_kept() {
        let before = "cff-version: 1.2.0\n# Keep in sync.\nversion: \"0.5.120\"\ndate-released: \"2026-06-09\"\ntype: software\n";
        let after = sync_citation(before, "0.6.47", "2026-11-10").unwrap();
        assert_eq!(
            after,
            "cff-version: 1.2.0\n# Keep in sync.\nversion: \"0.6.47\"\ndate-released: \"2026-11-10\"\ntype: software\n"
        );
    }

    #[test]
    fn citation_without_version_is_an_error() {
        let _missing = sync_citation("cff-version: 1.2.0\n", "0.6.47", "2026-11-10").unwrap_err();
    }
}
