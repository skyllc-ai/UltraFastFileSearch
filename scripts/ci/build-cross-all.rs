#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! serde_json = "1.0"
//! dirs-next = "2.0"
//! sha2 = "0.10"
//! ```
// =============================================================================
// scripts/ci/build-cross-all.rs - UFFS Local Cross-Platform Build Helper
// =============================================================================
//
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//
// UFFS - UltraFastFileSearch: High-Performance File Search Tool
//! Local cross-compile + publish helper for UFFS binaries.
//!
//! ## Role in the release architecture
//!
//! UFFS has **two distinct build paths**, intentionally separated:
//!
//! ### 1. Local iteration (this script)
//! Fast, developer-friendly builds used for `just q` / `just ship` and
//! day-to-day deploy-to-Windows-test-box loops.  Uses `cargo-xwin` to
//! cross-compile the MSVC target from macOS/Linux and (by default) uploads
//! the shipping binaries to the GitHub Release matching the current
//! workspace version via `gh release create` / `gh release upload`.
//!
//! ### 2. Official releases (`.github/workflows/release.yml`)
//! Tag-triggered clean-room builds.  Each target is built natively on its
//! own GitHub-hosted runner (windows-latest, macos-latest, ubuntu-22.04)
//! with stable CPU baselines (`x86-64-v3` / `apple-m1`) and the full
//! `[profile.release]` (LTO=fat, codegen-units=1, strip=symbols).  These
//! are the authoritative binaries users download from GitHub Releases.
//!
//! **Prefer the tag-triggered `release.yml` path for real releases**:
//! `git tag vX.Y.Z && git push origin vX.Y.Z` → CI builds and publishes.
//! Keep this script for dev iteration and pre-release smoke tests.
//!
//! ## Platform support reality
//!
//! UFFS's **live** indexing (reading a mounted NTFS volume's Master File
//! Table via Windows kernel APIs) is Windows-only.  The engine core is
//! cross-platform, so macOS and Linux binaries are useful for **offline
//! MFT analysis** — loading a captured `.mft` / `.bin` snapshot via
//! `--mft-file`.  The shipping matrix in `release.yml` reflects this:
//! Windows x86_64 as primary, macOS ARM64 + Linux x86_64 for offline.
//!
//! This script currently cross-compiles *only* the MSVC target; macOS and
//! Linux binaries come from `release.yml`.  Future enhancement: teach
//! this script to also build the native-host target for local smoke tests.
//!
//! ## Prerequisites (local use)
//!
//! - macOS or Linux host (for cross-compilation)
//! - rustup target: rustup target add x86_64-pc-windows-msvc
//! - cargo-xwin: cargo install cargo-xwin
//! - LLVM/clang: brew install llvm (macOS) or apt install clang (Linux)
//! - `gh` CLI authenticated, if uploading to a GitHub Release

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::{env, fs};

use sha2::{Digest, Sha256};

/// Host triple for macOS ARM64 (the expected cross-compilation host)
const HOST_TRIPLE: &str = "aarch64-apple-darwin";

/// Binaries uploaded to GitHub Release (the shipping set).
///
/// NOTE: uffs_tui and uffs_gui have moved to the private uffs-products repo.
/// `uffs-broker` is **Windows-only** — it is staged for the Windows target
/// only (see [`is_windows_only`]); off Windows it would just be a no-op stub.
const RELEASE_BINARIES: &[(&str, &str)] = &[
    ("uffs", "uffs-cli"),
    ("uffsd", "uffs-daemon"),
    ("uffsmcp", "uffs-mcp"),
    ("uffs-mft", "uffs-mft"),
    ("uffs-update", "uffs-update"),
    ("uffs-broker", "uffs-broker"),
    ("uffs-watchdog", "uffs-watchdog"),
];

/// Binaries that only make sense on Windows — staged for the Windows target
/// only, never the macOS/Linux ones.
fn is_windows_only(binary: &str) -> bool {
    binary == "uffs-broker" || binary == "uffs-watchdog"
}

