// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

#![expect(
    clippy::print_stdout,
    reason = "operational CLI tool — git/gh progress lines go to stdout (issue #212)"
)]

//! `git` + `gh` CLI helpers that Phase 2 of the ship pipeline drives.
//!
//! The entry points are [`git_commit`] (create the `chore: development
//! vX.Y.Z ...` signed commit) and [`git_push`] (push the release
//! branch + open the release PR and enroll it in `main`'s merge
//! queue).  Each of the push sub-steps (`ensure_on_release_branch`,
//! `merge_upstream`, `push_release_branch`, `find_existing_release_pr`,
//! `open_release_pr`, `enable_auto_merge`, `return_to_base_branch`) is
//! its own small helper so a failure points at a named function in the
//! backtrace and refactors stay surgical.
//!
//! The auto-commit is created on the `release/vX.Y.Z` branch (see
//! [`git_commit`] / `switch_to_release_branch`), never on `main`, so a
//! ship leaves local `main` exactly at `origin/main` — no post-merge
//! `git reset --hard` is ever needed.
//!
//! [`count_unpushed_commits`] is the Phase 6 (resumable-push-fix)
//! helper: it lets the ship pipeline detect "HEAD is ahead of
//! `origin/release/<ver>`" and re-run the cached-completed push step
//! instead of silently skipping it.

use anyhow::{Context as _, Result};
use colored::Colorize as _;
use tokio::process::Command;

use crate::context::PipelineContext;
use crate::exec::execute_command;
use crate::version::extract_version_from_cargo_toml;

/// Name of the long-lived base branch the release PR targets.  The
/// ship pipeline never commits onto it locally (see [`git_commit`]); it
/// is only ever the PR base and the branch the developer is left on
/// after a ship.
pub(crate) const BASE_BRANCH: &str = "main";

/// Switch onto the `release/vX.Y.Z` branch (creating or resetting it to
/// the current `HEAD`) so the auto-commit lands there instead of on
/// `main`.  Idempotent: safe to re-run on a resumed ship even if the
/// branch already exists.
///
/// Why this matters: committing on `main` left local `main` permanently
/// 1-ahead of `origin/main`, and once the PR merged the local commit
/// diverged from what GitHub recorded on `main`, forcing a
/// `git reset --hard origin/main` after every ship.  Committing on the
/// release branch keeps local `main` exactly at `origin/main`
/// throughout, so a plain `git pull --ff-only` syncs it post-merge.
///
/// `git switch -C` (capital C) creates-or-resets the branch to HEAD,
/// which is exactly the resumable semantics we want: first run creates
/// it at the pre-commit HEAD; a resumed run that is already on the
/// branch (with the commit) re-points it to the same HEAD as a no-op.
///
/// # Errors
///
/// Propagates any failure from the `git switch` subprocess.
async fn switch_to_release_branch(ctx: &PipelineContext, release_branch: &str) -> Result<()> {
    println!(
        "🌿 Switching to release branch {} (commit lands here, not on {})",
        release_branch.cyan(),
        BASE_BRANCH.cyan()
    );
    execute_command(
        "Git switch (release branch)",
        "git",
        &["switch", "-C", release_branch],
        ctx,
    )
    .await
}

/// Create the auto-generated `chore: development vX.Y.Z ... [auto-commit]`
/// commit **on the release branch** (never on `main`).  Commit message
/// shape is stable so the release PR template can parse it.
///
/// Switches onto `release/vX.Y.Z` first (via [`switch_to_release_branch`])
/// so `main` is never mutated by the ship — see that helper's docs for
/// the local-`main`-divergence rationale.
///
/// # Errors
///
/// Propagates any failure from the branch switch, the wrapped `git add`,
/// the Cargo.toml read, or the `git commit` subprocess.
pub(crate) async fn git_commit(ctx: &PipelineContext) -> Result<()> {
    println!("{}", "📝 Creating auto-generated commit...".blue());

    let cargo_toml = std::fs::read_to_string("Cargo.toml").context("Failed to read Cargo.toml")?;
    let version = extract_version_from_cargo_toml(&cargo_toml)?;
    let release_branch = format!("release/v{version}");

    // Land the commit on the release branch, not on `main`.
    switch_to_release_branch(ctx, &release_branch).await?;

    execute_command("Git add", "git", &["add", "."], ctx).await?;
    let commit_message =
        format!("chore: development v{version} - comprehensive testing complete [auto-commit]");
    execute_command("Git commit", "git", &["commit", "-m", &commit_message], ctx).await?;
    Ok(())
}

