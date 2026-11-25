<!--
SPDX-License-Identifier: MPL-2.0
Copyright (c) 2025-2026 SKY, LLC.

UFFS — Release Automation: Current Flow Cheatsheet
-->

# Release Automation — Current Flow

> **Last updated**: 2026-04-25
> **Current phase**: **R3 — release-plz shadow mode** (observation only)
> **Authoritative release mechanism**: still `just ship` + `auto-tag-release.yml` + `release.yml` (unchanged from pre-R3)

> _One-page contributor cheatsheet. For the full multi-phase migration plan see [`release-automation-plan.md`](release-automation-plan.md). For metrics and per-phase validation results see [`release-automation-baseline.md`](release-automation-baseline.md)._

## What you do — vs — what's automatic

| You | Automation |
|---|---|
| Open PR with **conventional-commit title** (`type(scope): subject`) | Commitlint posts an advisory comment if the title is non-conforming; never blocks merge today (Phase R1a). |
| Squash-merge as usual | `release-plz.yml` shadow workflow runs; posts the **proposed release plan** to the run summary. Does not push, tag, PR, or publish. |
| Cut releases the OLD way: run `just ship` locally | `auto-tag-release.yml` detects the version bump, creates the `vX.Y.Z` tag. `release.yml` builds binaries, signs, uploads, creates the GitHub Release. |
| Periodically check the shadow workflow runs at [`Actions → 🔮 Release-plz (shadow)`](https://github.com/skyllc-ai/UltraFastFileSearch/actions/workflows/release-plz.yml) and compare to your gut "what should this release have bumped" | — |

## PR title format (the only thing you have to remember)

```
type(scope): subject
```

| Type | Triggers release? | Version bump |
|---|---|---|
| `feat` | yes | minor (0.X.0) |
| `fix` | yes | patch (0.0.X) |
| `perf` | yes | patch |
| `feat!` / `fix!` | yes | major (or minor pre-1.0) |
| `refactor`, `docs`, `test`, `chore`, `ci`, `build`, `style`, `revert` | no | — |

**Scope** (optional): a crate name or short area tag — `mft`, `cli`, `core`, `security`, `polars`, `ci`, `architecture`, etc.  Omit if the change is workspace-wide.

**If unsure**: use `chore:` — it never triggers a release.

Full reference (with examples): [`CONTRIBUTING.md` → "Commit message conventions"](../../CONTRIBUTING.md#commit-message-conventions).

## Step-by-step for a typical change

```bash
# 1. Branch
git checkout -b feat/my-change

# 2. Code
# (... your edits ...)

# 3. Local validation (cheap layer, ~seconds)
just lint-fast

# 4. Commit (intermediate commits don't need conventional format —
#    only the squash subject does)
git commit -m "WIP: trying X"
git push --set-upstream origin feat/my-change

# 5. Open PR with a CONVENTIONAL TITLE
gh pr create --title "feat(mft): add zstd-compressed MFT archive support" \
             --body "..."

# 6. CI runs:
#    - PR Fast (sanity, security, build, test, windows, ...)
#    - Commitlint advisory on title
#    - Other auto-checks

# 7. Address review feedback, push more commits
# 8. Squash-merge.  PR title becomes the squash subject on `main`.

# 9. Watch:
#    - Shadow `release-plz.yml` runs and posts proposed plan to run summary.
#    - No release happens — that requires `just ship`.
```

## Step-by-step for cutting a release (still the OLD way)

Until R4 lands, releases are still cut manually exactly as they were before R0:

```bash
# On main, after merging the PRs you want in this release:
git checkout main && git pull
just ship           # runs the full pipeline:
                    #   - bumps versions in Cargo.toml + Cargo.lock
                    #   - regenerates lockfile (R0 fix)
                    #   - commits the version bump
                    #   - pushes to main
# auto-tag-release.yml then:
#   - sees the version diff in Cargo.toml
#   - creates tag `vX.Y.Z`
# release.yml then:
#   - builds binaries on Linux/macOS/Windows
#   - signs (codesign on macOS)
#   - publishes the GitHub Release with binaries attached
```

## What changes at each upcoming phase

(Each row only describes the **delta** from the previous phase.  See [`release-automation-plan.md`](release-automation-plan.md) for full detail.)

| Phase | What flips | What you'll do differently |
|---|---|---|
| **R3 (now)** | Shadow workflow active | Watch the run summary on each merge.  No flow change. |
| **R1b** (≥1 month after R1a) | Commitlint becomes mandatory | A non-conforming title hard-fails PR Fast.  Edit the title; the bot re-checks automatically. |
| **R4** | release-plz flips to active mode (`release-pr` + `release` commands, write permissions, `RELEASE_PLZ_TOKEN` secret) | After merging release-worthy PRs, release-plz opens a "Release PR" automatically.  You **review and merge it**.  Tag + GitHub Release happen automatically.  `just ship` is no longer needed for releases. |
| **R5** | Bespoke tooling retired (`build/update_all_versions.rs`, `auto-tag-release.yml`, `scripts/ci-pipeline/src/version.rs`) | `just ship` becomes a thin local sanity-check wrapper or is deleted entirely.  Releases happen exclusively via release-plz PRs. |
| **R6** | Per-crate `[[package]]` config + `cargo-semver-checks` | API breaking changes get caught at PR time, not at release time.  No flow change. |
| **R7** | OIDC trusted publisher scaffolded (still dormant) | No change in your day-to-day. |
| **R8** | First crates.io publish dress rehearsal — `uffs-time` only | One crate becomes available on `crates.io`.  No change to your flow. |
| **R9** | Live publishing for the full publishable workspace (deferred — explicit decision) | Deferred indefinitely; documented separately when scheduled. |

The whole multi-phase rollout is designed so each transition is **a single small PR** that you review like any other.  No "big bang".

## FAQ

**Q: I forgot the conventional title and merged a PR with `Improve search performance`. Did I just break the release pipeline?**
A: No.  In R1a (today) commitlint is advisory — your PR merged fine.  In R3 the shadow workflow's release-plz proposal will quietly skip that commit (treated as if the merge had no release-worthy content), and you'll see no entry for it in the proposed changelog.  Going forward, just use `perf: improve search performance` on the next PR.  In R1b (≥1 month from now) the gate becomes mandatory and you'd be asked to edit the title before merge.

**Q: The shadow workflow turned red. Should I worry?**
A: Probably not.  The shadow workflow is advisory — it has no required-checks dependents and cannot block any other workflow or PR.  If it's red, click into the run, look at the error, and either fix it (`release-plz.toml` / `cliff.toml` regression) or open an issue if it's a release-plz upstream bug.  In the meantime everything else keeps working.

**Q: I want to test the shadow workflow without waiting for a merge. Can I?**
A: Yes — go to [`Actions → 🔮 Release-plz (shadow) → Run workflow`](https://github.com/skyllc-ai/UltraFastFileSearch/actions/workflows/release-plz.yml) (the `workflow_dispatch` trigger).  This runs against the current `main` HEAD with no side effects.

**Q: Can I run release-plz locally to preview?**
A: Yes.  `cargo install release-plz --locked` then `release-plz update --config release-plz.toml` from the workspace root.  This edits files in your working tree but never pushes — it's the same command CI runs.  Discard the changes with `git checkout .` when done.  Note: requires a clean working tree (`release-plz` aborts if there are uncommitted changes; pass `--allow-dirty` to override locally).

**Q: I need to ship a fix RIGHT NOW. Does the R3 work block me?**
A: No.  Run `just ship` exactly as you would have pre-R3.  R3 added a parallel observation workflow; it didn't change the live release path.

**Q: What's the merge order if multiple release-worthy PRs land in a single day?**
A: For now (R3): each merge triggers an independent shadow run; the latest one is the most relevant.  For R4+: release-plz coalesces them into a single Release PR, which auto-updates as new release-worthy commits land.  You merge it when you're ready to cut the release.

**Q: I see `release-plz update` created `crates/uffs-*/CHANGELOG.md` files in my local tree. Do I commit them?**
A: No.  Per-crate vs workspace-root changelog structure is an open R4-blocking decision (see [`release-automation-baseline.md` §9](release-automation-baseline.md#9-r3-addendum--release-plz-shadow-mode-validation-2026-04-25)).  Discard them with `rm crates/*/CHANGELOG.md` until R4 settles the structure.

## Where things live

| Concern | File |
|---|---|
| PR-title convention reference | [`CONTRIBUTING.md`](../../CONTRIBUTING.md) → "Commit message conventions" |
| Commitlint workflow | [`.github/workflows/commitlint.yml`](../../.github/workflows/commitlint.yml) |
| git-cliff template (changelog format) | [`cliff.toml`](../../cliff.toml) (workspace root) |
| release-plz config | [`release-plz.toml`](../../release-plz.toml) (workspace root) |
| Shadow workflow (R3) | [`.github/workflows/release-plz.yml`](../../.github/workflows/release-plz.yml) |
| Live release workflow (current) | [`.github/workflows/release.yml`](../../.github/workflows/release.yml) |
| Tag-creation workflow | `auto-tag-release.yml`, retired in R5 (#379) |
| Bespoke version bumper | `build/update_all_versions.rs`, retired in R5 (#379) |
| Multi-phase migration plan | [`release-automation-plan.md`](release-automation-plan.md) |
| Per-phase validation evidence | [`release-automation-baseline.md`](release-automation-baseline.md) |

## Maintenance of this document

This file is a **living reference** — its `Current phase` header at top is updated at each phase transition.  When you open a phase-N PR, also update:

- The `Current phase:` line at top.
- The "Last updated" line.
- The "What changes at each upcoming phase" table — strikethrough or remove the row that just landed; verify the next row is still accurate.
- Any FAQ that's been overtaken by reality.

If you're reading this and the `Last updated` line is stale by more than a phase transition, ping the maintainer.
