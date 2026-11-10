# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.

# Phase 1: safe-by-default validation.
phase1-test:
    @printf "\033[0;34m🧪 PHASE 1: Safe-by-Default Validation\033[0m\n"
    @printf "\033[1;33mNo version bump, deploy, commit, or push in this lane.\033[0m\n"
    @echo "========================================================"
    @echo ""
    @printf "\033[0;34mStep 1: Compile validation without applying fixes...\033[0m\n"
    @printf "\033[0;36m  → cargo check --workspace --all-targets --all-features\033[0m\n"
    cargo check --workspace --all-targets --all-features
    @printf "\033[0;34mStep 1b: File size policy (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → bash scripts/ci/check_file_size_policy.sh\033[0m\n"
    bash scripts/ci/check_file_size_policy.sh
    @printf "\033[0;34mStep 2: Running workspace tests (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → cargo nextest run --workspace --all-features --lib --tests\033[0m\n"
    cargo nextest run --workspace --all-features --lib --tests
    @printf "\033[0;32m✅ All workspace tests passed\033[0m\n"
    @printf "\033[0;34mStep 3: Documentation tests validation (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → just test-doc\033[0m\n"
    just test-doc
    @printf "\033[0;34mStep 3b: Rustdoc link validation (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → just rustdoc\033[0m\n"
    just rustdoc
    @printf "\033[0;34mStep 4: Ultra-strict production code linting (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → just lint-prod\033[0m\n"
    just lint-prod
    @printf "\033[0;34mStep 5: Pragmatic test code linting (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → just lint-tests\033[0m\n"
    just lint-tests
    @printf "\033[0;34mStep 6: Dependency security validation (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → cargo deny check\033[0m\n"
    cargo deny check
    @printf "\033[0;34mStep 7: CI lint gating (FAST-FAIL)...\033[0m\n"
    @printf "\033[0;36m  → just lint-ci\033[0m\n"
    just lint-ci
    @echo ""
    @printf "\033[0;32m✅ PHASE 1 COMPLETE: Validation passed with no release-side effects.\033[0m\n"
    @printf "\033[0;36m📝 Note: Linux-specific code paths (cfg target_os = linux) are validated by CI on push.\033[0m\n"
    @printf "\033[0;34m💡 Next: Run 'just phase2-ship' only when ready to commit/push (version via release-plz)\033[0m\n"

# Phase 2: commit and push (version bump retired — release-plz handles it).
phase2-ship:
    @printf "\033[0;34m🚀 PHASE 2: Explicit Ship Lane\033[0m\n"
    @printf "\033[1;33mThis lane performs commit and push. Version bumping now handled by release-plz on main.\033[0m\n"
    @echo "========================================================"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- phase2

# Backward compatibility alias for phase2-ship.
phase2-commit: phase2-ship

# Safe-by-default validation workflow (Rust optimized).
# Lints macOS code paths locally; Linux paths validated by CI on push.
go:
    @printf "\033[0;34m🚀 Safe-by-Default Validation Workflow (RUST OPTIMIZED)\033[0m\n"
    @printf "\033[1;33mNo version bump, deploy, commit, or push in this default lane.\033[0m\n"
    @printf "\033[0;36m📝 Lints macOS code paths — Linux cfg() paths validated by CI on push.\033[0m\n"
    @echo "========================================================"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- go

# Full ship pipeline: Phase 1 validation + Phase 2 deploy (resumable).
# Re-runs automatically skip already-completed steps.
# Lints macOS code paths locally; Linux paths validated by CI on push.
# Pass flags like: just ship -v, just ship --coverage-report
ship *ARGS:
    @printf "\033[0;34m🚢 Full Ship Pipeline (Phase 1 + Phase 2, Resumable)\033[0m\n"
    @printf "\033[1;33mValidates and ships. Re-runs skip completed steps.\033[0m\n"
    @printf "\033[0;36m📝 Lints macOS code paths — Linux cfg() paths validated by CI on push.\033[0m\n"
    @echo "========================================================"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- ship {{ ARGS }}

# Full ship pipeline from scratch (ignores previous progress).
# Pass flags like: just ship-fresh -v
ship-fresh *ARGS:
    @printf "\033[0;34m🚢 Full Ship Pipeline (Fresh Start)\033[0m\n"
    @printf "\033[1;33mIgnoring previous progress, starting from scratch.\033[0m\n"
    @echo "========================================================"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- ship --fresh {{ ARGS }}

# Draft CHANGELOG [Unreleased] from the Conventional-Commit subjects since the
# last release tag — a placeholder to polish before shipping. Never overwrites
# hand-authored notes (the ship flow also auto-drafts if [Unreleased] is empty).
changelog-draft:
    @printf "\033[0;34m📝 Drafting CHANGELOG [Unreleased] from commits since the last release\033[0m\n"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- changelog-draft