/// All workspace binaries — release + diagnostic tools.
/// Everything here gets built (via `--workspace`) and copied to `dist/`.
/// Only `RELEASE_BINARIES` are uploaded to GitHub Release.
const ALL_BINARIES: &[(&str, &str)] = &[
    // Release binaries
    ("uffs", "uffs-cli"),
    ("uffsd", "uffs-daemon"),
    ("uffsmcp", "uffs-mcp"),
    ("uffs-mft", "uffs-mft"),
    ("uffs-update", "uffs-update"),
    ("uffs-broker", "uffs-broker"), // Windows-only (see is_windows_only)
    ("uffs-watchdog", "uffs-watchdog"), // Windows-only (see is_windows_only)
    // Diagnostic binaries (all hyphenated per issue #213 / F1.13).
    ("uffs-analyze-mft-parents", "uffs-diag"),
    ("uffs-dump-mft-records", "uffs-diag"),
    ("uffs-scan-mft-magic", "uffs-diag"),
    ("uffs-dump-mft-extents", "uffs-diag"),
    ("uffs-cross-check-mft-reference", "uffs-diag"),
    ("uffs-compare-raw-mft", "uffs-diag"),
    ("uffs-inspect-mft-record-flow", "uffs-diag"),
    ("uffs-analyze-diff", "uffs-diag"),
    ("uffs-compare-scan-parity", "uffs-diag"),
    ("uffs-verify-iocp-capture", "uffs-diag"),
];

struct Target {
    triple: &'static str,
    platform_name: &'static str,
    use_xwin: bool,
    requires_linker: Option<&'static str>,
}

/// Build mode enum for clearer logic
#[derive(Debug, Clone, Copy, PartialEq)]
enum BuildMode {
    Release,
    Dev,
    Profiling,
}

/// Determine build mode from environment variables.
/// Priority: UFFS_PROFILING_BUILD > UFFS_RELEASE_BUILD > default (release)
fn get_build_mode() -> BuildMode {
    // Check for profiling mode first (highest priority)
    if env::var("UFFS_PROFILING_BUILD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return BuildMode::Profiling;
    }

    // Check for release mode (default is true)
    if env::var("UFFS_RELEASE_BUILD")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
    {
        BuildMode::Release
    } else {
        BuildMode::Dev
    }
}

/// Check if release build mode is enabled via UFFS_RELEASE_BUILD env var.
/// Default is DEV mode for faster iteration during development.
#[allow(dead_code)]
fn is_release_build() -> bool {
    get_build_mode() == BuildMode::Release
}

/// Check if profiling build mode is enabled via UFFS_PROFILING_BUILD env var.
#[allow(dead_code)]
fn is_profiling_build() -> bool {
    get_build_mode() == BuildMode::Profiling
}

/// Get the build profile name for display purposes
fn build_profile() -> &'static str {
    match get_build_mode() {
        BuildMode::Release => "release",
        BuildMode::Dev => "xwin-dev",
        BuildMode::Profiling => "profiling",
    }
}

/// Get the cargo output directory name (where binaries are placed)
/// Note: Custom profiles like "xwin-dev" and "profiling" output to their own directory
fn build_output_dir() -> &'static str {
    match get_build_mode() {
        BuildMode::Release => "release",
        BuildMode::Dev => "xwin-dev",
        BuildMode::Profiling => "profiling",
    }
}

/// UFFS only runs on Windows (requires NTFS MFT access via Windows APIs).
/// We only build Windows binaries - macOS/Linux are just cross-compilation
/// hosts.
const TARGETS: &[Target] = &[Target {
    triple: "x86_64-pc-windows-msvc",
    platform_name: "windows-x64",
    use_xwin: true,
    requires_linker: None,
}];

