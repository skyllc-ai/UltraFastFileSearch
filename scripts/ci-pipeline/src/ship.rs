// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

#![expect(
    clippy::print_stdout,
    reason = "operational CLI tool — ship-pipeline phase banners + completion footers go to stdout (issue #212)"
)]

//! Resumable ship pipeline driver.
//!
//! Everything in this module wraps [`crate::exec::execute_step_with_tracking`]
//! around a pipeline step so the on-disk
//! [`crate::workflow::WorkflowState`] records each transition — a
//! re-run after a failure picks up at the first non-completed step.
//!
//! Layout:
//! * `tracked_*_step` — one helper per resumable pipeline step (toolchain,
//!   clean, coverage, parallel validation, format check).
//! * `run_enhanced_phase1` / `run_enhanced_phase2` — thin orchestrators over
//!   those tracked helpers.
//! * `load_or_reset_ship_state` / `print_ship_*` — resume / summary /
//!   next-steps helpers.
//! * `run_ship_pipeline` — the entry point `just ship` ultimately calls.

use core::time::Duration;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use colored::Colorize as _;
use tokio::process::Command;

use crate::changelog::roll_changelog_file;
use crate::context::{
    PipelineContext, active_rustc_id, bytes_to_gib, dir_size_bytes, disk_free_bytes,
    get_cargo_target_dir,
};
use crate::exec::{
    execute_command, execute_command_with_env, execute_parallel_with_env,
    execute_step_with_tracking,
};
use crate::git_ops::{count_unpushed_commits, git_commit, git_push};
use crate::version::{bump_workspace_version, get_current_version, sync_citation_file};
use crate::workflow::{
    ALL_STEPS, STEP_CLEAN_ARTIFACTS, STEP_COVERAGE_TESTS, STEP_FORMAT_CHECK, STEP_FORMAT_CODE,
    STEP_GIT_COMMIT, STEP_GIT_PUSH, STEP_PARALLEL_VALIDATION, STEP_TOOLCHAIN_SYNC, WorkflowPhase,
    WorkflowState,
};

// ─────────────────────────────────────────────────────────────────────────────
// Tracked step helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Tracked toolchain step: `--fresh` runs bump to today's nightly via
/// `just toolchain-sync` (unless `--skip-toolchain-sync`); resumable
/// runs only ensure the currently pinned nightly is installed so
/// repeat ship invocations don't churn `rust-toolchain.toml`.
async fn tracked_toolchain_step(state: &mut WorkflowState, ctx: &PipelineContext) -> Result<()> {
    let (step_label, step_recipe) = if ctx.flags.fresh && !ctx.flags.skip_toolchain_sync {
        (
            "Toolchain sync (fresh bump to latest nightly)",
            "toolchain-sync",
        )
    } else {
        ("Toolchain ensure (pinned nightly)", "toolchain-ensure")
    };
    execute_step_with_tracking(state, STEP_TOOLCHAIN_SYNC, || async {
        execute_command(step_label, "just", &[step_recipe], ctx).await
    })
    .await
}

/// Filename (under `CARGO_TARGET_DIR`) recording the `rustc -vV`
/// fingerprint of the toolchain that last built the cache.  The clean
/// step compares it against the active toolchain on every run: a nightly
/// bump in step 00 leaves the shared target dir built by the previous
/// rustc, whose cross-crate metadata is version-specific, so reusing it
/// explodes with `E0514`.  A mismatch forces a `cargo clean` before any
/// build step runs.
const RUSTC_FINGERPRINT_FILE: &str = ".uffs-ci-rustc-fingerprint";