# Safe-by-default validation workflow (justfile fallback).
go-justfile:
    #!/usr/bin/env bash
    printf "\033[0;34m🚀 Safe-by-Default Validation Workflow (JUSTFILE)\033[0m\n"
    printf "\033[1;33mThis fallback validates only; ship actions stay in 'just phase2-ship'.\033[0m\n"
    echo "========================================================"
    echo ""
    START_TIME=$(date +%s)
    printf "\033[0;36m⏱️  Workflow started at: $(date +%H:%M:%S)\033[0m\n"
    echo ""
    printf "\033[0;34m🧪 PHASE 1: Comprehensive Fast-Fail Testing & Validation\033[0m\n"
    just phase1-test
    echo ""
    printf "\033[0;32m✅ PHASE 1 COMPLETE - All validation passed!\033[0m\n"
    END_TIME=$(date +%s)
    DURATION=$((END_TIME - START_TIME))
    echo ""
    printf "\033[0;32m🎉 SAFE VALIDATION WORKFLOW FINISHED!\033[0m\n"
    printf "\033[0;32m✅ Validation completed without release-side effects\033[0m\n"
    printf "\033[0;34m💡 Run 'just phase2-ship' only when ready to ship\033[0m\n"
    printf "\033[0;36m⏱️  Total execution time: ${DURATION}s\033[0m\n"

# ─────────────────────────────────────────────────────────────────────
# Signed-merge flow helpers (see scripts/ci/check_commit_signatures.sh)
# ─────────────────────────────────────────────────────────────────────

# Merge a PR through main's merge queue (the queue-correct incantation).
#
# `main` uses a GitHub merge queue, so `gh pr merge <PR> --squash` (or
# any method flag) is rejected ("the merge strategy for main is set by
# the merge queue") and does NOT enroll the PR.  This wrapper uses the
# right form — `gh pr merge <PR> --auto` with NO method flag — so the PR
# enters the queue and merges once its checks pass.  The queue's method
# is MERGE (`main-protection`): the PR lands as a merge commit with the
# branch's signed commits kept one by one.  Never squash, never rebase.
merge PR:
    @printf "\033[0;34m🔀 Enrolling PR #{{ PR }} into main's merge queue...\033[0m\n"
    gh pr merge {{ PR }} --auto
    @printf "\033[0;32m✅ PR #{{ PR }} queued — it merges when checks pass (watch: gh pr view {{ PR }}).\033[0m\n"

# Self-heal: re-sign every commit on the current branch, then re-push.
#
# `main`'s ruleset requires every commit to carry a verified signature.
# If one slipped through unsigned (e.g. committed with
# `-c commit.gpgsign=false`), the `commit-signatures` pre-push gate blocks
# the push.  This rebases the branch in place, re-signing each commit with
# your configured GPG key, then force-pushes with lease.  `--rebase-merges`
# keeps every merge the branch carries (a plain rebase would flatten them,
# and PRs land as merge commits with their history kept).  `--amend` leaves
# the staged set empty so the per-commit pre-commit hook no-ops; the heavy
# gates run once on the final push.
sign-branch:
    #!/usr/bin/env bash
    set -euo pipefail
    branch=$(git rev-parse --abbrev-ref HEAD)
    if [[ "$branch" == "main" ]]; then
        printf "\033[0;31m❌ refusing to rewrite main — run this on a feature branch.\033[0m\n" >&2
        exit 1
    fi
    base=$(git merge-base origin/main HEAD)
    n=$(git rev-list --count "$base"..HEAD)
    printf "\033[0;34m🔏 Re-signing %s commit(s) on '%s' (base %s)...\033[0m\n" "$n" "$branch" "${base:0:10}"
    git rebase --rebase-merges --exec 'git commit --amend --no-edit -S' "$base"
    printf "\033[0;34m⬆️  Force-pushing (with lease)...\033[0m\n"
    git push --force-with-lease
    printf "\033[0;32m✅ Branch re-signed and pushed — every commit now carries a signature.\033[0m\n"