fn main() {
    let args: Vec<String> = env::args().collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let native_only = args.iter().any(|a| a == "--native-only");

    // Disk safety:
    // By default we prune (delete) cross-target build artifacts after copying
    // binaries into dist/. Each target triple gets its own (potentially huge)
    // subtree under the cargo target-dir. Keeping all of them quickly explodes
    // disk usage (e.g. 4 targets => ~4× polars build).
    //
    // Use --keep-target-artifacts to opt out.
    // Use --prune-host to also delete host release artifacts after copying.
    let prune_cross_targets = !args
        .iter()
        .any(|a| a == "--keep-target-artifacts" || a == "--no-prune");
    let prune_host = args.iter().any(|a| a == "--prune-host");

    println!("🚀 UFFS Cross-Platform Build (Windows Only)");
    println!("ℹ️  UFFS is Windows-only (requires NTFS MFT access)");
    if verbose {
        println!("🔍 Verbose mode enabled");
    }

    // Show build mode
    let build_mode = get_build_mode();
    let build_mode_str = match build_mode {
        BuildMode::Release => "RELEASE (optimized)",
        BuildMode::Dev => "DEV (fast, default)",
        BuildMode::Profiling => "PROFILING (optimized + debug symbols for samply/PerfView)",
    };
    println!("🔧 Build mode: {}", build_mode_str);
    if build_mode == BuildMode::Profiling {
        println!("📊 Profiling build: binaries will include PDB symbols for analysis");
    }

    let (host_os, host_arch) = (env::consts::OS, env::consts::ARCH);
    println!(
        "🖥️  Host: {} {} (cross-compilation host)",
        host_os, host_arch
    );

    // On Windows, just build natively
    if native_only || host_os == "windows" {
        if native_only {
            println!("ℹ️  --native-only flag set. Using native-only build.");
        } else {
            println!("🎯 Running on Windows - building natively...");
        }
        build_native_only();
        return;
    }

    // On macOS/Linux, cross-compile for Windows
    if host_os != "macos" && host_os != "linux" {
        eprintln!(
            "⚠️  Unsupported host OS: {}. Use Windows, macOS, or Linux.",
            host_os
        );
        exit(1);
    }

    let version = read_current_version();
    let target_dir = get_cargo_target_dir();

    // Preflight: remove any leftover cross-target build trees from previous runs.
    // This prevents starting a new release with disk already consumed.
    if prune_cross_targets {
        prune_previous_cross_target_artifacts(&target_dir);
    }

    println!(
        "📦 Version: {}\n📂 Target: {}",
        version,
        target_dir.display()
    );

    // Ensure required tools are present before we start building
    ensure_required_tools_or_exit();

    let available = check_available_targets();
    println!("\n📋 Available targets:");
    for t in &available {
        println!("   ✅ {} ({})", t.triple, t.platform_name);
    }

    // Build order: non-host targets first, host last.
    // This lowers peak disk usage because the host build can be relatively large, and
    // we don't want it resident while building every other target triple.
    let mut build_order: Vec<&Target> = available
        .iter()
        .filter(|t| t.triple != HOST_TRIPLE)
        .copied()
        .collect();
    if let Some(host) = available.iter().find(|t| t.triple == HOST_TRIPLE) {
        build_order.push(host);
    }

    for target in &build_order {
        println!(
            "\n{}\n🎯 Building {} ({})\n{}",
            "═".repeat(60),
            target.triple,
            target.platform_name,
            "═".repeat(60)
        );

        print_free_disk(&target_dir, "before build");

        if !build_for_target(target, &target_dir, verbose) {
            eprintln!("\n❌ Build failed for {} - aborting!", target.triple);
            exit(1);
        }

        // Only stage binaries for release builds (not profiling or dev)
        if build_mode == BuildMode::Release {
            if !stage_binaries(&version, target, &target_dir) {
                eprintln!("\n❌ Binary staging failed for {} - aborting!", target.triple);
                eprintln!("   This usually means the build succeeded but binaries were not placed in the expected location.");
                eprintln!(
                    "   Check the target directory: {:?}",
                    target_dir.join(target.triple)
                );
                exit(1);
            }
        }

        // Critical disk optimization: delete the *target-specific* build tree after
        // we've staged the final binaries.
        if prune_cross_targets && target.triple != HOST_TRIPLE {
            prune_target_artifacts_for_triple(&target_dir, target.triple);
            print_free_disk(&target_dir, "after prune");
        }

        // Optional: also prune host release artifacts after building it.
        if prune_host && target.triple == HOST_TRIPLE {
            prune_host_release_artifacts(&target_dir);
            print_free_disk(&target_dir, "after host prune");
        }
    }

    // Upload to GitHub Release for release builds
    if build_mode == BuildMode::Release {
        // Also stage macOS host binaries (built in the earlier release build step)
        stage_host_binaries(&version, &target_dir);

        create_checksums_from_staging(&version);
        upload_to_github_release(&version);

        // Integration test: download release back to dist/ for local use
        download_release_to_dist(&version);

        // Copy all binaries (incl. diag) to dist/ — diag binaries aren't on
        // GitHub Release, so we copy them directly from the build output.
        copy_all_binaries_to_dist(&version, &target_dir);

        println!(
            "\n✅ Build complete!\n📦 Binaries uploaded to GitHub Release {} and cached in dist/{}",
            version, version
        );
    } else if build_mode == BuildMode::Profiling {
        println!(
            "\n✅ Profiling build complete!\n📦 Binaries in {:?}/{}/profiling/",
            target_dir, TARGETS[0].triple
        );
        println!("📋 Run 'just profile-usb' to copy to USB for Windows profiling");
    } else {
        println!("\n✅ Dev build complete!");
    }
    println!("ℹ️  Note: UFFS only runs on Windows (requires NTFS MFT access)");
}

