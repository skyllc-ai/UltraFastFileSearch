# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2025-2026 SKY, LLC.

# Shared settings, flags, helpers, and bootstrap.
# Updated: April 2026 — Nightly (1.96+) / Edition 2024

set shell := ["bash", "-euo", "pipefail", "-c"]
# Windows PowerShell 5.1 (powershell.exe) reads files without a BOM as the
# system ANSI code page (typically CP1252) — NOT as UTF-8.  `just` writes
# shebang recipe bodies (`#!powershell`) as UTF-8 without a BOM, so any
# multi-byte Unicode codepoint whose UTF-8 continuation byte lands in
# 0x91-0x94 decodes to a smart quotation mark (U+2018-U+201D) which the
# PowerShell tokenizer treats as a string delimiter.  This silently
# terminates string literals mid-line and surfaces downstream as
# "Unexpected token" / "Missing closing '}'" errors far from the real
# cause.  Known offenders that hit this trap:
#   em-dash (—)        UTF-8 E2 80 94  -> 0x94 = U+201D right double quote
#   box-drawing (─)    UTF-8 E2 94 80  -> 0x94 = U+201D right double quote
#   right arrow (→)    UTF-8 E2 86 92  -> 0x92 = U+2019 right single quote
#   package emoji (📦) UTF-8 F0 9F 93 A6 -> 0x93 = U+201C left double quote
#   bulb emoji   (💡) UTF-8 F0 9F 92 A1 -> 0x92 = U+2019 right single quote
# RULE: Keep all `[windows]` `#!powershell` recipe strings ASCII-only.
# Unicode inside `#` comments is safe (tokenizer skips comment bodies).
# Migrating to PowerShell 7 (`pwsh`) would also fix this — pwsh reads
# scripts as UTF-8 by default — but that requires every operator to have
# PS Core installed, which the Windows-host CI baseline does not.
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

export FORCE_COLOR := "1"
export CLICOLOR_FORCE := "1"
export TERM := "xterm-256color"
export COLORTERM := "truecolor"
export CARGO_TERM_COLOR := "always"
# CARGO_INCREMENTAL is deliberately NOT exported here.  `.cargo/config.toml`
# owns the cache policy: `build.rustc-wrapper` (the optional-sccache shim at
# `scripts/dev/rustc-wrapper`, see issue #587) + `build.incremental = false`.
# Env var overrides config, so exporting `CARGO_INCREMENTAL=1`
# from just would defeat sccache (incremental artifacts are non-cacheable)
# and recreate v0.5.71's Bug B.  See docs/architecture/dev-flow-implementation-plan.md § 2.1.
#
# NOTE: `RUSTC_WRAPPER := ""` below then disables the wrapper outright for
# every just-driven build, so sccache is in practice only active for raw
# `cargo` invocations.  That predates the shim and is left as-is here; it is
# also why the shim's blast radius is small.
export CARGO_NET_RETRY := "3"
export CARGO_HTTP_TIMEOUT := "30"
export CARGO_HTTP_MULTIPLEXING := "true"
export RUSTC_WRAPPER := ""

# Clippy flags — supplement workspace.lints (belt-and-suspenders for CI).
# Workspace Cargo.toml [workspace.lints] is the source of truth; these ensure
# the justfile lint recipes stay strict even if workspace lints drift.
common_flags := "-D clippy::pedantic -D clippy::nursery -D clippy::cargo -A clippy::multiple_crate_versions -A clippy::redundant_pub_crate -W clippy::panic -W clippy::todo -W clippy::unimplemented -D warnings"
prod_flags := common_flags + " -W clippy::unwrap_used -W clippy::expect_used -W clippy::missing_docs_in_private_items"
test_flags := common_flags + " -A clippy::unwrap_used -A clippy::expect_used"

# Install cargo tool if missing (idempotent)
_install-if-missing TOOL CRATE:
    #!/usr/bin/env bash
    if ! command -v {{ TOOL }} >/dev/null 2>&1; then
        printf "\033[0;34m📦 Installing {{ CRATE }}...\033[0m\n"
        if command -v cargo-binstall >/dev/null 2>&1; then
            cargo binstall {{ CRATE }} --no-confirm --quiet
        else
            cargo install {{ CRATE }} --locked --quiet
        fi
        printf "\033[0;32m✅ {{ TOOL }} installed\033[0m\n"
    else
        printf "\033[0;32m✅ {{ TOOL }} already installed (skip)\033[0m\n"
    fi

# Install rustup component if missing (idempotent)
_install-component COMPONENT:
    #!/usr/bin/env bash
    if ! rustup component list --installed | grep -q "{{ COMPONENT }}"; then
        printf "\033[0;34m📦 Installing {{ COMPONENT }} component...\033[0m\n"
        rustup component add {{ COMPONENT }}
        printf "\033[0;32m✅ {{ COMPONENT }} component installed\033[0m\n"
    else
        printf "\033[0;32m✅ {{ COMPONENT }} component already installed (skip)\033[0m\n"
    fi