# Verify commit signing is set up so commits can land on main.
#
# Checks, in order: commit.gpgsign is on; a signing key is configured; GPG
# can actually produce a signature with it; and the key is registered on
# GitHub (so commits render as "verified").  Each is a soft check with a
# clear fix; exits non-zero if any fails.
doctor-signing:
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    ok()   { printf "  \033[0;32m✅ %s\033[0m\n" "$1"; }
    bad()  { printf "  \033[0;31m❌ %s\033[0m\n" "$1"; fail=1; }
    printf "\033[0;34m🩺 Commit-signing doctor\033[0m\n"

    if [[ "$(git config --get commit.gpgsign || echo false)" == "true" ]]; then
        ok "commit.gpgsign = true"
    else
        bad "commit.gpgsign is not true — run: git config commit.gpgsign true"
    fi

    key=$(git config --get user.signingkey || true)
    if [[ -n "$key" ]]; then
        ok "user.signingkey = $key"
    else
        bad "no user.signingkey — set it: git config user.signingkey <KEYID>"
    fi

    if printf 'test' | gpg ${key:+--local-user "$key"} --clearsign >/dev/null 2>&1; then
        ok "GPG can sign with the configured key"
    else
        bad "GPG cannot sign (broken agent / locked key) — check 'gpg --list-secret-keys'"
    fi

    # GitHub-registration check is ADVISORY: listing GPG keys needs the
    # `admin:gpg_key` token scope, which the default `gh` auth lacks (404).
    # The authoritative proof is GitHub verifying the actual commit at push
    # time, so a missing scope is an info note, never a hard fail.
    info() { printf "  \033[0;36mℹ️  %s\033[0m\n" "$1"; }
    if [[ -n "$key" ]]; then
        # Branch on gh's exit status, not its output: on a 404 (missing
        # scope) gh prints error JSON that would otherwise leak into the
        # match and read as a false "key not found".
        if keys=$(gh api user/gpg_keys --jq '.[].key_id, .[].subkeys[]?.key_id' 2>/dev/null); then
            if printf '%s\n' "$keys" | grep -qiF "${key: -16}"; then
                ok "signing key is registered on GitHub (commits will verify)"
            else
                info "configured key not found among your GitHub GPG keys — if commits show 'Unverified', add it: https://github.com/settings/keys"
            fi
        else
            info "could not list GitHub GPG keys (gh token lacks admin:gpg_key scope) — skipped; GitHub still verifies signatures at push time"
        fi
    fi

    if (( fail )); then
        printf "\033[0;31m❌ signing setup incomplete — commits may be rejected by main's ruleset.\033[0m\n" >&2
        exit 1
    fi
    printf "\033[0;32m✅ local signing is set up — commits will be signed.\033[0m\n"

# Comprehensive validation (Rust optimized).
check-all:
    @printf "\033[0;34m📋 Comprehensive Validation (RUST OPTIMIZED)\033[0m\n"
    @printf "\033[1;33mUsing high-performance Rust script with parallel execution...\033[0m\n"
    @echo "========================================================"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- check-all

# Generate coverage report (Rust optimized).
coverage-report:
    @printf "\033[0;34m📊 Coverage Report Generation (RUST OPTIMIZED)\033[0m\n"
    @printf "\033[1;33mSmart detection: generates from existing data or runs tests if needed...\033[0m\n"
    @echo "========================================================"
    cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- coverage-report

# Watch the latest release workflow run.
release-watch:
    @printf "\033[0;34m👀 Watching latest release workflow...\033[0m\n"
    @gh run watch $(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')

# Open release workflow in browser.
release-status:
    @printf "\033[0;34m🔗 Opening release workflow in browser...\033[0m\n"
    @gh run list --workflow=release.yml --limit=1 --web

# Check current workflow status.
workflow-status:
    @printf "\033[0;34m📊 Checking workflow status...\033[0m\n"
    @cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- workflow-status

# Reset workflow state.
workflow-reset:
    @printf "\033[0;33m⚠️  Resetting workflow state...\033[0m\n"
    @cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- workflow-reset

# Resume incomplete workflow.
workflow-resume:
    @printf "\033[0;34m🔄 Resuming workflow...\033[0m\n"
    @cargo run -q --release --target-dir target/ci-bootstrap -p uffs-ci-pipeline -- workflow-resume


# File a port suggestion on the template repo (rust-forge-template).
#
# UFFS is the daily driver where scaffolding improvements are usually
# discovered (new clippy lints, gate tweaks, workflow hardening, hook
# fixes). When a change is generic — useful to ANY project, not just
# UFFS — let the template know so it doesn't silently drift behind:
#
#   just forge-note "adopt clippy::new_lint at deny" "landed in PR #NNN; rationale ..."
#
# The issue lands on skyllc-ai/rust-forge-template with the
# `upstream-port` label and a backlink to the current UFFS commit.
forge-note TITLE BODY="":
    #!/usr/bin/env bash
    set -euo pipefail
    SHA=$(git rev-parse --short HEAD)
    BRANCH=$(git branch --show-current)
    gh issue create -R skyllc-ai/rust-forge-template \
        --title "port: {{ TITLE }}" \
        --label upstream-port \
        --body "$(printf 'Port suggestion from UFFS (`%s` @ `%s`).\n\n%s\n\n---\nChecklist for the port:\n- [ ] genericize (no UFFS identifiers / product specifics)\n- [ ] apply to the template on a branch; its own gates prove it\n- [ ] update COMPONENTS.md / docs if the surface changed' "$BRANCH" "$SHA" "{{ BODY }}")"
    printf "\033[0;32m✅ Filed on rust-forge-template (label: upstream-port)\033[0m\n"