/// Count commits on the local HEAD that have not yet landed on
/// `origin/<remote_branch>`.
///
/// Phase 6 helper (dev-flow-implementation-plan.md § 6.3) used by the
/// ship pipeline to decide whether a previously-completed
/// `STEP_GIT_PUSH` needs to be re-run.  After a push succeeds the step
/// is cached; if the developer then commits more locally (e.g. to fix
/// a CI-detected audit failure) and re-runs `just ship`, the cached
/// "completed" state would silently skip the push and the new commits
/// would never land.  Counting `origin/<branch>..HEAD` reliably
/// detects that case.
///
/// Special cases:
/// * If the remote ref does not yet exist (first push of a new release branch),
///   `git rev-list` fails — we treat that as "1 unpushed commit" so the push
///   runs.
/// * If HEAD equals the remote ref, the count is 0 and the cached completion is
///   honoured (idempotent re-runs stay cheap).
///
/// # Errors
///
/// Returns an error only when the `git rev-list` subprocess itself
/// cannot be spawned.  A non-zero exit status is treated as the "ref
/// missing" special case and folded into `Ok(1)`.
pub(crate) async fn count_unpushed_commits(remote_branch: &str) -> Result<u64> {
    let remote_ref = format!("origin/{remote_branch}");
    let spec = format!("{remote_ref}..HEAD");
    let out = Command::new("git")
        .args(["rev-list", "--count", &spec])
        .output()
        .await
        .with_context(|| format!("Failed to run git rev-list for {spec}"))?;
    if !out.status.success() {
        // Remote ref doesn't exist yet (first push) — be conservative
        // and treat HEAD as ahead so the push always runs.
        return Ok(1);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.trim().parse::<u64>().unwrap_or(1))
}

// ─────────────────────────────────────────────────────────────────────────────
// `git_push` sub-steps
// ─────────────────────────────────────────────────────────────────────────────

/// Ensure the working tree is on the existing `release_branch` before
/// pushing — without resetting it.
///
/// Uses plain `git switch <branch>` (NOT `-C`): on a resumed ship where
/// `git_commit` was cached-skipped, the release branch already exists
/// with the auto-commit and we must land on *that* commit, not reset the
/// branch to a commit-less `HEAD`.  On a fresh run we are already on the
/// release branch (just created by `git_commit`), so the switch is a
/// no-op.  `git switch` to the already-current branch succeeds silently.
///
/// # Errors
///
/// Propagates any failure from the `git switch` subprocess (e.g. the
/// branch does not exist — which would indicate `git_commit` never ran).
async fn ensure_on_release_branch(ctx: &PipelineContext, release_branch: &str) -> Result<()> {
    execute_command(
        "Git switch (ensure release branch)",
        "git",
        &["switch", release_branch],
        ctx,
    )
    .await
}

/// Merge `origin/<BASE_BRANCH>` into the currently-checked-out release
/// branch so the PR is based on current mainline before it opens.
///
/// A MERGE, never a rebase: the branch's commits are signed, and a PR
/// lands on `main` as a merge commit with those commits kept one by
/// one.  A rebase would rewrite (and drop the signatures of) every
/// commit and flatten any merge already on the branch.  When the branch
/// already contains `origin/main` this is a no-op.
///
/// # Errors
///
/// Propagates any failure from the wrapped `git pull --no-rebase`
/// subprocess (network, merge conflicts, ...).
async fn merge_upstream(ctx: &PipelineContext) -> Result<()> {
    execute_command(
        "Git pull merge",
        "git",
        &["pull", "--no-rebase", "--no-edit", "origin", BASE_BRANCH],
        ctx,
    )
    .await
}

