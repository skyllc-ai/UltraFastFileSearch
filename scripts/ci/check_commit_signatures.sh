#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.
#
# Commit-signature validator.
#
# **Why this exists** — `main` is governed by a GitHub ruleset that
# requires every commit to carry a *verified* signature (plus a merge
# queue).  An unsigned commit pushes fine and passes every other local
# gate, so the breakage only surfaces at the merge queue
# ("Merging is blocked: Commits must have verified signatures") — a slow,
# maximally-late failure after a full CI round-trip.  This gate moves that
# catch left: it fails the push in ~1 second when any commit in the pushed
# range is unsigned, and points at the one-command self-heal.
#
# It mirrors the shape of `check_commit_subjects.sh` (same `range` mode,
# same `{{COMMIT_RANGES}}` wiring from the pre-push hook).
#
# **Mode**:
#   range RANGE   -- check every commit, merges included, in the git
#                    revision range RANGE (e.g. `origin/main..HEAD`).
#                    An empty range silently succeeds.
#
# **What counts as signed** — `git log --format=%G?` status codes:
#   G  good signature                              -> OK
#   U  good signature, unknown validity            -> OK
#   E  signature present, cannot verify (key not   -> OK (GitHub is the
#      in local keyring, e.g. a teammate's commit)     authoritative check)
#   N  NO signature                                -> FAIL (the bug we hit)
#   B  BAD signature                               -> FAIL
#   X/Y/R  expired key / expired sig / revoked     -> FAIL (GitHub rejects too)
#
# **Exit codes**: 0 all signed, 1 an unsigned/bad commit (or input error).
#
# There is intentionally NO bypass knob: an unsigned commit on a branch
# bound for `main` WILL be rejected by the ruleset, so "bypassing" locally
# only defers the same failure to the merge queue.  Fix it, don't skip it.

set -euo pipefail

# ── Colours ─────────────────────────────────────────────────────────
if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    C_GREEN=$'\033[0;32m'
    C_RED=$'\033[0;31m'
    C_YELLOW=$'\033[1;33m'
    C_CYAN=$'\033[0;36m'
    C_RESET=$'\033[0m'
else
    C_GREEN=''
    C_RED=''
    C_YELLOW=''
    C_CYAN=''
    C_RESET=''
fi

usage() {
    cat <<USAGE >&2
Usage: $(basename "$0") range RANGE

  range RANGE   Verify every commit (merges included) in RANGE is signed
                (e.g. 'origin/main..HEAD').

Example:
  $(basename "$0") range origin/main..HEAD
USAGE
    exit 2
}

print_help_block() {
    cat >&2 <<HELP

${C_YELLOW}-> Why:${C_RESET} \`main\` requires every commit to have a ${C_CYAN}verified signature${C_RESET}
   (GitHub ruleset).  Unsigned commits are rejected at the merge queue.

${C_YELLOW}Fix (self-heal):${C_RESET} re-sign the whole branch in place, then re-push:
   ${C_CYAN}just sign-branch${C_RESET}

   (equivalent to:
     ${C_CYAN}git rebase --rebase-merges --exec 'git commit --amend --no-edit -S' \$(git merge-base origin/main HEAD)${C_RESET}
     ${C_CYAN}git push --force-with-lease${C_RESET})

${C_YELLOW}Root cause to avoid:${C_RESET} never commit with ${C_CYAN}-c commit.gpgsign=false${C_RESET}.
   \`commit.gpgsign=true\` is the repo default; run ${C_CYAN}just doctor-signing${C_RESET}
   to confirm your GPG key works and is registered on GitHub.
HELP
}

# ── Main ────────────────────────────────────────────────────────────
[[ $# -ge 2 ]] || usage
mode="$1"
arg="$2"

case "$mode" in
    range)
        if ! git rev-parse --git-dir >/dev/null 2>&1; then
            printf '%s[FAIL]%s not in a git repository\n' "$C_RED" "$C_RESET" >&2
            exit 1
        fi
        # `%H<tab>%G?` one line per commit, merges included: PRs land on
        # `main` as merge commits through the queue, so a merge made on a
        # branch reaches `main` as-is and must carry a signature like any
        # other commit (the ruleset rejects it otherwise).
        fail=0
        count=0
        while IFS=$'\t' read -r oid sig; do
            [[ -n "$oid" ]] || continue
            count=$((count + 1))
            case "$sig" in
                G | U | E) ;; # signed (GitHub does the authoritative verify)
                *)
                    fail=1
                    printf '%s[FAIL]%s unsigned/bad-signature commit (%s) %s\n' \
                        "$C_RED" "$C_RESET" "$sig" "${oid:0:10}" >&2
                    printf '   %ssubject:%s %s\n' "$C_CYAN" "$C_RESET" \
                        "$(git log -1 --format='%s' "$oid" 2>/dev/null)" >&2
                    ;;
            esac
        done < <(git log --format='%H%x09%G?' "$arg" 2>/dev/null || true)

        if (( fail )); then
            print_help_block
            exit 1
        fi
        if [[ "${COMMIT_SIGNATURE_VERBOSE:-0}" == "1" ]]; then
            printf '%s[OK]%s %d commit(s) in %s are signed\n' \
                "$C_GREEN" "$C_RESET" "$count" "$arg"
        fi
        ;;

    *)
        usage
        ;;
esac