/// Upload staged binaries to a GitHub Release using `gh release create`.
/// Creates the release and uploads all binaries + checksums as assets.
fn upload_to_github_release(version: &str) {
    println!("\n📦 Uploading binaries to GitHub Release {}...", version);
    let staging_dir = PathBuf::from(format!("target/release-staging/{}", version));

    if !staging_dir.exists() {
        eprintln!("  ❌ Staging directory does not exist: {:?}", staging_dir);
        exit(1);
    }

    // Collect all files to upload
    let mut assets: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&staging_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                assets.push(path.to_string_lossy().to_string());
            }
        }
    }

    if assets.is_empty() {
        eprintln!("  ❌ No assets found in staging directory");
        exit(1);
    }

    println!("  📋 Uploading {} assets:", assets.len());
    for a in &assets {
        println!("     → {}", Path::new(a).file_name().unwrap_or_default().to_string_lossy());
    }

    // Build gh release create command
    let mut args = vec![
        "release".to_string(),
        "create".to_string(),
        version.to_string(),
        "--title".to_string(),
        format!("UFFS {}", version),
        "--notes".to_string(),
        format!("UFFS {} — Windows binaries (cross-compiled from macOS ARM64).\n\nSee README for installation instructions.", version),
        "--latest".to_string(),
    ];

    // Add all asset files
    for asset in &assets {
        args.push(asset.clone());
    }

    let status = Command::new("gh")
        .args(&args)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("  ✅ GitHub Release {} created with {} assets", version, assets.len());
        }
        Ok(s) => {
            // Release may already exist — try uploading assets to existing release
            eprintln!("  ⚠️  gh release create exited with {}, trying to upload to existing release...", s);
            let upload_status = Command::new("gh")
                .args(["release", "upload", version, "--clobber"])
                .args(&assets)
                .status();
            match upload_status {
                Ok(s) if s.success() => {
                    println!("  ✅ Assets uploaded to existing release {}", version);
                }
                Ok(s) => {
                    eprintln!("  ❌ Failed to upload assets: exit {}", s);
                    exit(1);
                }
                Err(e) => {
                    eprintln!("  ❌ Failed to run gh release upload: {}", e);
                    exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("  ❌ Failed to run gh: {}", e);
            eprintln!("  💡 Make sure `gh` CLI is installed and authenticated");
            exit(1);
        }
    }

    // Clean up staging directory
    let _ = fs::remove_dir_all(&staging_dir);
    println!("  🧹 Cleaned up staging directory");
}

/// Stage macOS host binaries into the release staging directory.
/// These are built by the earlier `cargo build --release` step.
fn stage_host_binaries(version: &str, target_dir: &Path) {
    println!("\n📁 Staging macOS ARM64 binaries...");
    let staging_dir = PathBuf::from(format!("target/release-staging/{}", version));
    let _ = fs::create_dir_all(&staging_dir);

    for (binary, _) in RELEASE_BINARIES {
        // Windows-only binaries (the broker) are never staged for macOS.
        if is_windows_only(binary) {
            continue;
        }
        let source = target_dir.join("release").join(binary);
        let dest_name = format!("{}-macos-arm64", binary);
        let dest = staging_dir.join(&dest_name);

        if source.exists() {
            if let Err(e) = fs::copy(&source, &dest) {
                eprintln!("  ⚠️  Failed to stage macOS {}: {}", binary, e);
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(m) = fs::metadata(&dest) {
                        let mut p = m.permissions();
                        p.set_mode(0o755);
                        let _ = fs::set_permissions(&dest, p);
                    }
                }
                if let Ok(m) = fs::metadata(&dest) {
                    let size_mb = m.len() as f64 / 1_048_576.0;
                    println!("  ✅ {} ({:.1} MB)", dest_name, size_mb);
                }
            }
        } else {
            eprintln!("  ⚠️  macOS binary not found: {:?} (skipping)", source);
        }
    }
}