/// Push `HEAD` to `refs/heads/<release_branch>` on `origin`.  No-op /
/// fast-forward when the pipeline is resuming after a previously-
/// failed step 11 that already pushed the same commit.
///
/// # Errors
///
/// Propagates any failure from the wrapped `git push` subprocess.
async fn push_release_branch(ctx: &PipelineContext, release_branch: &str) -> Result<()> {
    let push_ref = format!("HEAD:refs/heads/{release_branch}");
    println!("📤 Pushing HEAD to {}", release_branch.cyan());
    execute_command(
        "Git push (release branch)",
        "git",
        &["push", "origin", &push_ref],
        ctx,
    )
    .await
}

/// Return the PR number of an already-open PR for `release_branch`, or
/// `None` if no open PR exists.  Used by [`git_push`] to keep the
/// PR-creation step idempotent across resumed ship runs.
///
/// # Errors
///
/// Propagates any failure from the wrapped `gh pr list` subprocess.
fn find_existing_release_pr(release_branch: &str) -> Result<Option<String>> {
    let existing_pr_output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            release_branch,
            "--state",
            "open",
            "--json",
            "number",
            "-q",
            ".[0].number",
        ])
        .output()
        .context("Failed to query existing release PR via gh")?;
    let trimmed = String::from_utf8_lossy(&existing_pr_output.stdout)
        .trim()
        .to_owned();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}

/// Open a new release PR via `gh pr create`, with the branch-
/// protection-compatible title + body that explains how it lands (the
/// merge queue, a merge commit) and what the merge triggers.
///
/// # Errors
///
/// Propagates any failure from the wrapped `gh pr create` subprocess.
async fn open_release_pr(
    ctx: &PipelineContext,
    base_branch: &str,
    release_branch: &str,
    version: &str,
) -> Result<()> {
    let pr_title = format!("chore: release v{version} — ship pipeline auto-commit");
    let pr_body = format!(
        "## Summary\n\n\
         `just ship` Phase 2 auto-commit for **v{version}** — the \
         `[workspace.package].version` bump in `Cargo.toml`.\n\n\
         ## Merge\n\n\
         The PR is enrolled in `{base_branch}`'s merge queue \
         (`gh pr merge --auto`, the same call as `just merge <PR>`).  \
         The queue merges it once the required checks pass, with a \
         merge commit: the branch's signed commits land one by one, \
         and GitHub signs the merge commit it creates, so \
         `required_signatures: true` holds.  Never squash, never \
         rebase.\n\n\
         ## After merge\n\n\
         `release-auto-trigger.yml` sees the merged `{release_branch}` \
         and dispatches `release.yml`, which builds the binaries and \
         publishes GitHub Release v{version} (`just release-tag` is the \
         manual fallback).  The auto-commit lived only on \
         `{release_branch}`, so local `{base_branch}` never drifted — \
         sync it with `git pull --ff-only origin {base_branch}`."
    );

    println!("📬 Opening release PR");
    execute_command(
        "Open release PR",
        "gh",
        &[
            "pr",
            "create",
            "--base",
            base_branch,
            "--head",
            release_branch,
            "--title",
            &pr_title,
            "--body",
            &pr_body,
        ],
        ctx,
    )
    .await
}

/// Enroll the release PR in `main`'s merge queue (`gh pr merge --auto`
/// with NO method flag, exactly what `just merge <PR>` runs).
///
/// The queue owns the merge method, and `main-protection` sets it to
/// MERGE: the release commit, signed by its author, lands on `main`
/// unchanged, and GitHub signs the merge commit it creates, so
/// `required_signatures: true` holds for both.  Passing `--squash` (the
/// old call) is refused by a queue-governed branch and squashes the
/// history this project keeps; rebase cannot be signed by GitHub at all.
/// The queue only merges once the required checks pass on the merge
/// group, so enrolling early never lands an unchecked PR.
///
/// # Errors
///
/// Propagates any failure from the wrapped `gh pr merge` subprocess.
async fn enable_auto_merge(ctx: &PipelineContext, release_branch: &str) -> Result<()> {
    println!("⚡ Enrolling the release PR in the merge queue (merge commit)");
    execute_command(
        "Enable auto-merge",
        "gh",
        &["pr", "merge", release_branch, "--auto"],
        ctx,
    )
    .await
}