# Ensure the pinned nightly toolchain (from rust-toolchain.toml) is installed.
# Called automatically by the CI pipeline. Does NOT change the pinned version.
toolchain-ensure:
    #!/usr/bin/env bash
    set -euo pipefail
    TOOLCHAIN_FILE="rust-toolchain.toml"
    CURRENT=$(grep '^channel' "$TOOLCHAIN_FILE" | sed 's/.*"\(.*\)".*/\1/')
    printf "\033[0;34m🔧 Ensuring toolchain %s is installed...\033[0m\n" "$CURRENT"
    if ! rustup toolchain install "$CURRENT" --profile default 2>&1; then
        printf "\033[0;33m⚠️  Install failed — clearing rustup download cache and retrying...\033[0m\n"
        rm -rf ~/.rustup/downloads/*
        rustup toolchain install "$CURRENT" --profile default
    fi
    rustup default "$CURRENT"
    printf "\033[0;32m✅ Toolchain %s ready\033[0m\n" "$CURRENT"
    rustc --version

# Sync the nightly toolchain to the latest WORKING date and update rust-toolchain.toml.
# Run this MANUALLY when you want to bump the pinned nightly (e.g. before `just go`).
# Do NOT call from CI — use `toolchain-ensure` instead.
#
# Algorithm:
#   1. Start at today's UTC date (the newest nightly that *could* be published).
#   2. Try to install that nightly (with one auto-retry after cleaning the
#      rustup downloads cache, so a stale `.partial` file / transient network
#      error does not immediately force a step-back).  `no release found`
#      short-circuits straight to step-back because it is not transient.
#      Post-install health probe (`rustc +CANDIDATE --version`) catches the
#      rare "install reported success but toolchain is corrupt" case.
#   3. Run clippy ONCE per feature set with the SAME strict flag stack as the
#      PR merge gate (`--workspace --all-targets --locked --no-deps -- -D
#      warnings`) under the `+TOOLCHAIN` override (so rust-toolchain.toml stays
#      untouched during the probe).  Clippy is a SUPERSET of `cargo check` — it
#      runs the full compile front-end and THEN the lint passes — so this one
#      gate proves the workspace both COMPILES and is lint-clean; a separate
#      `cargo check` is deliberately NOT run because it would only recompile
#      the same code for no extra signal (clippy can't reuse the check driver's
#      artifacts, so running both compiled the workspace twice).  Probe BOTH
#      the `--all-features` and `--no-default-features` sets the gate uses,
#      because cfg/feature-gated code (e.g. the non-windows MftReader stubs) can
#      trip a lint in one set but not the other.  A nightly can compile yet
#      still fail the gate by tripping a new/renamed lint or leaving a
#      now-stale `#[expect(...)]` unfulfilled — exactly how the v0.5.106 bump
#      to nightly-2026-05-31 compiled here but failed the merge gate (the
#      `unused_async` → `unused_async_trait_impl` lint split, and
#      `duration_suboptimal_units` no longer firing).  If clippy fails — step
#      back one day, so sync only pins a nightly the merge gate will accept.
#   4. Verify every component listed in rust-toolchain.toml installs cleanly on
#      the candidate.  If a component conflict fires (e.g. the rustup rename
#      `llvm-tools-preview` → `llvm-tools` tripping a stale bookkeeping entry),
#      auto-recover by uninstalling the candidate toolchain, cleaning the
#      rustup downloads cache, and reinstalling fresh.  If the conflict
#      persists even after recovery — step back one day.
#   5. Stop at the current pin.  If the current pin itself works: no-op exit 0.
#      If nothing between today and the current pin works (including the pin):
#      exit 1 with a clear error — manual intervention required.
#
# This makes the recipe robust against:
#   • today's nightly not being published yet (UTC date vs. rust publish time),
#   • transient network / rustup-downloads errors (auto-retried once after a
#     cache wipe before stepping back — handles the common `.partial` rename
#     failure mode and short network blips without losing a day on the pin),
#   • corrupt toolchain installs (`rustc --version` health probe),
#   • upstream nightly regressions (ethnum, rustc ICE, etc.) that fail to
#     COMPILE on specific dates — caught by the Phase 2 clippy gate, which
#     compiles the workspace as part of linting (no separate `cargo check`),
#   • upstream nightly *lint* changes — new lints, lint renames, or lints that
#     stop firing and strand an `#[expect(...)]` — that compile clean but fail
#     the merge gate's `clippy -D warnings` (also caught by the Phase 2 gate),
#   • rustup component-rename bookkeeping conflicts that would otherwise fire
#     on the *next* rustup invocation (breaking `cargo fmt` / `ship-fresh`
#     after a successful sync).
toolchain-sync:
    #!/usr/bin/env bash
    set -uo pipefail
    printf "\033[0;34m🔧 Syncing to latest working nightly toolchain...\033[0m\n"

    TOOLCHAIN_FILE="rust-toolchain.toml"
    MAX_ITERS=21  # safety cap — three weeks of step-back is plenty

    # Read the current channel — this is the floor.  Never downgrade below it
    # silently; that's the user's documented known-good in the file header.
    CURRENT=$(grep '^channel' "$TOOLCHAIN_FILE" | sed 's/.*"\(.*\)".*/\1/')
    CURRENT_DATE="${CURRENT#nightly-}"
    if [ "$CURRENT_DATE" = "$CURRENT" ]; then
        printf "\033[0;31m❌ Current pin %s is not a nightly-YYYY-MM-DD channel — refusing to auto-sync.\033[0m\n" "$CURRENT"
        exit 1
    fi
    printf "\033[0;34m   Current pin: %s (floor — will not step below this)\033[0m\n" "$CURRENT"

    # Cross-target gate (Phase 2b below) validates the Windows build before a
    # bump, not just the host — a host-only probe green-lights x86 / Windows-only
    # regressions (cf. the ring 0.17.x episode: nightly-2026-06-27..30 failed to
    # compile ring for x86_64-pc-windows-msvc while the aarch64 host built clean,
    # so a host-only bump shipped a Windows-broken nightly that only blew up at
    # the pre-push xwin step). That gate needs cargo-xwin; refuse to run without
    # it rather than silently fall back to a host-only — and thus unsafe — bump.
    if ! command -v cargo-xwin >/dev/null 2>&1; then
        printf "\033[0;31m❌ cargo-xwin is required: toolchain-sync now validates the Windows target before a bump.\033[0m\n"
        printf "\033[0;31m   Install it with: cargo install cargo-xwin\033[0m\n"
        exit 1
    fi
    # The Linux release leg (x86_64-unknown-linux-gnu on ubuntu-22.04) gets the
    # same treatment via cargo-zigbuild (zig provides the cross C compiler +
    # linker) — Phase 2c below. Refuse rather than fall back to a gate that
    # skips a shipped target.
    if ! command -v cargo-zigbuild >/dev/null 2>&1 || ! command -v zig >/dev/null 2>&1; then
        printf "\033[0;31m❌ cargo-zigbuild + zig are required: toolchain-sync validates the Linux target before a bump.\033[0m\n"
        printf "\033[0;31m   Install them with: cargo install cargo-zigbuild && brew install zig (or cargo install zig-bin)\033[0m\n"
        exit 1
    fi

    # Raise the file-descriptor limit before any zig link.
    #
    # macOS ships a soft `RLIMIT_NOFILE` of 256.  A zig link of this
    # workspace passes ~240 objects plus ~50 rlibs and needs more than
    # that, so it dies with:
    #
    #   error: unable to search for static library …/libaddr2line-….rlib:
    #   ProcessFdQuotaExceeded
    #
    # toolchain-sync reads that as "this nightly is broken", steps back a
    # day, and repeats until it hits the floor — aborting the ship with
    # "current pin no longer compiles for x86_64-unknown-linux-gnu" when
    # nothing is wrong with any toolchain.  Every candidate passes clippy
    # and the Windows gate; only the Linux probe fails, always on fds.
    #
    # Measured on this host: at soft 256 the link fails, and the same
    # command at 4096 exits 0 — package-scoped and for the full
    # `--workspace --all-features`.
    #
    # Only the SOFT limit moves; the hard limit is the ceiling and
    # lowering it would be irreversible for this shell.  If the hard
    # limit is below the target we take whatever it allows.  The result
    # is printed: a guard that silently does nothing looks exactly like
    # one that worked, and an earlier version of this block did nothing
    # for two commits because of it.
    FD_BEFORE=$(ulimit -Sn 2>/dev/null || echo unknown)
    if [ "$FD_BEFORE" != "unlimited" ] && [ "$FD_BEFORE" -lt 4096 ] 2>/dev/null; then
        ulimit -S -n 4096 2>/dev/null || ulimit -S -n "$(ulimit -Hn)" 2>/dev/null || true
    fi
    printf "   \033[0;36mfd limit: %s → %s (zig linker needs headroom)\033[0m\n" \
        "$FD_BEFORE" "$(ulimit -Sn)"

    START_DATE=$(date -u +%Y-%m-%d)
    printf "\033[0;34m   Start date:  nightly-%s (today UTC)\033[0m\n\n" "$START_DATE"

    # Portable one-day-back helper — macOS (BSD date) vs. Linux (GNU date).
    date_minus_one() {
        local d="$1"
        if date -v -1d -j -f "%Y-%m-%d" "$d" "+%Y-%m-%d" 2>/dev/null; then
            return 0
        fi
        date -u -d "$d -1 day" "+%Y-%m-%d"
    }

    # Portable sed -i — GNU sed wants `-i`, BSD sed wants `-i ''`.  The old
    # recipe's `sed -i''` pattern parsed as `-i` with extension `-e` on BSD,
    # which is why `rust-toolchain.toml-e` kept reappearing.
    sed_inplace() {
        local script="$1"
        local file="$2"
        if sed --version >/dev/null 2>&1; then
            sed -i -e "$script" "$file"
        else
            sed -i '' -e "$script" "$file"
        fi
    }

    # Extract the canonical component list from rust-toolchain.toml.
    # Reads the `components = [...]` block and emits one component name per
    # line, quote-stripped.  Returns empty on a missing / malformed file.
    #
    # awk range `/^components = \[/,/^\]/` selects the block; grep narrows
    # to the quoted tokens; tr strips the quotes.  Comment lines inside the
    # block are skipped naturally because `grep -Eo '"[a-z_-]+"'` only
    # matches bare quoted identifiers.
    read_components() {
        awk '/^components = \[/,/^\]/' "$TOOLCHAIN_FILE" \
            | grep -Eo '"[a-z_-]+"' \
            | tr -d '"' || true
    }

    # Add every rust-toolchain.toml component to `$1` (the toolchain name).
    # Returns:
    #   0 — all components installed cleanly.
    #   2 — at least one component failed with a "detected conflict"
    #       error (stale rustup bookkeeping from the rename of
    #       `llvm-tools-preview` → `llvm-tools`, etc.).  Caller can
    #       recover with uninstall + reinstall.
    #   1 — any other component-install failure.
    ensure_components() {
        local toolchain="$1"
        local comp
        for comp in $(read_components); do
            if ! rustup component add "$comp" --toolchain "$toolchain" \
                    >/tmp/toolchain-sync-component.log 2>&1; then
                if grep -q 'detected conflict' /tmp/toolchain-sync-component.log; then
                    return 2
                fi
                return 1
            fi
        done
        return 0
    }

    # Wipe the rustup downloads cache so stale `.partial` files (left
    # behind by interrupted installs or concurrent rustup invocations)
    # can't block the next download with `could not rename downloaded
    # file … No such file or directory (os error 2)`.  Safe no-op if
    # the directory is empty or missing.
    clean_rustup_downloads() {
        rm -rf "${HOME}/.rustup/downloads"/* 2>/dev/null || true
    }

    # Try `rustup toolchain install $1 --profile default` with
    # auto-recovery on transient failures.
    # Returns:
    #   0   — install succeeded and `rustc +$1 --version` confirms the
    #         toolchain is usable.
    #   10  — channel does not exist (`no release found`).  Caller
    #         should step back; the cache has not been touched.
    #   1   — install or health check failed after one retry.  Caller
    #         should step back.
    #
    # Recovery flow:
    #   1. First `rustup toolchain install` attempt.
    #   2. On failure: if the log reports `no release found`, return
    #      10 immediately (not a transient error).  Otherwise wipe the
    #      downloads cache and retry once.
    #   3. After a successful install, run `rustc +$1 --version` as a
    #      health probe.  If rustc fails to launch (missing binary,
    #      corrupt install), treat the install as failed.
    try_install() {
        local toolchain="$1"
        local attempt
        for attempt in 1 2; do
            if rustup toolchain install "$toolchain" --profile default \
                    >/tmp/toolchain-sync-install.log 2>&1; then
                # Health check — install succeeded, does rustc actually run?
                if rustc "+$toolchain" --version \
                        >/tmp/toolchain-sync-health.log 2>&1; then
                    return 0
                fi
                # Install claimed success but rustc is broken.  Clean
                # and fall through to the retry (or return 1 on
                # attempt 2).
                clean_rustup_downloads
                continue
            fi
            if grep -q 'no release found' /tmp/toolchain-sync-install.log; then
                return 10
            fi
            # Some other error (rename / partial / network flake).
            # Clean the downloads cache before the retry; on the
            # second attempt there is no retry and we fall out to
            # return 1 below.
            if [ "$attempt" = "1" ]; then
                printf "\033[0;33m   ⚠️  install hit a transient error — cleaning rustup downloads cache and retrying.\033[0m\n"
                clean_rustup_downloads
            fi
        done
        return 1
    }

    # Per-candidate scratch target dir so each probe gets a clean artifact
    # cache.  Sharing the main `target/` dir across candidates fails with
    # "please recompile that crate using this compiler" errors whenever the
    # rustc hash differs between the cached artifacts and the current probe
    # — which is exactly the common case (probing N nightlies in a row).
    # Trap cleans up on any exit (success, error, or ^C).
    PROBE_ROOT="${TMPDIR:-/tmp}/uffs-toolchain-probe.$$"
    mkdir -p "$PROBE_ROOT"
    trap 'rm -rf "$PROBE_ROOT"' EXIT

    CANDIDATE_DATE="$START_DATE"
    ITER=0
    while [ "$ITER" -lt "$MAX_ITERS" ]; do
        CANDIDATE="nightly-${CANDIDATE_DATE}"
        printf "\033[0;34m──▶ [%d/%d] Trying %s\033[0m\n" "$((ITER+1))" "$MAX_ITERS" "$CANDIDATE"

        # ── Phase 1: install + health check ─────────────────────
        # `try_install` already retries once on transient download /
        # rename failures after cleaning `~/.rustup/downloads`, so
        # flaky networks or stale `.partial` files from earlier
        # interrupted installs recover automatically.  A post-install
        # `rustc +CANDIDATE --version` health probe catches the
        # "install reported success but the toolchain is broken"
        # class of corruption.
        try_install "$CANDIDATE"
        INSTALL_RC=$?
        if [ "$INSTALL_RC" = "0" ]; then
            printf "\033[0;32m   ✅ install ok\033[0m\n"
        else
            if [ "$INSTALL_RC" = "10" ]; then
                printf "\033[0;33m   ⚠️  %s not published yet — stepping back one day.\033[0m\n" "$CANDIDATE"
            else
                printf "\033[0;33m   ⚠️  install of %s failed after retry (see /tmp/toolchain-sync-install.log) — stepping back one day.\033[0m\n" "$CANDIDATE"
            fi

            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s cannot be installed — it may be an invalid or future date.\033[0m\n" "$CURRENT"
                printf "\033[0;31m   Fix rust-toolchain.toml manually, then rerun.\033[0m\n"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi

        # ── Phase 2: clippy gate (same strict flags as the PR merge gate) ──
        # `+TOOLCHAIN` override bypasses rust-toolchain.toml, so a failed probe
        # never mutates the pin.  This is the SINGLE compile step: clippy is a
        # superset of `cargo check` — it runs the full compile front-end and
        # THEN the lint passes — so we deliberately do NOT also run `cargo
        # check` (that would just recompile the same code for no extra signal;
        # clippy can't reuse the check driver's artifacts, so keeping both
        # compiled the workspace twice).  This one gate therefore proves the
        # workspace COMPILES *and* passes the PR gate's `-D warnings` lint.
        # The PR gate (`pr-fast.yml`) runs clippy with `-D warnings`, so a
        # nightly that compiles can still fail it by tripping a new/renamed lint
        # or leaving a now-stale `#[expect(...)]` unfulfilled — exactly the
        # v0.5.106 → nightly-2026-05-31 episode (`unused_async` →
        # `unused_async_trait_impl` split; `duration_suboptimal_units` no longer
        # firing — which a bare `cargo check` sailed straight past).  Probe
        # BOTH the `--all-features` and `--no-default-features` sets the gate
        # uses, because cfg/feature-gated code (e.g. the non-windows MftReader
        # stubs) can trip a lint in one set but not the other; `--all-targets`
        # covers tests/benches/examples (e.g. the ethnum-via-polars-parquet
        # path that only surfaces under test targets).  CARGO_TARGET_DIR is
        # scoped per-candidate so cross-toolchain artifact hash mismatches
        # ("please recompile that crate using this compiler") can't produce
        # false negatives, and the two feature-set runs share it.  This is the
        # HOST leg of the gate (aarch64 here); the Windows cross-compile is
        # validated separately in Phase 2b below, so a host-only pass can no
        # longer green-light an x86 / Windows-only regression.  `clippy` ships in
        # the `--profile default` install (Phase 1), so it is already present.
        CLIPPY_OK=1
        for CLIPPY_FEATURES in "--all-features" "--no-default-features"; do
            printf "\033[0;34m   Running cargo +%s clippy %s -- -D warnings...\033[0m\n" "$CANDIDATE" "$CLIPPY_FEATURES"
            if CARGO_TARGET_DIR="$PROBE_ROOT/$CANDIDATE" \
                cargo "+$CANDIDATE" clippy --workspace --all-targets "$CLIPPY_FEATURES" --locked --no-deps --quiet -- -D warnings \
                >/tmp/toolchain-sync-clippy.log 2>&1; then
                printf "\033[0;32m   ✅ clippy ok (%s)\033[0m\n" "$CLIPPY_FEATURES"
            else
                printf "\033[0;33m   ⚠️  clippy failed under %s (%s) — stepping back one day.\033[0m\n" "$CANDIDATE" "$CLIPPY_FEATURES"
                printf "\033[0;33m      (last 5 lines from /tmp/toolchain-sync-clippy.log:)\033[0m\n"
                tail -n 5 /tmp/toolchain-sync-clippy.log | sed 's/^/        /'
                CLIPPY_OK=0
                break
            fi
        done
        if [ "$CLIPPY_OK" != "1" ]; then
            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s no longer compiles + passes clippy -D warnings.\033[0m\n" "$CURRENT"
                printf "\033[0;31m   Full log: /tmp/toolchain-sync-clippy.log\033[0m\n"
                printf "\033[0;31m   Manual intervention required — fix the lints; the merge gate will reject this pin.\033[0m\n"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi

        # ── Phase 2b: Windows cross-compile gate ────────────────
        # The host clippy above only proves the build works on the maintainer's
        # arch (aarch64).  A bump must not be accepted on a nightly that cannot
        # build the SHIPPED Windows binary, or a Windows-only regression sails
        # through here and only surfaces at the pre-push `cargo xwin` step (cf.
        # the ring 0.17.x episode).  Compile the workspace for
        # x86_64-pc-windows-msvc via cargo-xwin under `-D warnings`; on any
        # failure step back exactly like the host clippy gate.  The candidate
        # needs the Windows rust-std first (`--profile default` installs neither
        # extra targets nor components — components come in Phase 3).  Reuses the
        # per-candidate scratch CARGO_TARGET_DIR (artifacts land under its
        # `x86_64-pc-windows-msvc/` subdir, separate from the host leg).
        printf "\033[0;34m   Adding x86_64-pc-windows-msvc target to %s...\033[0m\n" "$CANDIDATE"
        if ! rustup target add x86_64-pc-windows-msvc --toolchain "$CANDIDATE" \
                >/tmp/toolchain-sync-wintarget.log 2>&1; then
            printf "\033[0;33m   ⚠️  could not add the windows target to %s — stepping back one day.\033[0m\n" "$CANDIDATE"
            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s cannot add x86_64-pc-windows-msvc.\033[0m\n" "$CURRENT"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi
        printf "\033[0;34m   Running cargo +%s xwin clippy --target x86_64-pc-windows-msvc...\033[0m\n" "$CANDIDATE"
        if CARGO_TARGET_DIR="$PROBE_ROOT/$CANDIDATE" \
            cargo "+$CANDIDATE" xwin clippy --target x86_64-pc-windows-msvc --workspace --all-targets --all-features --locked --no-deps --quiet -- -D warnings \
            >/tmp/toolchain-sync-xwin.log 2>&1; then
            printf "\033[0;32m   ✅ windows xwin clippy ok\033[0m\n"
        else
            printf "\033[0;33m   ⚠️  windows xwin clippy failed under %s — stepping back one day.\033[0m\n" "$CANDIDATE"
            printf "\033[0;33m      (last 5 lines from /tmp/toolchain-sync-xwin.log:)\033[0m\n"
            tail -n 5 /tmp/toolchain-sync-xwin.log | sed 's/^/        /'
            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s no longer compiles for x86_64-pc-windows-msvc.\033[0m\n" "$CURRENT"
                printf "\033[0;31m   Full log: /tmp/toolchain-sync-xwin.log\033[0m\n"
                printf "\033[0;31m   Manual intervention required — the merge gate's Windows build will reject this pin.\033[0m\n"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi

        # ── Phase 2c: Linux cross-compile gate ──────────────────
        # The release also ships x86_64-unknown-linux-gnu (built natively on
        # the ubuntu-22.04 runner), so a bump must not be accepted on a nightly
        # that cannot build that leg either — the exact class of the ring
        # episode, just pointed at Linux. cargo-zigbuild supplies the cross C
        # compiler + linker (zig), so crates with native code (ring, mimalloc)
        # compile for the real GNU triple from this host. `zigbuild` performs a
        # full build (compile + link) — the faithful equivalent of what the
        # release runner does; lint drift is already covered by the host clippy
        # leg. Reuses the per-candidate scratch CARGO_TARGET_DIR.
        printf "\033[0;34m   Adding x86_64-unknown-linux-gnu target to %s...\033[0m\n" "$CANDIDATE"
        if ! rustup target add x86_64-unknown-linux-gnu --toolchain "$CANDIDATE" \
                >/tmp/toolchain-sync-linuxtarget.log 2>&1; then
            printf "\033[0;33m   ⚠️  could not add the linux target to %s — stepping back one day.\033[0m\n" "$CANDIDATE"
            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s cannot add x86_64-unknown-linux-gnu.\033[0m\n" "$CURRENT"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi
        printf "\033[0;34m   Running cargo +%s zigbuild --target x86_64-unknown-linux-gnu...\033[0m\n" "$CANDIDATE"
        if CARGO_TARGET_DIR="$PROBE_ROOT/$CANDIDATE" \
            cargo "+$CANDIDATE" zigbuild --target x86_64-unknown-linux-gnu --workspace --all-features --locked --quiet \
            >/tmp/toolchain-sync-zigbuild.log 2>&1; then
            printf "\033[0;32m   ✅ linux zigbuild ok\033[0m\n"
        else
            printf "\033[0;33m   ⚠️  linux zigbuild failed under %s — stepping back one day.\033[0m\n" "$CANDIDATE"
            printf "\033[0;33m      (last 5 lines from /tmp/toolchain-sync-zigbuild.log:)\033[0m\n"
            tail -n 5 /tmp/toolchain-sync-zigbuild.log | sed 's/^/        /'
            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s no longer compiles for x86_64-unknown-linux-gnu.\033[0m\n" "$CURRENT"
                printf "\033[0;31m   Full log: /tmp/toolchain-sync-zigbuild.log\033[0m\n"
                printf "\033[0;31m   Manual intervention required — the release build's Linux leg will reject this pin.\033[0m\n"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi

        # ── Phase 3: component verification ─────────────────────
        # Install every component listed in rust-toolchain.toml.  The
        # `ship-fresh` failure mode this guards against: rustup lazily
        # tries to add extra components on the *next* invocation after
        # a `--profile default` install, and a component-rename
        # bookkeeping conflict (e.g. stale `llvm-tools-preview` vs
        # current `llvm-tools`) breaks `cargo fmt` / `cargo clippy` /
        # any subsequent CI step.  Catching this at sync-time lets us
        # recover with uninstall + reinstall without polluting the
        # pipeline log with ambiguous failures.
        printf "\033[0;34m   Adding rust-toolchain.toml components to %s...\033[0m\n" "$CANDIDATE"
        ensure_components "$CANDIDATE"
        COMPONENT_RC=$?
        if [ "$COMPONENT_RC" = "2" ]; then
            printf "\033[0;33m   ⚠️  Component conflict — reinstalling %s to clear rustup bookkeeping.\033[0m\n" "$CANDIDATE"
            rustup toolchain uninstall "$CANDIDATE" >/dev/null 2>&1 || true
            # Clean the downloads cache too — the same run that left
            # the component-bookkeeping in a bad state likely also
            # left `.partial` downloads behind.
            clean_rustup_downloads
            if try_install "$CANDIDATE" >/dev/null 2>&1; then
                ensure_components "$CANDIDATE"
                COMPONENT_RC=$?
                if [ "$COMPONENT_RC" = "0" ]; then
                    printf "\033[0;32m   ✅ reinstall recovered the conflict\033[0m\n"
                fi
            fi
        fi
        if [ "$COMPONENT_RC" != "0" ]; then
            printf "\033[0;33m   ⚠️  component install failed under %s — stepping back one day.\033[0m\n" "$CANDIDATE"
            printf "\033[0;33m      (last 3 lines from /tmp/toolchain-sync-component.log:)\033[0m\n"
            tail -n 3 /tmp/toolchain-sync-component.log | sed 's/^/        /'

            if [ "$CANDIDATE" = "$CURRENT" ]; then
                printf "\n\033[0;31m❌ Current pin %s cannot install all required components.\033[0m\n" "$CURRENT"
                printf "\033[0;31m   Full log: /tmp/toolchain-sync-component.log\033[0m\n"
                printf "\033[0;31m   Manual intervention required.\033[0m\n"
                exit 1
            fi
            CANDIDATE_DATE=$(date_minus_one "$CANDIDATE_DATE")
            ITER=$((ITER+1))
            continue
        fi
        printf "\033[0;32m   ✅ all components installed\033[0m\n\n"

        # ── Success ────────────────────────────────────────────
        if [ "$CANDIDATE" = "$CURRENT" ]; then
            printf "\033[0;32m✅ Current pin %s still works — no bump available.\033[0m\n" "$CURRENT"
            rustup default "$CURRENT" >/dev/null 2>&1 || true
            rustc --version
            exit 0
        fi

        printf "\033[0;32m🎉 Found newer working nightly: %s\033[0m\n" "$CANDIDATE"
        printf "\033[0;34m   Updating %s: %s → %s\033[0m\n" "$TOOLCHAIN_FILE" "$CURRENT" "$CANDIDATE"
        sed_inplace "s|channel = \"${CURRENT}\"|channel = \"${CANDIDATE}\"|" "$TOOLCHAIN_FILE"
        # Clean up any BSD-sed backup artifact from older broken invocations.
        rm -f "${TOOLCHAIN_FILE}-e"
        rustup default "$CANDIDATE"

        # ── Invalidate stale build caches after the bump ───────────
        # The probe loop above used a per-candidate scratch CARGO_TARGET_DIR,
        # so the MAIN `target/` and the sccache daemon are still holding the
        # OLD nightly's artifacts.  Two distinct caches must be invalidated or
        # the next build dies with E0514 "found crate X compiled by an
        # incompatible version of rustc":
        #   1. `cargo clean` — the main `target/` has rlibs/rmeta stamped with
        #      the OLD rustc commit-hash; cargo's fingerprint does not always
        #      force a rebuild across a nightly bump, so clear it outright.
        #   2. `sccache --stop-server` — the long-lived sccache server caches
        #      its compiler-version detection keyed by the `rustc` binary's
        #      mtime/size.  Under rustup, `rustc` is a toolchain-agnostic proxy
        #      (~/.cargo/bin/rustc -> rustup) whose bytes never change across a
        #      bump, so the running daemon keeps serving/storing objects under
        #      the OLD nightly's identity.  Stopping the server forces a fresh
        #      `rustc -vV` probe on the next invocation, restoring correct
        #      cache keying.  A full 30 GiB cache wipe is unnecessary — the
        #      daemon restart is the targeted fix.
        printf "\033[0;34m   Invalidating stale build caches (cargo clean + sccache restart)...\033[0m\n"
        cargo clean 2>/dev/null || true
        if command -v sccache >/dev/null 2>&1; then
            sccache --stop-server >/dev/null 2>&1 || true
        fi

        # A new nightly's rustfmt can re-wrap code the old one accepted (the
        # 2026-10-14 -> 2026-10-29 bump re-wrapped comments in twenty files),
        # and the pre-push fmt gate then refuses the very push that carries
        # the pin.  Reformat with the new pin now, so the bump and its
        # formatting land in the same change instead of surfacing at pre-push.
        printf "\033[0;34m   Formatting with the new pin's rustfmt (cargo fmt --all)...\033[0m\n"
        if ! cargo fmt --all; then
            printf "\033[0;31m❌ cargo fmt failed under %s - fix it before committing the pin.\033[0m\n" "$CANDIDATE"
            exit 1
        fi
        if ! git diff --quiet -- '*.rs'; then
            printf "\033[0;33m   ⚠️  rustfmt re-wrapped files under the new pin - commit them with the bump:\033[0m\n"
            git diff --stat -- '*.rs' | tail -n 1 | sed 's/^/        /'
        fi

        printf "\033[0;32m✅ Toolchain synced to %s\033[0m\n" "$CANDIDATE"
        rustc --version
        cargo --version
        exit 0
    done

    printf "\033[0;31m❌ Safety cap of %d iterations reached without finding a working nightly.\033[0m\n" "$MAX_ITERS"
    printf "\033[0;31m   Last tried: nightly-%s.  Either the cap is too low or your workspace is broken.\033[0m\n" "$CANDIDATE_DATE"
    exit 1

# First-time development environment setup (install-if-missing only).
# For updates, use `just update-tools`.
setup:
    #!/usr/bin/env bash
    printf "\033[0;34m🔧 Development Environment Setup\033[0m\n"
    printf "\033[0;34m   Nightly toolchain + essential Rust development tools\033[0m\n"
    echo "========================================================"
    just _install-if-missing cargo-binstall cargo-binstall
    just _install-if-missing cargo-nextest cargo-nextest
    just _install-if-missing cargo-llvm-cov cargo-llvm-cov
    just _install-if-missing cargo-deny cargo-deny
    just _install-if-missing cargo-xwin cargo-xwin
    just _install-if-missing cargo-machete cargo-machete
    just _install-if-missing cargo-outdated cargo-outdated
    just _install-if-missing cargo-semver-checks cargo-semver-checks
    just _install-if-missing cargo-mutants cargo-mutants
    just _install-if-missing cargo-insta cargo-insta
    just _install-if-missing cargo-watch cargo-watch
    just _install-if-missing cargo-expand cargo-expand
    just _install-if-missing rust-script rust-script
    just _install-if-missing sccache sccache
    just _install-if-missing flamegraph flamegraph
    just _install-if-missing samply samply
    just _install-component llvm-tools
    just _install-component miri
    printf "\033[0;32m✅ Development environment setup complete!\033[0m\n"
    printf "\033[0;34mToolchain: $(rustc --version)\033[0m\n"
    printf "\033[0;34mNext steps:\033[0m\n"
    printf "  • Run 'just update-tools' to update everything to latest\n"
    printf "  • Run 'just go' to validate everything safely\n"
    printf "  • Run 'just ship' for the full ship pipeline\n"