/// Copy ALL workspace binaries (release + diag) to `dist/<version>/`.
/// Release binaries are already there from `download_release_to_dist`,
/// so this adds the diagnostic binaries from the build output.
fn copy_all_binaries_to_dist(version: &str, target_dir: &Path) {
    let dist_dir = PathBuf::from(format!("dist/{}", version));
    let _ = fs::create_dir_all(&dist_dir);

    let output_dir = build_output_dir();
    let mut copied = 0u32;

    // Diagnostic binaries only — release binaries are already in dist/ from GH download.
    let diag_binaries: Vec<_> = ALL_BINARIES
        .iter()
        .filter(|(b, _)| !RELEASE_BINARIES.iter().any(|(rb, _)| rb == b))
        .collect();

    if diag_binaries.is_empty() {
        return;
    }

    println!("\n📁 Copying diagnostic binaries to dist/{}...", version);

    for target in TARGETS {
        for (binary, _) in &diag_binaries {
            let bin_name = if target.triple.contains("windows") {
                format!("{}.exe", binary)
            } else {
                (*binary).to_string()
            };
            let source = target_dir
                .join(target.triple)
                .join(output_dir)
                .join(&bin_name);
            let dest_name = if target.triple.contains("windows") {
                format!("{}-{}.exe", binary, target.platform_name)
            } else {
                format!("{}-{}", binary, target.platform_name)
            };
            let dest = dist_dir.join(&dest_name);

            if source.exists() {
                if let Err(e) = fs::copy(&source, &dest) {
                    eprintln!("  ⚠️  Failed to copy {}: {}", dest_name, e);
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(m) = fs::metadata(&dest) {
                            let mut p = m.permissions();
                            p.set_mode(0o755);
                            let _ = fs::set_permissions(&dest, p);
                        }
                    }
                    copied += 1;
                    println!("  ✅ {}", dest_name);
                }
            }
        }
    }

    // Also copy macOS host diag binaries
    for (binary, _) in &diag_binaries {
        let source = target_dir.join("release").join(binary);
        let dest_name = format!("{}-macos-arm64", binary);
        let dest = dist_dir.join(&dest_name);

        if source.exists() {
            if let Err(e) = fs::copy(&source, &dest) {
                eprintln!("  ⚠️  Failed to copy {}: {}", dest_name, e);
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(m) = fs::metadata(&dest) {
                        let mut p = m.permissions();
                        p.set_mode(0o755);
                        let _ = fs::set_permissions(&dest, p);
                    }
                }
                copied += 1;
                println!("  ✅ {}", dest_name);
            }
        }
    }

    if copied > 0 {
        println!("  📦 {} diagnostic binaries copied to dist/{}", copied, version);
    }
}


/// Download the release from GitHub back to dist/ as an integration test
/// and local cache for `just use`.
fn download_release_to_dist(version: &str) {
    println!("\n🔄 Downloading release {} from GitHub (integration test)...", version);
    let dist_dir = format!("dist/{}", version);

    if let Err(e) = fs::create_dir_all(&dist_dir) {
        eprintln!("  ❌ Failed to create {}: {}", dist_dir, e);
        return;
    }

    let status = Command::new("gh")
        .args(["release", "download", version, "--dir", &dist_dir, "--clobber"])
        .status();

    match status {
        Ok(s) if s.success() => {
            // List what we downloaded
            if let Ok(entries) = fs::read_dir(&dist_dir) {
                let mut files: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let size = e.metadata().ok().map(|m| m.len()).unwrap_or(0);
                        Some(format!("     {} ({:.1} MB)", name, size as f64 / 1_048_576.0))
                    })
                    .collect();
                files.sort();
                println!("  ✅ Downloaded {} files to {}:", files.len(), dist_dir);
                for f in &files {
                    println!("{}", f);
                }
            }

            // Prune old versions (keep current + 1 previous)
            prune_old_dist_versions(version, 2);
        }
        Ok(s) => {
            eprintln!("  ⚠️  gh release download exited with {} — dist/ may be incomplete", s);
        }
        Err(e) => {
            eprintln!("  ⚠️  Failed to run gh release download: {} — dist/ not populated", e);
        }
    }
}

/// Keep only the `keep` most recent versioned directories in dist/.
fn prune_old_dist_versions(current: &str, keep: usize) {
    let dist = Path::new("dist");
    if !dist.exists() {
        return;
    }

    let mut versions: Vec<String> = fs::read_dir(dist)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('v') { Some(name) } else { None }
        })
        .collect();

    versions.sort();

    let to_remove: Vec<String> = if versions.len() > keep {
        versions[..versions.len() - keep]
            .iter()
            .filter(|v| v.as_str() != current)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    for v in &to_remove {
        let path = dist.join(v);
        if let Err(e) = fs::remove_dir_all(&path) {
            eprintln!("  ⚠️  Failed to remove dist/{}: {}", v, e);
        } else {
            println!("  🗑️  Pruned old dist/{}", v);
        }
    }

    if !to_remove.is_empty() {
        println!("  ✅ Pruned {} old version(s), kept {}", to_remove.len(), keep);
    }
}