/// Push the release branch and open the release PR against
/// [`BASE_BRANCH`].  Branch-protection compatible: never pushes
/// directly to `main`.
///
/// Thin orchestrator — each sub-step lives in its own helper
/// ([`ensure_on_release_branch`], [`merge_upstream`],
/// [`push_release_branch`], [`find_existing_release_pr`],
/// [`open_release_pr`], [`enable_auto_merge`], [`return_to_base_branch`])
/// so the control flow stays readable and individual failures map 1:1
/// to a named helper in the backtrace.
///
/// # Errors
///
/// Propagates any failure from the helper subprocesses.  The version
/// lookup via [`crate::version::get_current_version`] is the one
/// non-subprocess error source.
pub(crate) async fn git_push(ctx: &PipelineContext) -> Result<()> {
    println!(
        "{}",
        "🚀 Opening release PR (branch-protection-compatible)...".blue()
    );

    // The auto-commit was made on the release branch by `git_commit`, so
    // that is the branch checked out here.  Derive its name from the
    // version Phase 2 step 07 bumped (example: `release/v0.5.68`).
    let version = crate::version::get_current_version()?;
    let release_branch = format!("release/v{version}");
    println!("📌 Release branch: {}", release_branch.cyan());

    // Resilience for resumed ships: if step 10 (git_commit) was
    // cached-complete this run, we were NOT switched onto the release
    // branch — and a prior git_push may have already switched us back to
    // `main`.  Re-checkout the existing release branch (plain `switch`,
    // NOT `-C`, so we land on the branch that already holds the
    // auto-commit rather than resetting it to a commit-less HEAD).
    ensure_on_release_branch(ctx, &release_branch).await?;

    // Merge current mainline into the release branch before opening the
    // PR (keeps the PR based on the latest `origin/main`; never a rebase).
    merge_upstream(ctx).await?;

    push_release_branch(ctx, &release_branch).await?;

    // Idempotent PR creation: reuse an existing open PR for the same
    // release branch if the pipeline is resuming from a previously-
    // failed step 11.  Base is always `main` — the ship never targets
    // any other branch.
    match find_existing_release_pr(&release_branch)? {
        Some(pr_number) => {
            println!("ℹ️  Reusing existing release PR #{}", pr_number.cyan());
        }
        None => {
            open_release_pr(ctx, BASE_BRANCH, &release_branch, &version).await?;
        }
    }

    enable_auto_merge(ctx, &release_branch).await?;

    // Leave the developer back on `main`, untouched and exactly at
    // `origin/main`.  Because the commit only ever lived on the release
    // branch, local `main` never drifted — after the PR merges,
    // a plain `git pull --ff-only` syncs it (no `reset --hard` needed).
    return_to_base_branch(ctx).await?;

    println!(
        "{} Release PR for v{} opened with auto-merge queued",
        "✅".green(),
        version
    );
    println!(
        "   💡 Watch checks: {}",
        format!("gh pr checks {release_branch} --watch").cyan()
    );
    println!(
        "   💡 After merge:  {}",
        format!("git pull --ff-only origin {BASE_BRANCH} (local {BASE_BRANCH} never drifted)")
            .cyan()
    );

    Ok(())
}

/// Switch the working tree back to [`BASE_BRANCH`] after the release PR
/// has been opened, so the developer is left where they started.
///
/// Best-effort: a failure here does not undo the already-opened PR, so
/// it is logged but not fatal — the ship's real deliverable (the PR +
/// queued auto-merge) is already done by the time this runs.
///
/// # Errors
///
/// Never returns `Err` — a failed switch is downgraded to a warning so
/// it cannot fail an otherwise-successful ship.
async fn return_to_base_branch(ctx: &PipelineContext) -> Result<()> {
    println!("🔙 Returning to {}", BASE_BRANCH.cyan());
    if let Err(err) =
        execute_command("Git switch (base)", "git", &["switch", BASE_BRANCH], ctx).await
    {
        println!(
            "{} could not switch back to {BASE_BRANCH} ({err}); you are still on the release branch",
            "⚠️".yellow()
        );
    }
    Ok(())
}