/// Outcome of the clean-step decision.  Extracted as a pure value so the
/// side-effecting async step body stays a thin shell over the
/// unit-tested [`decide_clean`] policy.
enum CleanDecision {
    /// Run `cargo clean`; the payload is the human-readable reason.
    Clean(&'static str),
    /// Skip the clean — nothing is stale and the disk thresholds are fine.
    Skip,
    /// A toolchain change needs a clean but `--no-clean` suppressed it;
    /// the stale fingerprint is left in place so the next run re-detects
    /// the mismatch.
    SuppressedToolchainChange,
}

/// User's explicit clean preference, derived from the `--clean` /
/// `--no-clean` flags.  Modelled as an enum (rather than two bools) so
/// the pure [`decide_clean`] policy stays under
/// `clippy::fn_params_excessive_bools`.
#[derive(Clone, Copy)]
enum CleanMode {
    /// `--clean`: clean unconditionally.
    Force,
    /// Neither flag set: apply the auto-clean heuristics.
    Auto,
    /// `--no-clean`: never auto-clean (a needed toolchain clean is
    /// downgraded to a warning so the next run can re-detect it).
    Never,
}

/// Pure clean-step policy.  Precedence, highest first:
///
/// 1. [`CleanMode::Force`] (`--clean`) always cleans.
/// 2. [`CleanMode::Never`] (`--no-clean`) suppresses everything (a pending
///    toolchain change is reported separately so its fingerprint can be
///    preserved).
/// 3. A toolchain (rustc) change forces a clean to avoid stale-artifact
///    `E0514`.
/// 4. Disk pressure (low free space or oversized target) auto-cleans.
const fn decide_clean(
    mode: CleanMode,
    toolchain_changed: bool,
    disk_pressure: bool,
) -> CleanDecision {
    match mode {
        CleanMode::Force => CleanDecision::Clean("Forced clean (--clean flag)"),
        CleanMode::Never => {
            if toolchain_changed {
                CleanDecision::SuppressedToolchainChange
            } else {
                CleanDecision::Skip
            }
        }
        CleanMode::Auto => {
            if toolchain_changed {
                CleanDecision::Clean(
                    "Toolchain changed (rustc fingerprint mismatch) — forcing clean to avoid stale-artifact E0514",
                )
            } else if disk_pressure {
                CleanDecision::Clean("Auto-clean triggered (disk space low or target too large)")
            } else {
                CleanDecision::Skip
            }
        }
    }
}

/// Return `true` when `target_dir` already holds build output — i.e. any
/// entry other than the fingerprint file itself.  A missing or
/// fingerprint-only dir means there is nothing to invalidate, so a fresh
/// checkout (or the state right after a `cargo clean`) never triggers a
/// spurious toolchain-clean.
fn target_dir_has_build_output(target_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(target_dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| &*entry.file_name().to_string_lossy() != RUSTC_FINGERPRINT_FILE)
}

/// Persist the active `rustc` fingerprint under `target_dir`, recording
/// the toolchain that (re)built the cache so a later run can detect the
/// next bump.  Best-effort: recreates the dir if `cargo clean` removed it
/// and swallows any write error — a missing marker only costs the next
/// run a re-probe, never a hard failure.
fn record_rustc_fingerprint(target_dir: &Path, id: &str) {
    if fs::create_dir_all(target_dir).is_ok() {
        _ = fs::write(target_dir.join(RUSTC_FINGERPRINT_FILE), id);
    }
}

/// Tracked clean step: forces a `cargo clean` when the active `rustc`
/// differs from the toolchain that built the cached `target` dir (a
/// nightly bump → stale cross-crate metadata → `E0514`), and otherwise
/// applies the disk-pressure auto-clean policy.  `--clean` always cleans;
/// `--no-clean` suppresses both paths.  After the decision the active
/// toolchain fingerprint is recorded for the next run.
async fn tracked_clean_step(state: &mut WorkflowState, ctx: &PipelineContext) -> Result<()> {
    execute_step_with_tracking(state, STEP_CLEAN_ARTIFACTS, || async {
        let target_dir = get_cargo_target_dir();
        let free_gb = disk_free_bytes(&target_dir).await.map(bytes_to_gib);
        let target_gb = dir_size_bytes(&target_dir, Duration::from_secs(30))
            .await
            .map(bytes_to_gib);

        // Toolchain fingerprint: did rustc change since this target dir
        // was last built?  Only meaningful when the dir actually holds
        // build output and we could read the active toolchain id.
        let active_id = active_rustc_id();
        let stored_id = fs::read_to_string(target_dir.join(RUSTC_FINGERPRINT_FILE))
            .ok()
            .map(|text| text.trim().to_owned());
        let toolchain_changed = active_id.is_some()
            && target_dir_has_build_output(&target_dir)
            && stored_id.as_deref() != active_id.as_deref();

        if ctx.flags.verbose {
            if let Some(free) = free_gb {
                println!("  💾 Free disk space: {free} GiB");
            }
            if let Some(size) = target_gb {
                println!("  📁 Target directory size: {size} GiB");
            }
        }

        let disk_pressure = free_gb.is_some_and(|gb| gb < ctx.min_free_gb)
            || target_gb.is_some_and(|gb| gb > ctx.max_target_gb);

        let mode = if ctx.flags.force_clean {
            CleanMode::Force
        } else if ctx.flags.force_no_clean {
            CleanMode::Never
        } else {
            CleanMode::Auto
        };

        match decide_clean(mode, toolchain_changed, disk_pressure) {
            CleanDecision::Clean(reason) => {
                println!("  🧹 {reason}");
                // `cargo clean` doesn't compile anything but still probes
                // the toolchain via `<rustc-wrapper> rustc -vV`.  On some
                // macOS hosts sccache's wrapped probe dies with "Operation
                // not permitted" in nested subprocesses even when it works
                // at the top level.  Since clean never needs a wrapper,
                // force-clear `RUSTC_WRAPPER` for this specific step.
                execute_command_with_env(
                    "Clean build artifacts",
                    "cargo",
                    &["clean"],
                    &[("RUSTC_WRAPPER", "")],
                    ctx,
                )
                .await?;
            }
            CleanDecision::SuppressedToolchainChange => {
                println!(
                    "  ⚠️  rustc changed but --no-clean is set — keeping stale artifacts (build may hit E0514)"
                );
                // Leave the stale fingerprint untouched so the next run
                // re-detects the mismatch and can clean.
                return Ok(());
            }
            CleanDecision::Skip => {
                println!("  ⏭️  Skipping clean (disk OK, target size OK, toolchain unchanged)");
            }
        }

        // Record the toolchain that will build this target dir (after a
        // clean, or confirming the unchanged cache) for next-run detection.
        if let Some(id) = active_id {
            record_rustc_fingerprint(&target_dir, &id);
        }
        Ok(())
    })
    .await
}

/// Tracked coverage-tests step: `cargo llvm-cov nextest` across the
/// workspace.  Uses `--lib --bins --tests` rather than `--all-targets`
/// to exclude benchmarks whose init-time `DataFrame` allocations would
/// SIGKILL `nextest` during test enumeration.
async fn tracked_coverage_tests_step(
    state: &mut WorkflowState,
    ctx: &PipelineContext,
) -> Result<()> {
    execute_step_with_tracking(state, STEP_COVERAGE_TESTS, || async {
        execute_command(
            "Coverage tests",
            "cargo",
            &[
                "llvm-cov",
                "nextest",
                "--workspace",
                "--all-features",
                "--lib",
                "--bins",
                "--tests",
                "--profile",
                "ci",
                "--no-report",
            ],
            ctx,
        )
        .await
    })
    .await
}

/// Tracked parallel-validation step: doctests + production clippy +
/// test clippy + `cargo deny` + rustdoc link validation, all fanned
/// out with `RUSTDOCFLAGS=-Dwarnings` so rustdoc failures are hard.
async fn tracked_parallel_validation_step(
    state: &mut WorkflowState,
    ctx: &PipelineContext,
) -> Result<()> {
    execute_step_with_tracking(state, STEP_PARALLEL_VALIDATION, || async {
        let parallel_commands = vec![
            ("Documentation tests", "cargo", vec![
                "test",
                "--doc",
                "--workspace",
                "--all-features",
            ]),
            // pedantic/nursery/cargo/multiple_crate_versions levels are
            // set in workspace Cargo.toml — only per-target overrides
            // needed here.
            ("Production linting", "cargo", vec![
                "clippy",
                "--workspace",
                "--lib",
                "--bins",
                "--all-features",
                "--no-deps",
                "--",
                "-D",
                "warnings",
                "-W",
                "clippy::panic",
                "-W",
                "clippy::todo",
                "-W",
                "clippy::unimplemented",
            ]),
            ("Test linting", "cargo", vec![
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--tests",
                "--no-deps",
                "--",
                "-D",
                "warnings",
                "-W",
                "clippy::panic",
                "-W",
                "clippy::todo",
                "-W",
                "clippy::unimplemented",
                "-A",
                "clippy::unwrap_used",
                "-A",
                "clippy::expect_used",
                "-A",
                "unused-crate-dependencies",
            ]),
            ("Dependency security", "cargo", vec!["deny", "check"]),
            // `--document-private-items` validates links across the private
            // surface too — see the matching note in `phases.rs`.
            ("Rustdoc link validation", "cargo", vec![
                "doc",
                "--workspace",
                "--all-features",
                "--no-deps",
                "--document-private-items",
            ]),
        ];
        execute_parallel_with_env(parallel_commands, &[("RUSTDOCFLAGS", "-Dwarnings")], ctx).await
    })
    .await
}

/// Tracked format-check step: `cargo fmt --all -- --check` on the
/// *post-format* tree — catches any files rustfmt can't stabilise in
/// one pass (extremely rare, but worth gating before shipping).
async fn tracked_format_check_step(state: &mut WorkflowState, ctx: &PipelineContext) -> Result<()> {
    execute_step_with_tracking(state, STEP_FORMAT_CHECK, || async {
        execute_command(
            "Format check",
            "cargo",
            &["fmt", "--all", "--", "--check"],
            ctx,
        )
        .await
    })
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// Resumable Phase 1 / Phase 2
// ─────────────────────────────────────────────────────────────────────────────

/// Phase 1 of the ship pipeline: the full validation sweep (toolchain +
/// polars + clean + format + coverage tests + parallel validation +
/// format check).  Mutates `state` so every step's started / completed
/// / failed transition is persisted — a re-run after a failure picks
/// up at the first non-completed step.
///
/// Thin orchestrator: each `STEP_*` gets a named `tracked_*_step`
/// helper, so the control flow is readable and a backtrace on failure
/// points at the specific tracked step that failed.
///
/// # Errors
///
/// Propagates any failure from the wrapped tracked steps.
pub(crate) async fn run_enhanced_phase1(
    state: &mut WorkflowState,
    ctx: &PipelineContext,
) -> Result<()> {
    println!(
        "{}",
        "🧪 PHASE 1: Optimized Testing & Validation Pipeline"
            .blue()
            .bold()
    );
    println!(
        "ℹ️  Running all validation with CURRENT version - version increment happens AFTER validation passes"
    );
    println!(
        "🚀 Using safe parallel optimization: format → compile → parallel(doc tests + linting)"
    );

    // ── Tree-hash invalidation ──
    // A resumed `just ship` skips a validation step only when the code it
    // validated is byte-identical.  If the working tree changed since Phase 1
    // last passed (you edited a tracked file or committed between runs), the
    // cached "completed" flags for the code-dependent steps are invalidated so
    // they re-run on the new code.  An unchanged tree (e.g. resuming after a
    // Phase-2 push failure) keeps the fast skip.  The authoritative gate is
    // still the uncacheable PR CI; this closes the local "passed step skipped
    // after a code edit" gap.
    let fingerprint = working_tree_fingerprint();
    if state.validated_fingerprint != fingerprint {
        if !state.validated_fingerprint.is_empty() {
            println!(
                "{}",
                "↻ Working tree changed since last validation — re-running Phase 1 checks".yellow()
            );
        }
        for step in [
            STEP_FORMAT_CODE,
            STEP_COVERAGE_TESTS,
            STEP_PARALLEL_VALIDATION,
            STEP_FORMAT_CHECK,
        ] {
            state.invalidate_step(step)?;
        }
    }

    tracked_toolchain_step(state, ctx).await?;

    println!("{}", "📋 Stage 1: Sequential Prerequisites".yellow().bold());

    tracked_clean_step(state, ctx).await?;
    execute_step_with_tracking(state, STEP_FORMAT_CODE, || async {
        execute_command("Format code", "cargo", &["fmt", "--all"], ctx).await
    })
    .await?;

    // File size policy runs inline (not resumable) because it's
    // ~100 ms and a stale "completed" state could mask a new
    // violation.
    execute_command(
        "File size policy",
        "bash",
        &["scripts/ci/check_file_size_policy.sh"],
        ctx,
    )
    .await?;

    tracked_coverage_tests_step(state, ctx).await?;
    tracked_parallel_validation_step(state, ctx).await?;
    tracked_format_check_step(state, ctx).await?;

    // Record the (post-`cargo fmt`) tree this Phase-1 run validated, so a
    // resume on an unchanged tree skips the checks and any later edit re-runs
    // them.  Recomputed here (not reused from the top) because `03-format-code`
    // may have rewritten files.
    state.validated_fingerprint = working_tree_fingerprint();
    state.save()?;

    println!(
        "{}",
        "✅ PHASE 1 COMPLETE - All testing and validation passed!"
            .green()
            .bold()
    );
    Ok(())
}

/// Fingerprint of the working tree that validation actually sees: the `HEAD`
/// commit plus the full diff of tracked changes against it.  A new commit or
/// any edit to a tracked file changes the hash; a brand-new *untracked* file
/// is the one gap, and the uncacheable PR CI still covers it.
///
/// Uses `DefaultHasher` (fixed-key `SipHash` — deterministic across processes,
/// unlike `HashMap`'s randomized state); non-cryptographic but ample for
/// change detection.  On any `git` error the components default to empty, which
/// simply makes the fingerprint conservative (more likely to differ → re-run).
fn working_tree_fingerprint() -> String {
    use core::hash::{Hash as _, Hasher as _};

    fn git(args: &[&str]) -> Vec<u8> {
        std::process::Command::new("git")
            .args(args)
            .output()
            .map_or_default(|out| out.stdout)
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    git(&["rev-parse", "HEAD"]).hash(&mut hasher);
    git(&["diff", "HEAD"]).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Phase 2 of the ship pipeline: version bump + commit + push + open
/// release PR.  Same resumable-state semantics as
/// [`run_enhanced_phase1`].  The push step participates in the Phase 6
/// `invalidate_step` fix so a follow-up commit after a cached
/// "push completed" state re-runs the push rather than silently
/// skipping.
///
/// # Errors
///
/// Propagates any failure from the wrapped tracked steps or from the
/// workflow-state writes surrounding them.
pub(crate) async fn run_enhanced_phase2(
    state: &mut WorkflowState,
    ctx: &PipelineContext,
) -> Result<()> {
    println!(
        "{}",
        "📦 PHASE 2: version bump → commit → release PR"
            .blue()
            .bold()
    );

    // Version increment (Path B, 2026-06-10): restored after R5 retired it.
    // release-plz only versions the 2 publishable leaf libs — it cannot drive
    // binary releases — so the lockstep workspace bump happens HERE, at the end
    // of `just ship`, gated behind the resumable `version_incremented` flag so
    // a re-run after a mid-ship failure never double-bumps.  Default level:
    // patch.
    if !state.version_incremented {
        bump_workspace_version("patch").context("Failed to bump workspace version")?;
        let new_version = get_current_version().context("Failed to read bumped version")?;
        println!("🔖 Bumped workspace version → v{new_version}");
        // Roll CHANGELOG `## [Unreleased]` into the dated `## [vX.Y.Z]` section
        // BEFORE the commit so it lands in the same release commit (`git add .`
        // in `git_commit` stages it).  This is what stops the changelog from
        // drifting — every ship records its release notes (see PR #490 / the
        // `changelog` module).  Idempotent: a resumed ship after this point is
        // gated by `version_incremented`, and the roll itself is a no-op on an
        // already-rolled file.
        roll_changelog_file(&new_version).context("Failed to roll CHANGELOG [Unreleased]")?;
        // CITATION.cff names the release too (same commit, same date as the
        // changelog roll).
        let release_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        sync_citation_file(&new_version, &release_date)
            .context("Failed to sync CITATION.cff to the new version")?;
        state.version_incremented = true;
        state.current_version = new_version;
        state.save()?;
    }

    // Step 10: Git commit (signed commit on the working branch).
    execute_step_with_tracking(state, STEP_GIT_COMMIT, || async { git_commit(ctx).await }).await?;

    // Step 11: Git push -- opens release/vX.Y.Z PR with auto-merge
    // queued.
    //
    // Binaries are NOT built here.  Once the PR merges to main,
    // release-plz creates the tag and dispatches `release.yml`,
    // which produces the reproducible cross-platform binaries on
    // GitHub-hosted runners.
    //
    // Phase 6 resumable-push fix (docs/architecture/dev-flow.md §
    // 5.1 / dev-flow-implementation-plan.md § 6.3): if the developer
    // committed locally since the previous ship run (e.g. to fix a
    // CI-detected audit failure), HEAD will be ahead of the already-
    // pushed release branch on origin.  The cached "completed" state
    // from the prior run would otherwise silently skip this step and
    // the new commits would never land.  Detect that condition and
    // invalidate the cached state so the step re-runs.
    let release_branch_peek = format!("release/v{}", get_current_version()?);
    let unpushed = count_unpushed_commits(&release_branch_peek).await?;
    if unpushed > 0 && state.is_step_completed(STEP_GIT_PUSH) {
        println!(
            "↻ {} unpushed commit(s) on HEAD vs origin/{} — re-running step {}",
            unpushed.to_string().yellow(),
            release_branch_peek.cyan(),
            STEP_GIT_PUSH.cyan(),
        );
        state.invalidate_step(STEP_GIT_PUSH)?;
    }
    execute_step_with_tracking(state, STEP_GIT_PUSH, || async { git_push(ctx).await }).await?;

    println!(
        "{}",
        "✅ PHASE 2 COMPLETE - Release PR opened; GitHub Actions will produce binaries."
            .green()
            .bold()
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Ship-entry helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Load the on-disk workflow state for a ship run, resetting it to a
/// fresh default when `--fresh` is in effect.  Non-fresh runs that
/// fail to parse the state file fall through to a fresh default so a
/// corrupt state can't strand the pipeline.
///
/// # Errors
///
/// Returns an error if `--fresh` saves cannot persist the reset state
/// to disk.
fn load_or_reset_ship_state(ctx: &PipelineContext) -> Result<WorkflowState> {
    if ctx.flags.fresh {
        println!(
            "{} Fresh run requested - resetting workflow state",
            "🔄".yellow()
        );
        let current_version = get_current_version().unwrap_or_else(|_| "unknown".to_owned());
        let new_state = WorkflowState::new_workflow(current_version);
        new_state
            .save()
            .context("Failed to save fresh workflow state")?;
        Ok(new_state)
    } else {
        let loaded = WorkflowState::load().ok();
        // A COMPLETED cycle must not be "resumed": its steps are all marked
        // done and `version_incremented` is set, so resuming would skip both
        // Phase 1 validation AND the Phase 2 version bump — the next release
        // would commit nothing.  `is_resumable()` already excludes Completed,
        // so a finished state starts a fresh cycle.  (`--fresh` is still the
        // explicit reset; this just stops a stale terminal state from wedging
        // the next `just ship`.)
        match loaded {
            Some(state) if state.phase != WorkflowPhase::Completed => Ok(state),
            _ => {
                let current_version =
                    get_current_version().unwrap_or_else(|_| "unknown".to_owned());
                Ok(WorkflowState::new_workflow(current_version))
            }
        }
    }
}

/// Print the resume banner: how many steps are already completed,
/// which ones, and which are still pending.  No-op on `--fresh` runs
/// and on the very first ship attempt (nothing to resume).
fn print_ship_resume_banner(state: &WorkflowState, ctx: &PipelineContext) {
    let completed_count = state.step_tracker.completed_steps.len();
    if completed_count == 0 || ctx.flags.fresh {
        return;
    }
    let total_steps = ALL_STEPS.len();
    let pending_steps = state.get_pending_steps(ALL_STEPS);

    println!(
        "{} Resuming from previous run: {}/{} steps completed",
        "📋".cyan(),
        completed_count,
        total_steps
    );
    if !state.step_tracker.completed_steps.is_empty() {
        println!("   Already completed:");
        for step in &state.step_tracker.completed_steps {
            println!("     {} {}", "✓".green(), step);
        }
    }
    if !pending_steps.is_empty() {
        println!("   Remaining:");
        for step in &pending_steps {
            println!("     {} {}", "○".dimmed(), step);
        }
    }
    println!();
}

/// Print the post-run summary: version, total wall time, completed
/// step count, and per-step timing breakdown.
fn print_ship_summary(state: &WorkflowState, total_time: Duration) {
    let total_steps = ALL_STEPS.len();
    println!();
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════════".green()
    );
    println!("{} Ship Pipeline Complete!", "🎉".green().bold());
    println!("═══════════════════════════════════════════════════════════════════");
    println!("   Version:    {}", state.current_version.cyan());
    println!("   Total time: {}s", total_time.as_secs());
    println!(
        "   Steps:      {}/{} completed",
        state.step_tracker.completed_steps.len(),
        total_steps
    );

    if !state.step_durations_secs.is_empty() {
        println!();
        println!("   Step Timings:");
        for (step, secs) in &state.step_durations_secs {
            let mins = secs / 60;
            let remaining_secs = secs % 60;
            if mins > 0 {
                println!("     {step} {mins}m {remaining_secs}s");
            } else {
                println!("     {step} {secs}s");
            }
        }
    }
}

/// Print sccache `-s` stats when both sccache was enabled and the user
/// asked for verbose output.  Returns silently in any other case.
pub(crate) async fn print_sccache_stats_if_verbose(ctx: &PipelineContext) {
    if ctx.flags.sccache_enabled
        && let Ok(out) = Command::new("sccache").arg("-s").output().await
        && ctx.flags.verbose
    {
        println!(
            "{} sccache stats:\n{}",
            "⚡".green(),
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Print the terminal "what GitHub does next" hint block that points
/// the developer at the release-workflow run list.
fn print_ship_next_steps(state: &WorkflowState) {
    println!();
    println!("{} Release PR opened; auto-merge queued", "📤".green());
    println!(
        "{} GitHub Actions will build + publish v{} on merge",
        "👷".green(),
        state.current_version
    );
    println!(
        "   💡 Watch: {}",
        "gh run list --repo <owner>/<repo> --workflow=release.yml --limit 5".cyan()
    );
}

/// Print a horizontal separator used between the two phases of the
/// ship pipeline, for visual scannability in the terminal output.
fn print_ship_phase_separator() {
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
}

/// Combined ship pipeline: Phase 1 (validation) + Phase 2 (deploy).
/// Supports resumable execution — re-runs skip already-completed
/// steps.  Pass `--fresh` on the CLI to reset state and start over.
///
/// Thin orchestrator: state hydration, progress banner, phase calls,
/// summary, sccache stats, and next-steps hint each live in their own
/// named helper so the ship entry point reads as a single recipe.
///
/// # Errors
///
/// Propagates any failure from the two phase functions or from the
/// workflow-state writes surrounding them.
pub(crate) async fn run_ship_pipeline(ctx: &PipelineContext) -> Result<()> {
    println!(
        "{}",
        "🚢 UFFS Ship Pipeline (Phase 1 + Phase 2, Resumable)"
            .blue()
            .bold()
    );
    println!("═══════════════════════════════════════════════════════════════════");

    let mut state = load_or_reset_ship_state(ctx)?;
    print_ship_resume_banner(&state, ctx);

    print_ship_phase_separator();
    run_enhanced_phase1(&mut state, ctx)
        .await
        .context("Phase 1 (validation) failed")?;

    print_ship_phase_separator();
    run_enhanced_phase2(&mut state, ctx)
        .await
        .context("Phase 2 (release PR) failed")?;

    state.advance_phase(WorkflowPhase::Completed)?;

    let total_time = ctx.start_time.elapsed();
    print_ship_summary(&state, total_time);
    print_sccache_stats_if_verbose(ctx).await;
    print_ship_next_steps(&state);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CleanDecision, CleanMode, decide_clean, working_tree_fingerprint};

    #[test]
    fn tree_fingerprint_is_deterministic_and_nonempty() {
        // Two back-to-back calls observe the same working tree, so they MUST
        // agree — if the fingerprint were unstable, every `just ship` would
        // needlessly re-run all of Phase 1.  16 hex chars (u64) is the shape.
        let first = working_tree_fingerprint();
        let second = working_tree_fingerprint();
        assert_eq!(
            first, second,
            "fingerprint must be stable for an unchanged tree"
        );
        assert_eq!(first.len(), 16, "fingerprint is the 16-hex-char u64 hash");
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn force_mode_always_cleans() {
        assert!(matches!(
            decide_clean(CleanMode::Force, false, false),
            CleanDecision::Clean(_)
        ));
    }

    #[test]
    fn never_mode_suppresses_toolchain_change() {
        assert!(matches!(
            decide_clean(CleanMode::Never, true, true),
            CleanDecision::SuppressedToolchainChange
        ));
    }

    #[test]
    fn never_mode_without_change_skips() {
        assert!(matches!(
            decide_clean(CleanMode::Never, false, true),
            CleanDecision::Skip
        ));
    }

    #[test]
    fn auto_mode_toolchain_change_forces_clean() {
        assert!(matches!(
            decide_clean(CleanMode::Auto, true, false),
            CleanDecision::Clean(_)
        ));
    }

    #[test]
    fn auto_mode_disk_pressure_cleans() {
        assert!(matches!(
            decide_clean(CleanMode::Auto, false, true),
            CleanDecision::Clean(_)
        ));
    }

    #[test]
    fn auto_mode_toolchain_change_takes_precedence_over_disk() {
        assert!(matches!(
            decide_clean(CleanMode::Auto, true, true),
            CleanDecision::Clean(_)
        ));
    }

    #[test]
    fn auto_mode_nothing_to_do_skips() {
        assert!(matches!(
            decide_clean(CleanMode::Auto, false, false),
            CleanDecision::Skip
        ));
    }
}