fn build_native_only() {
    let s = Command::new("rust-script")
        .arg("scripts/dev/build-local.rs")
        .status()
        .expect("Failed");
    if !s.success() {
        exit(1);
    }
}

fn read_current_version() -> String {
    // Try UFFS workflow state first
    if let Ok(c) = fs::read_to_string("build/.uffs-workflow-state.json") {
        if let Ok(s) = serde_json::from_str::<serde_json::Value>(&c) {
            if let Some(v) = s.get("current_version").and_then(|v| v.as_str()) {
                if v != "unknown" {
                    return format!("v{}", v);
                }
            }
        }
    }
    // Fallback to Cargo.toml [workspace.package] version
    if let Ok(c) = fs::read_to_string("Cargo.toml") {
        let mut in_workspace_package = false;
        for l in c.lines() {
            let trimmed = l.trim();
            if trimmed == "[workspace.package]" {
                in_workspace_package = true;
                continue;
            }
            if in_workspace_package {
                if trimmed.starts_with('[') && trimmed != "[workspace.package]" {
                    break;
                }
                if trimmed.starts_with("version") {
                    if let Some(v) = trimmed.split('"').nth(1) {
                        return format!("v{}", v);
                    }
                }
            }
        }
    }
    "v0.1.0".to_owned()
}

fn get_cargo_target_dir() -> PathBuf {
    if let Ok(d) = env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = parse_cargo_config_target_dir() {
        return d;
    }
    PathBuf::from("./target")
}

fn parse_cargo_config_target_dir() -> Option<PathBuf> {
    if let Ok(c) = fs::read_to_string(".cargo/config.toml") {
        for l in c.lines() {
            let t = l.trim();
            if t.starts_with("target-dir") {
                if let Some(v) = t.split('=').nth(1) {
                    let p = v.trim().trim_matches('"').trim_matches('\'');
                    if p.starts_with("~/") {
                        if let Some(h) = dirs_next::home_dir() {
                            return Some(h.join(p.strip_prefix("~/").unwrap_or("")));
                        }
                    }
                    return Some(PathBuf::from(p));
                }
            }
        }
    }
    None
}

fn check_available_targets() -> Vec<&'static Target> {
    let installed = get_installed_targets();
    TARGETS
        .iter()
        .filter(|t| {
            if !installed.contains(&t.triple.to_string()) {
                println!("   ⚠️  {} not installed", t.triple);
                return false;
            }
            if let Some(l) = t.requires_linker {
                if !cmd_exists(l) {
                    println!("   ⚠️  {} skipped (no {})", t.triple, l);
                    return false;
                }
            }
            if t.use_xwin && !cmd_exists("cargo-xwin") {
                println!("   ⚠️  {} skipped (no cargo-xwin)", t.triple);
                return false;
            }
            true
        })
        .collect()
}

fn get_installed_targets() -> Vec<String> {
    let o = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .expect("rustup");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(String::from)
        .collect()
}

fn cmd_exists(c: &str) -> bool {
    Command::new("which")
        .arg(c)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Disk space monitoring and artifact pruning
// ─────────────────────────────────────────────────────────────────────────────

/// Returns free disk space in GiB for the volume containing `path`.
fn disk_free_gib(path: &Path) -> f64 {
    // Use `df` to get free space - works on macOS and Linux
    let output = Command::new("df")
        .arg("-k") // 1K blocks
        .arg(path)
        .output();

    if let Ok(o) = output {
        let stdout = String::from_utf8_lossy(&o.stdout);
        // Parse the second line (first is header)
        if let Some(line) = stdout.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // df -k output: Filesystem 1K-blocks Used Available Use% Mounted
            if parts.len() >= 4 {
                if let Ok(avail_kb) = parts[3].parse::<u64>() {
                    return avail_kb as f64 / (1024.0 * 1024.0); // KB to GiB
                }
            }
        }
    }
    0.0
}

/// Print free disk space for the volume containing `path`.
fn print_free_disk(path: &Path, label: &str) {
    let free = disk_free_gib(path);
    println!("  💾 Free disk space ({}): {:.1} GiB", label, free);
}

/// Remove a directory tree, ignoring errors (best-effort cleanup).
fn remove_dir_best_effort(path: &Path) {
    if path.exists() {
        if let Err(e) = fs::remove_dir_all(path) {
            eprintln!("  ⚠️  Could not remove {}: {}", path.display(), e);
        }
    }
}

/// Preflight cleanup: remove leftover cross-target build trees from previous runs.
/// This prevents starting a new release with disk already consumed by stale artifacts.
fn prune_previous_cross_target_artifacts(target_dir: &Path) {
    println!("\n🧹 Preflight cleanup: removing leftover cross-target artifacts...");
    let mut removed_any = false;

    for target in TARGETS {
        // Skip host target - we don't want to remove our own build artifacts
        if target.triple == HOST_TRIPLE {
            continue;
        }

        let triple_dir = target_dir.join(target.triple);
        if triple_dir.exists() {
            println!("  🗑️  Removing {}", triple_dir.display());
            remove_dir_best_effort(&triple_dir);
            removed_any = true;
        }
    }

    if !removed_any {
        println!("  ✅ No leftover cross-target artifacts found");
    } else {
        print_free_disk(target_dir, "after preflight cleanup");
    }
}

/// Prune build artifacts for a specific target triple after building.
fn prune_target_artifacts_for_triple(target_dir: &Path, triple: &str) {
    let triple_dir = target_dir.join(triple);
    if triple_dir.exists() {
        println!("  🗑️  Pruning artifacts for {}", triple);
        remove_dir_best_effort(&triple_dir);
    }
}

/// Prune host release artifacts (optional, for extreme disk savings).
fn prune_host_release_artifacts(target_dir: &Path) {
    let release_dir = target_dir.join("release");
    if release_dir.exists() {
        println!("  🗑️  Pruning host release artifacts");
        remove_dir_best_effort(&release_dir);
    }
}

/// Ensure required tools are installed before starting the build.
fn ensure_required_tools_or_exit() {
    let mut missing = Vec::new();

    // Check for cargo-xwin (required for Windows cross-compilation)
    if !cmd_exists("cargo-xwin") {
        missing.push("cargo-xwin (install with: cargo install cargo-xwin)");
    }

    // Check for LLVM/clang-cl (required for Windows cross-compilation on macOS)
    if !Path::new("/opt/homebrew/opt/llvm/bin/clang-cl").exists() {
        missing.push("LLVM clang-cl (install with: brew install llvm)");
    }

    if !missing.is_empty() {
        eprintln!("\n❌ Missing required tools:");
        for tool in &missing {
            eprintln!("   • {}", tool);
        }
        exit(1);
    }
}

fn build_for_target(target: &Target, target_dir: &Path, verbose: bool) -> bool {
    let build_mode = get_build_mode();
    let profile = build_profile();

    // Build all binaries in a single cargo invocation.
    // This shares the entire dependency compilation chain (Polars alone is ~4 min)
    // instead of recompiling it per binary.
    let mut args: Vec<&str> = if target.use_xwin {
        vec!["xwin", "build"]
    } else {
        vec!["build"]
    };

    // Add profile based on build mode
    match build_mode {
        BuildMode::Release => {
            args.push("--release");
        }
        BuildMode::Profiling => {
            // Use profiling profile for performance analysis with debug symbols
            args.extend_from_slice(&["--profile", "profiling"]);
        }
        BuildMode::Dev => {
            if target.use_xwin {
                // Use xwin-dev profile for xwin dev builds to avoid COFF archive size limits
                // See: docs/xwin-msvc-rlib-size-root-cause-and-workarounds.md
                args.extend_from_slice(&["--profile", "xwin-dev"]);
            }
        }
    }

    // Add target triple and build entire workspace in one invocation.
    // All binaries (incl. diag) are built; only RELEASE_BINARIES get uploaded
    // to GitHub Release, but ALL_BINARIES are copied to dist/.
    args.extend_from_slice(&["--target", target.triple, "--workspace"]);

    // Print what we're building
    println!(
        "  → workspace ({}) → cargo {} (target: {})",
        profile,
        args.join(" "),
        target.triple
    );

    let mut cmd = Command::new("cargo");
    cmd.args(&args);

    // Set CARGO_TARGET_DIR to the expanded path (cargo xwin doesn't expand ~ in config)
    cmd.env("CARGO_TARGET_DIR", target_dir);

    // For Windows cross-compilation, add LLVM to PATH for clang-cl
    if target.use_xwin {
        let llvm_bin = "/opt/homebrew/opt/llvm/bin";
        let current_path = env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", llvm_bin, current_path);
        cmd.env("PATH", new_path);
        cmd.env(
            "CC_x86_64_pc_windows_msvc",
            format!("{}/clang-cl", llvm_bin),
        );
        cmd.env(
            "CXX_x86_64_pc_windows_msvc",
            format!("{}/clang-cl", llvm_bin),
        );
        cmd.env(
            "AR_x86_64_pc_windows_msvc",
            format!("{}/llvm-lib", llvm_bin),
        );
    }

    // For Linux musl cross-compilation, set bindgen to use musl headers
    if target.triple == "x86_64-unknown-linux-musl" {
        let musl_sysroot = "/opt/homebrew/opt/musl-cross/libexec/x86_64-linux-musl";
        cmd.env(
            "BINDGEN_EXTRA_CLANG_ARGS",
            format!(
                "--sysroot={} -isystem {}/include",
                musl_sysroot, musl_sysroot
            ),
        );
    }

    // In verbose mode, inherit stdio; otherwise capture output
    if verbose {
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::piped());
    }

    let output = cmd.output().expect("cargo failed to start");
    if !output.status.success() {
        eprintln!("  ❌ Build failed for {}", target.triple);
        if !verbose {
            // Print stderr on failure even in non-verbose mode
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprintln!("{}", stderr);
            }
        }
        return false;
    }

    for (binary, _) in ALL_BINARIES {
        println!("  ✅ {}", binary);
    }
    true
}

/// Stage built binaries to a temporary directory for GitHub Release upload.
/// Returns true if ALL binaries were staged successfully.
fn stage_binaries(version: &str, target: &Target, target_dir: &Path) -> bool {
    let output_dir = build_output_dir();
    let staging_dir = PathBuf::from(format!("target/release-staging/{}", version));
    println!("  📁 Staging binaries for release {}...", version);

    if let Err(e) = fs::create_dir_all(&staging_dir) {
        eprintln!("  ❌ Failed to create staging dir: {}", e);
        return false;
    }

    let mut all_success = true;

    for (binary, _) in RELEASE_BINARIES {
        // The broker only ships on Windows — skip it for mac/linux targets.
        if is_windows_only(binary) && !target.triple.contains("windows") {
            continue;
        }
        let bin_name = if target.triple.contains("windows") {
            format!("{}.exe", binary)
        } else {
            (*binary).to_string()
        };
        let source = target_dir
            .join(target.triple)
            .join(output_dir)
            .join(&bin_name);
        let dest_name = if target.triple.contains("windows") {
            format!("{}-{}.exe", binary, target.platform_name)
        } else {
            format!("{}-{}", binary, target.platform_name)
        };
        let dest = staging_dir.join(&dest_name);

        if source.exists() {
            if let Err(e) = fs::copy(&source, &dest) {
                eprintln!("  ❌ Failed to stage {}: {}", binary, e);
                all_success = false;
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(m) = fs::metadata(&dest) {
                        let mut p = m.permissions();
                        p.set_mode(0o755);
                        let _ = fs::set_permissions(&dest, p);
                    }
                }
                println!("  ✅ {}", dest_name);
            }
        } else {
            eprintln!("  ❌ Binary not found: {:?}", source);
            all_success = false;
        }
    }

    all_success
}

/// Create checksums file in the staging directory.
/// Create checksums for all binaries in the staging directory.
fn create_checksums_from_staging(version: &str) {
    println!("\n📋 Creating checksums...");
    let staging_dir = PathBuf::from(format!("target/release-staging/{}", version));
    let checksums_path = staging_dir.join("CHECKSUMS.txt");
    let mut lines: Vec<String> = Vec::new();

    if let Ok(entries) = fs::read_dir(&staging_dir) {
        let mut files: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .collect();
        files.sort_by_key(|e| e.file_name());

        for entry in &files {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "CHECKSUMS.txt" {
                continue;
            }
            let p = entry.path();
            if let Some(hash) = calc_hash(&p) {
                if let Ok(m) = fs::metadata(&p) {
                    lines.push(format!("{}  {} ({} bytes)", hash, name, m.len()));
                }
            }
        }
    }

    if let Err(e) = fs::write(&checksums_path, lines.join("\n") + "\n") {
        eprintln!("⚠️  Failed to write checksums: {}", e);
    } else {
        println!("✅ Created {} ({} entries)", checksums_path.display(), lines.len());
    }
}

fn calc_hash(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|c| {
        let mut h = Sha256::new();
        h.update(&c);
        format!("{:x}", h.finalize())
    })
}
