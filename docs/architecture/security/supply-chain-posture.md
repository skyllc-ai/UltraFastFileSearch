# Supply-Chain Posture

<!-- SPDX-License-Identifier: MPL-2.0 -->
<!-- Copyright (c) 2025-2026 SKY, LLC. -->

Status as of **2026-05-12** · Maintainer: `@githubrobbi` · Review cadence: monthly

**Changelog**:
- 2026-05-12 — **`cargo-machete` promoted to pre-push + pr-fast hard gate**.
  Sub-second AST-based unused-dependency detector, complementary to the
  existing weekly `cargo-udeps` check (compile-driven, nightly-only,
  multi-minute).  Closes `dev-flow.md §4.4 GAP 4` — unused-dep
  detection was previously weekly-only.  Wired via `[[gate]] id =
  "machete"` in `scripts/ci/gates.toml`, regenerated `_lint_pre_push.sh`,
  and bundled into `pr-fast.yml`'s existing `security` job step list
  (display name kept stable to preserve check-run history).  Added to
  `just install-dev-tools` so contributors onboard with the binary
  required by the gate.  Workspace was clean against `cargo machete
  --skip-target-dir` at adoption.  See
  [`../code-quality/lint-posture.md` §12 entry for 2026-05-12
  `cargo-machete`](../code-quality/lint-posture.md#12--decisions-log)
  for the full decision-log record.
- 2026-05-12 — **Four-layer cargo-vet audit-discipline policy** (PR #172).
  Closes the lazy-bump anti-pattern PR #166 introduced and PR #170
  manually undid.  Four defenses now compose to make `cargo vet
  regenerate exemptions` the wrong-by-default answer to a patch-bump
  breaking `cargo vet check`:
  1. **CI gate**: `scripts/ci/check_vet_audit_discipline.sh` runs at
     pre-push and in `pr-fast.yml::security`.  Every
     `[[exemptions.<crate>]]` version-change in the diff must be
     paired with both (A) a matching `[[audits.<crate>]]` delta block
     in `audits.toml` AND (B) a `Vet-Reviewed-Diff: <crate>@<old>-><new>`
     trailer on a commit in the same range.
  2. **`just vet-bump <crate>`**: guided audit driver that prints the
     exact `cargo vet diff` / `cargo vet certify` / `git commit
     --trailer` recipe.  Cuts the audit workflow from "remember the
     four cargo-vet subcommands" to one `just` invocation.
  3. **CODEOWNERS**: `supply-chain/` requires explicit @githubrobbi
     review on every PR, regardless of which other CODEOWNERS line
     it matches.
  4. **Commit-trailer**: the `Vet-Reviewed-Diff:` trailer is the
     reviewer's signature in the git log, anchored to the signed-
     commit branch-protection rule on `main`.  Same shape as
     `Signed-off-by:` from the kernel workflow.
  Bypass for emergencies only: `BYPASS_VET_AUDIT_DISCIPLINE=1 git
  push`.  CI has no bypass.  See §"Mandating audits over blanket
  bumps" below for the full posture.
- 2026-04-22 — Initial baseline (PR #33a).  Added `cargo-geiger`
  on-demand, `dependabot-review.yml` dep-tree-growth annotation.
- 2026-04-22 — Added cargo-vet audit trail (PR #33b): 5 upstream
  imports (Mozilla, Google, Bytecode Alliance, ISRG, Zcash),
  `cargo vet check --locked` gate in CI, weekly
  `cargo-vet-refresh.yml` import-refresh workflow.
- 2026-04-22 — Committed `Cargo.lock` (PR #33b hotfix): switched
  from gitignored-lockfile to committed-lockfile for reproducibility,
  SLSA-attestation integrity, and cargo-vet stability.  See
  [Lockfile pinning](#lockfile-pinning-cargolock) below.
- 2026-04-22 — Documented known softprops old-commit 403 limitation
  in the release flow (PR #33c).  See [Known limitations](#known-limitations-in-the-release-flow) below.
- 2026-04-22 — Pipeline hardening batch:
  - Added concurrency groups to `ci.yml` and `release.yml` so
    superseded PR runs are cancelled and release dispatches queue
    cleanly.
  - Renamed `optimized-ci.yml` → `tier-2.yml` for clarity.
  - Added **Tier 2 / Windows Compile Check** on `windows-latest` so
    Windows regressions surface weekly, not 10-15 minutes into a
    `just ship` release run.
  - Added **CycloneDX 1.5 SBOMs** to every release: one
    `sbom-<crate>.cdx.json` per workspace crate, covered by the
    same SLSA build-provenance attestation as the binaries.
  - Added **CodeQL (Rust SAST)** workflow on PR + weekly
    schedule.  Rust support is in public preview (CodeQL ≥ 2.22.1)
    so the check is not a required gate yet; promoted after a
    few weeks of clean baselines.
  - Split `notify-failure` labels per workflow
    (`ci-failure-tier-1`, `ci-failure-tier-2`,
    `ci-failure-release`) so a release failure is never buried as a
    comment on a week-old Tier 2 flake issue.
  - **Narrow Dependabot auto-merge** (`dependabot-auto-merge.yml`)
    — patch-level bumps with no active security advisory queue for
    auto-merge once all required checks + branch-protection rules
    are satisfied.  Minor, major, and security-advisory bumps keep
    the existing manual-review path.  Net effect: reviewer time
    reclaimed for the bumps that actually carry review signal.

This document captures UFFS's current supply-chain defences, the threat
model they address, and the concrete gaps that are deferred (with
rationale).  It's a living reference — update it whenever a new
control lands or a deferred item is promoted.

---

## Layered defences in place

| Layer | Tool | Scope | Cadence | CI gate? |
|---|---|---|---|---|
| Known CVE detection | `cargo-deny` `[advisories]` | Full dep tree vs RustSec DB | Every PR | **Yes** — `ci.yml` Security job |
| License policy | `cargo-deny` `[licenses]` | Permitted list (MIT, Apache-2.0, MPL-2.0, …) | Every PR | **Yes** — same job |
| Source whitelist | `cargo-deny` `[sources]` | crates.io + pinned polars git | Every PR | **Yes** — same job |
| Workflow permissions | `permissions:` in every workflow | Minimal (`contents: read` default, `write` only where proven needed) | Reviewed per PR | N/A |
| Concurrency hygiene | `concurrency:` groups on every workflow | Cancel superseded PR runs; queue (never cancel) release / scheduled runs | Every workflow | N/A |
| Tag integrity | `main-protection` + `tag-protection-v-prefix` rulesets | Required reviews + signed commits on main; `v*` tag deletion/update blocked | Always | GitHub enforces |
| Build-provenance | `actions/attest-build-provenance` | Every release asset signed with Sigstore OIDC | Every `v*` release | **Yes** — release.yml |
| SBOM | `cargo-cyclonedx` → CycloneDX 1.5 JSON | One SBOM per workspace crate, shipped alongside binaries; covered by SLSA attestation | Every `v*` release | **Yes** — release.yml |
| Commit ancestry check | Custom step in `release.yml` | `workflow_dispatch` `commit_sha` must be ancestor of main | Every release dispatch | **Yes** — release.yml |
| Dep-tree growth | `dependabot-review.yml` | Cargo.lock crate-count delta on Dependabot PRs | Every Dependabot PR | Annotation only |
| Lockfile pinning | Committed `Cargo.lock` | Every resolved crate-version frozen across devs / CI / releases | Always | **Yes** — `cargo vet check --locked` would fail any drift |
| Audit trail | `cargo-vet check --locked` | Every resolved crate-version must have import / own audit / exemption | Every PR | **Yes** — `pr-fast.yml` security job |
| Audit discipline | `scripts/ci/check_vet_audit_discipline.sh` | Every exemption version-bump must have a matching delta audit AND a `Vet-Reviewed-Diff:` commit trailer | Every push touching `supply-chain/config.toml` | **Yes** — pre-push hook + `pr-fast.yml::security` |
| Unused-dep hygiene (static) | `cargo-machete` | Every `[dependencies]` / `[dev-dependencies]` / `[build-dependencies]` entry must be `use`d somewhere in the workspace | Every code-changed push / PR | **Yes** — pre-push hook + `pr-fast.yml::security` (Tier R3-01, landed 2026-05-12) |
| Unused-dep hygiene (compiled) | `cargo-udeps` | Same scope as machete but compiles to detect — catches `#[cfg]`-gated false-negatives machete's grep misses | Weekly | **Yes** (advisory) — `tier-2.yml::udeps` |
| Broad UB / std-debug coverage | `cargo-careful` | Rebuilds `std` with extra debug assertions (slice / iterator bounds, integer overflow, RefCell borrow violations) and runs scoped tests against the augmented `std`.  Complements Miri's narrow deep-dive (4 specific raw-persistence tests) with broad coverage of the two unsafe-density hotspots: `uffs-security` (crypto / keystore) and `uffs-mft` (raw NTFS parsing) | Weekly | **Yes** (advisory) — `tier-2.yml::careful` (Tier R3-03, landed 2026-05-12) |
| Test-suite quality (mutation) | `cargo-mutants` | For every reachable function in `uffs-security`, generate small source mutations (operator swaps, return-value flips, etc.) and run the test suite against each.  Missed mutations indicate test gaps in safety-critical paths (crypto, keystore, runtime-dir privileges).  Output uploaded as `cargo-mutants-output-*` artifact (30-day retention) for offline triage | Weekly | **Yes** (advisory; informational baseline rollout) — `tier-2.yml::mutants` (Tier R3-04, landed 2026-05-12) |
| Feature-matrix compile coverage | `cargo-hack --each-feature` | For every workspace crate with a `[features]` block, type-check each feature in isolation plus `--no-default-features`.  Catches `use <crate-behind-feature>` added without `#[cfg(feature = "...")]` gating — invisible to `--all-features` builds | Weekly | **Yes** (advisory) — `tier-2.yml::hack` (Tier R3-02, landed 2026-05-12) |
| SemVer compliance | `cargo-semver-checks check-release` | For every publishable crate, diff the current rustdoc JSON against the latest version on crates.io.  Catches SemVer-breaking API changes (removed pub items, signature changes, new required generic params, etc.) shipped under a non-major version bump.  Informational pre-R8 (no crates.io baseline yet); becomes the canonical correctness check once the first publish lands | Weekly | **Yes** (advisory pre-R8, then GATE) — `crates-io-dry-run.yml` (Tier R3-05, landed 2026-05-12) |
| MSRV verification | `cargo +1.91 check --locked` (`--all-features` + `--no-default-features`) | Independently verifies the workspace builds on the claimed MSRV (`Cargo.toml::rust-version = "1.91"`) against the **stable** 1.91 toolchain. `rust-toolchain.toml` pins nightly so every other gate runs there; this is the only gate that exercises the contract surface downstream consumers see. Catches MSRV regressions that `clippy::incompatible_msrv` misses (new trait impls, deref coercion changes, new `const fn` qualifications, inference improvements). Hard gate \u2014 a failure means the rust-version claim is lying to crates.io consumers | Weekly | **Yes** (hard gate) \u2014 `tier-2.yml::msrv` (Tier R3-06, landed 2026-05-12) |
| Adversarial-input fuzzing | `cargo-fuzz` (libfuzzer-sys) | Runs libfuzzer-sys harnesses against the workspace's primary untrusted-byte attack surface: `uffs_mft::parse::{parse_record,parse_record_full,parse_record_zero_alloc,apply_fixup}`.  Every NTFS MFT record we parse came from an external filesystem image — fuzz tests robustness to malformed bytes (truncated records, cyclic FRS references, integer overflows in metadata fields, slice-bounds violations in the fixup application).  Complements Miri / cargo-careful (UB on *known* inputs) and cargo-mutants (test-suite quality on *known* inputs) by generating *unknown* adversarial inputs.  Time-bounded (`-max_total_time=900` per target, 30 min total).  Artifacts (`crashes/` + `corpus/`) uploaded for 90 days for offline triage and seed carry-over | Weekly | **Yes** (advisory; informational baseline rollout) — `tier-2.yml::fuzz` (Tier R3-07, landed 2026-05-12) |
| Import refresh | `cargo-vet-refresh.yml` | Weekly `cargo vet regenerate imports` → job-summary report (no PR) | Mondays 08:00 UTC | GitHub schedules |
| Structural audit | `cargo-geiger` via `just geiger` | unsafe / build.rs / proc-macro footprint | On-demand (monthly) | No |
| Semantic SAST | `codeql.yml` (Rust, public preview) | Dataflow-based bug patterns (path / SQL / regex injection, crypto misuse, unvalidated redirects) | PR + Tuesdays 06:30 UTC | Informational (not a required gate yet) |
| Windows regression check | `pr-fast.yml::windows-lint` job | `cargo clippy --workspace --all-targets --all-features --locked --no-deps -- -D warnings` on `windows-latest` | **Every PR** (was weekly Tier 2 pre-PR-#138; now PR-time) | GitHub-required-check |
| Human review for minor/major bumps | Dependabot PRs for minor + major + security advisories are NOT auto-merged | Minor / major / security-advisory bumps | Every Dependabot PR | GitHub enforces |
| Gated auto-merge for patch bumps | `dependabot-auto-merge.yml` | Only `version-update:semver-patch` bumps with NO active security advisory, gated on all required checks (cargo-deny, cargo-vet, clippy, tests, doc-tests, file-size policy) + branch-protection rules (signed commits, required reviews) | Every Dependabot PR | Gates enforced via required checks + branch protection |

## Threat model and coverage matrix

| Threat | Severity | Current control | Residual risk |
|---|---|---|---|
| Known CVE in a dep | High | `cargo-deny` RustSec DB on every PR | Low — CI gates |
| Unknown (zero-day) CVE | High | None specific; detection delayed | Medium — industry-wide |
| Typo-squatted dep added via PR | Medium | `deny.toml` `[sources]` allow-list + `unknown-git = warn` | Low |
| Maintainer-account compromise → silent minor-version malicious bump | **High** | `cargo-vet check` requires upstream or local audit for the new version; human review mandatory for minor+ / security-advisory bumps; `dependabot-review.yml` tree-growth annotation + human review of diffs.  Patch-level auto-merge is gated on cargo-vet + cargo-deny + the dep-tree-growth annotation, so a malicious patch still hits the same audit-trail wall as a manual-merge path | **Low-medium** — cargo-vet covers most crates via Mozilla/Google imports; residual is the first N days after a malicious version lands until upstream audits it (or we audit locally) |
| Malicious `build.rs` / proc-macro executing in CI | **High** | Dependabot PRs run with **read-only `GITHUB_TOKEN`** + no repo secrets (GitHub default); `permissions:` block denies writes elsewhere; `cargo-vet` imports from Mozilla/Google are the primary vetting signal for new build-script crates | **Medium** — blast radius bounded (runner has no sensitive tokens); new unaudited crate bumps are caught by `cargo vet check`, forcing a conscious decision |
| Release binary swapped on GitHub Release page | Medium | SHA256 `CHECKSUMS.txt` + SLSA build-provenance attestation via `gh attestation verify` | Low — requires attacker to also swap attestation, which is stored in GitHub Attestations API (separate audit trail) |
| SBOM swapped on GitHub Release page (misleading component inventory) | Medium | `sbom-*.cdx.json` files are covered by the same SLSA attestation as the binaries — `gh attestation verify` on the SBOM matches only if the bytes match this workflow run | Low — inherits the binary-swap residual |
| Windows-only build regression lands on main and only surfaces at release time | Low | `pr-fast.yml::windows-lint` job runs strict `cargo clippy -- -D warnings` natively on `windows-latest` on every PR (post-PR-#138 / Phase W5.5).  `cargo clippy` does a full type-check + executes every dep's `build.rs`, so any Windows-only build regression is caught at PR-time | Very low — minutes-scale detection latency, hard-gates merge to `main` |
| Semantic bug class (path / SQL / regex injection, crypto misuse) slipping past clippy | Medium | `codeql.yml` Rust SAST on every PR + weekly baseline | Medium — Rust support is in public preview; false-negative rate unknown |
| Rollback attack (release an older vulnerable commit) | Medium | `commit_sha` ancestor-of-main guard in `release.yml` | Low |
| Rogue `v*` tag push by write-access user | Low | `tag-protection-v-prefix` ruleset blocks deletion + update | **Medium on creation** — GitHub API does not support `Integration` bypass for user-owned repos, so `creation` rule not enforced (bot couldn't push tags if it were).  Partial protection only. |
| Compromised runner (GitHub Actions infra itself) | Low | None — SLSA L2 attestation trusts the runner | Industry-wide; accept |

## Explicitly deferred (with rationale)

### Reproducible builds + independent rebuild verifier

**Status**: Deferred indefinitely.

**What it would protect against**: Compromise of the GitHub-hosted
runner itself (the step the SLSA L2 attestation has to trust).

**Why deferred**: Requires pinning OS-image SHAs, `SOURCE_DATE_EPOCH`,
deterministic toolchain.  Worth ~3-5 days of work for L3.  The
threat is industry-wide and low-probability at UFFS's scale.

### Code signing (Authenticode / `codesign`)

**Status**: Deferred indefinitely.

**What it would protect against**: Users who rely on the OS's
SmartScreen / Gatekeeper reputation signal rather than
`gh attestation verify`.

**Why deferred**: $99-$200/yr cost + key-rotation discipline.  SLSA
attestation covers the technical threat for the audience who
verifies; deferring the UX-layer signing until there's enterprise
demand.

## Mandating audits over blanket bumps

**Origin story**: PR #166 (a routine Dependabot patch-bump roll-up)
broke `cargo vet check` because `assert_cmd 2.2.1 -> 2.2.2` and
`tokio 1.52.2 -> 1.52.3` were neither imported via Mozilla/Google nor
locally audited.  The path-of-least-resistance fix was to bump the
existing `[[exemptions.<crate>]]` `version =` line so vet would
re-pass — and that's exactly what PR #166 merged.  PR #170 reversed
the silent rubber-stamp manually: it anchored each exemption back at
the pre-bump version and added a proper `[[audits.<crate>]]` `delta`
block reviewing the actual upstream changes.

The lesson: **every exemption bump is a future audit-debt entry, and
the cargo-vet workflow is biased toward the bump over the audit**.
Without explicit guard-rails, the audit ledger silently grows shallower
with every dependabot run.

This section documents the four-layer policy that makes the audit the
default and the bump the exception.

### Layer 1 — CI gate (`check_vet_audit_discipline.sh`)

The script `scripts/ci/check_vet_audit_discipline.sh` parses the
`[[exemptions.*]]` blocks in `supply-chain/config.toml` at both the
diff base and HEAD, identifies every `(crate, version)` pair that is
new in HEAD but had a different prior version at base (a "bump"), and
verifies for each bump that:

- **(A)** `supply-chain/audits.toml` contains a `[[audits.<crate>]]`
  block with `delta = "<OLD> -> <NEW>"` (or the reverse — the gate is
  direction-insensitive so that "anchor-restoration" bumps like
  PR #170's `2.2.2 -> 2.2.1` are accepted when the matching forward
  delta exists).
- **(B)** at least one commit in the push range carries a
  `Vet-Reviewed-Diff: <crate>@<OLD>-><NEW>` trailer (or the reverse).

Both checks must pass.  A passing audit without a trailer means the
record predates this push (potentially stale); a passing trailer
without an audit means the reviewer skipped the formal `cargo vet
certify` step.

**Scope**: only **BUMPs** (existing crate, version change) are gated.
**ADDs** (brand-new exemption for a crate that wasn't previously
exempt) are skipped to keep `cargo vet regenerate exemptions` working
when a new transitive dep appears — those are caught by Layer 3
(CODEOWNERS).  **REMOVEs** are always good (audit-debt going down).

**Tiers**: pre-push (`scripts/hooks/_lint_pre_push.sh`, `spawn_bg`
bucket, runs concurrently with `cargo vet check`) + `pr-fast.yml`'s
`security` job (folded in with `cargo deny` and `cargo vet check`).
Both consult the same script for byte-identical semantics.

**Bypass**: `BYPASS_VET_AUDIT_DISCIPLINE=1 git push` at pre-push only
(prints a yellow `[WARN]` banner to stderr; the operator's intent is
visible in the push log).  CI has no bypass — the gate is hard on
`main`.

### Layer 2 — `just vet-bump <crate>` driver

The `just vet-bump <crate>` recipe (`just/security.just`, backed by
`scripts/ci/vet_bump.sh`) is the operator's easy button.  It detects
the current exemption version, the version Cargo.lock now wants, and
the criteria level the exemption holds, then prints the exact
four-step recipe to perform the audit cleanly:

1. `cargo vet diff <crate> <OLD> <NEW>` — manual diff review.
2. `cargo vet certify <crate> <OLD> <NEW> --criteria <X>` — record
   the audit with notes (cargo-vet opens `$EDITOR`).
3. `git commit -S --trailer 'Vet-Reviewed-Diff: <crate>@<OLD>-><NEW>'
   -m '…'` — commit with the required trailer.
4. `cargo vet check --locked` + `just vet-discipline` — local CI
   parity check.

The recipe deliberately **does not** mutate any file or run cargo
subcommands on its own — every step is operator-visible.  This keeps
the audit narrative in the operator's head (and in the commit `notes`
field), which is the whole point of mandating audits over rubber
stamps.

### Layer 3 — CODEOWNERS on `supply-chain/`

A single `supply-chain/                       @githubrobbi` line in
`.github/CODEOWNERS` ensures that **every** PR touching the directory
(exemption add, exemption bump, audit add, imports refresh — every
case the previous layers may or may not gate) routes to maintainer
review.  This is the human backstop for cases the script misses (new
exemption ADDs, audit `notes` quality, semantic correctness of the
`safe-to-*` criteria chosen).

### Layer 4 — `Vet-Reviewed-Diff:` commit trailer

The trailer's syntactic shape (`<crate>@<OLD>-><NEW>`, no quoting, no
nesting) matches the kernel-style `Signed-off-by:` workflow that
`git commit --trailer` already understands.  Branch protection on
`main` requires signed commits, so the trailer is implicitly attached
to the operator's commit-signing key.  The gate parses trailers via
`git log --format=%B` + grep (portable across git ≥ 1.x), so it
works on every supported runner.

### How the four layers compose

```
   PR opens with exemption bump
            │
            ▼
   ┌────────────────────┐
   │ Layer 1: CI gate   │  audit + trailer both present?   ── no ──► RED check, no merge
   └────────┬───────────┘
            │ yes
            ▼
   ┌────────────────────┐
   │ Layer 3: CODEOWNERS│  maintainer review required      ── no ──► no approval, no merge
   └────────┬───────────┘
            │ yes
            ▼
   ┌────────────────────┐
   │ Branch protection  │  signed commits + required reviews
   └────────┬───────────┘
            │ yes
            ▼
         merge to main
```

Layer 2 makes the `yes` paths cheap (1 minute, not 15).  Layer 4 is
the audit trail that future incident-response reviewers will read.

### What if I really need to skip?

The pre-push escape hatch (`BYPASS_VET_AUDIT_DISCIPLINE=1`) exists for
emergencies: on-call hot-fix where the operator is at an airport
without `cargo vet` installed.  Its use is visible in the operator's
shell history and in the pre-push stderr, but **not** in the commit
itself — meaning the bypass cannot bridge to a merge unless the
operator also opens a follow-up PR landing the audit.  CI does not
honour the bypass: any merge to `main` must satisfy the discipline
gate.

## Lockfile pinning (`Cargo.lock`)

As of 2026-04-22, `Cargo.lock` is committed to the repository
(previously in `.gitignore`).  This is the standard recommendation
for binary-crate workspaces per the [Cargo book](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html),
but for UFFS specifically it plays three supply-chain roles:

### Role 1 — Reproducible builds

Every developer clone, every CI run, every release build now resolves
the identical dep graph.  Before, a fresh CI runner without a
lockfile would `cargo generate-lockfile` on the spot, picking up
whatever versions crates.io had published in the meantime.  This
made the binary that got built from "commit X" non-deterministic,
which in turn weakened the SLSA attestation's "I built this
artifact from these sources" claim.

### Role 2 — `cargo-vet` stability

`cargo-vet` exemptions are keyed on specific `crate@version` pairs.
With a floating lockfile, any transitive dep getting a patch
release on crates.io (literally hundreds per week across our ~500
transitive deps) would fail CI's `cargo vet check` for reasons
unrelated to our PRs.  We hit this concretely on PR #33b when
`pastey 0.2.2` was published between `cargo vet init` and the
first CI run — the PR was blocked on an "unvetted" dep we hadn't
intentionally upgraded.  Committing the lockfile eliminates this
class of spurious failures; real bumps now only come via
deliberate `cargo update` / Dependabot PRs.

### Role 3 — Dependabot review surface

With the lockfile in-tree, every Dependabot PR visibly changes
`Cargo.lock` — the `dependabot-review.yml` workflow's tree-growth
annotation has a real artifact to compare against.  The ~500-line
`Cargo.lock` diff a bump produces is also a quick skim for
unexpected fan-out.

### Cost

- **Repo size**: one-time +300 KB (5512-line `Cargo.lock`).
- **Merge conflicts**: Dependabot PRs may conflict with each
  other on `Cargo.lock`.  Mitigation: merge Dependabot PRs one at
  a time (we already review them manually per
  [Operational playbook](#per-dependabot-pr-review-5-10-min),
  so the cost is zero).

## Known limitations in the release flow

### `softprops/action-gh-release` 403 on non-HEAD commits

**Symptom**: `Create GitHub Release` step fails with

```
GitHub release failed with status: 403
{"message":"Resource not accessible by integration"}
```

even though `permissions: contents: write` is set on the workflow.

**Root cause**: documented in
[softprops/action-gh-release README](https://github.com/softprops/action-gh-release):

> When creating a new tag for an older commit, `github.token` may
> not have permission to create the ref; use a PAT or another
> token with sufficient contents permissions if you hit
> `403 Resource not accessible by integration`.

This is a hardcoded restriction of the GitHub Apps token model:
`GITHUB_TOKEN` can create refs pointing at the default branch's
current HEAD, but not at arbitrary older commits.

**When this surfaces for UFFS**:

- Smoke tests dispatched against an old commit (e.g. we replay
  `release.yml` against a pre-fix commit to verify a fix works)
  — always fails.
- Real `just ship` flow where another commit lands on `main`
  during the ~10-15 min release build window — `commit_sha`
  stops matching `main` HEAD by the time softprops runs.
  Theoretically possible but rare at current solo-maintainer
  merge cadence (~1 PR/day).

**Mitigation**: accept and document.  If we ever hit the race in
practice, operator response is "re-dispatch `release.yml`, do not
merge during release window".  A PAT-based workaround would
re-introduce a supply-chain secret for a single edge-case — not
worth the trade.

**Separate from** the workflow-file-protection race that PR #32
fixed (which was `remote rejected: refusing to allow a GitHub App
to create or update workflow ... without 'workflows' permission`
at `git push origin v*` time).  That path is closed; softprops
now creates the tag via REST API, which has no workflow-file
guard.

## Baseline metrics

### cargo-geiger (2026-04-22, `uffs-cli` full feature set)

Aggregate unsafe footprint across the resolved dep tree:

```
Functions  Expressions  Impls  Traits  Methods
862/2072   73458/112403 1285/2290 106/112 2688/3519
```

Top-10 `unsafe`-heavy crates (by function count):

| Crate | Used/Total fns | Used/Total exprs | Role |
|---|---|---|---|
| `rustix 1.1.4` | 44/436 | 1560/7539 | syscall bindings |
| `objc2-core-foundation 0.3.2` | 66/185 | 1143/2829 | Apple platform bindings |
| `libc 0.2.185` | 1/92 | 10/725 | C library bindings |
| `portable-atomic 1.13.1` | 16/87 | 122/633 | atomics polyfill |
| `blake3 1.8.4` | 1/84 | 32/4365 | crypto (SIMD) |
| `sysinfo 0.37.2` | 12/73 | 1018/5802 | OS info |
| `polars-arrow 0.53.0` | 69/69 | 5226/5271 | columnar engine |
| `argminmax 0.6.3` | 60/60 | 2854/3391 | SIMD min/max |
| `zlib-rs 0.6.3` | 41/49 | 2594/3481 | compression (pure Rust) |
| `tokio 1.52.1` | 26/30 | 2270/2912 | async runtime |

**Interpretation**: every top-10 crate is either (a) platform bindings
that must use unsafe by definition, (b) SIMD-heavy performance code
in well-known widely-audited crates, or (c) foundational async/crypto
from the Rust ecosystem's top-tier maintainers.  **No surprises.**
None of the crates appearing here warrant a focused manual audit at
this time.

**How to refresh**: `just geiger` (writes dated report to
`target/geiger-YYYYMMDD.txt`).  Review monthly; flag any new top-20
entry that isn't obviously platform / SIMD / foundational.

### cargo-vet coverage (2026-04-22)

From `cargo vet check` immediately after init:

```
Vetting Succeeded (121 fully audited, 2 partially audited, 387 exempted)
```

**Interpretation**:

- **121 fully audited** — covered by an imported audit record from
  Mozilla / Google / Bytecode Alliance / ISRG / Zcash.  Zero
  maintenance on our side; refreshed weekly.
- **2 partially audited** — some criteria (`safe-to-deploy` vs
  `safe-to-run`) covered by imports, others exempted.
- **387 exempted** — grandfathered in at init time.  The weekly
  refresh workflow auto-prunes any exemption that becomes covered
  by a fresh upstream audit (numbers should trend down over
  months).

**Target trajectory**:

- 2026-04 (initial): 121 audited / 387 exempted (24% coverage)
- 2026-10 (6 months): aim for >40% coverage via upstream refreshes
- Long-term: >70% coverage via upstream; remaining exemptions are
  niche / rare-maintainer crates that are unlikely to be audited
  upstream.

**Ongoing cost**:

- Dependabot PR where the bump is covered by upstream: **0 min**
  (sails through CI).
- Dependabot PR where the bump isn't covered: **~2 min** to run
  `cargo vet regenerate exemptions && git commit --amend` locally
  and push.  Weekly refresh minimises how often this happens.
- Weekly refresh report: **~5 min** to skim the reported diff and apply
  it on the next working branch (`cargo vet regenerate imports &&
  cargo vet prune`).

### Cargo.lock crate count (2026-04-22)

<!-- Populate with actual count next time you run `grep -c '^name = ' Cargo.lock` -->
See the Dependabot-review workflow summary for the current value.  The
`dep-tree-growth` check compares each Dependabot PR against the value
on `main` at branch-open time.

## Operational playbook

### Monthly review (~30 min)

1. `just geiger` — regenerate report, diff against previous month's
   `target/geiger-YYYYMMDD.txt`.
2. Scan the top-20 for new entries; investigate any that aren't
   platform / SIMD / foundational.
3. Review any Dependabot PRs that are still open with
   `dep-tree-growth` warnings.
4. Glance at the GitHub Security tab for new Dependabot alerts.

### Per-Dependabot-PR review (~5-10 min)

1. Read the PR description (Dependabot summarises the changelog).
2. Check the `dep-tree-growth` annotation — if flagged, inspect the
   "Newly-resolved crate names" summary.
3. Check the `cargo vet check` CI status — if red, the bump isn't
   covered by any imported audit.  Fix locally with the **audit-first**
   recipe (the lazy `cargo vet regenerate exemptions` fallback is now
   blocked by `check_vet_audit_discipline.sh` — see §"Mandating audits
   over blanket bumps" below):
   ```bash
   gh pr checkout <N>
   cargo vet regenerate imports     # try upstream first
   cargo vet check                  # still red?
   just vet-bump <crate>            # guided audit (Layer 2)
   # ...edit cargo vet certify notes, then commit with trailer:
   git commit -S \
       --trailer 'Vet-Reviewed-Diff: <crate>@<old>-><new>' \
       -am 'chore(security): audit <crate> <old> -> <new>'
   git push
   ```
   The legacy `cargo vet regenerate exemptions` shortcut is only
   appropriate when a brand-new transitive crate appears with no
   prior exemption — the new gate skips ADDs by design (Layer 3 is
   the CODEOWNERS review for those cases).  For any version-bump of
   an *existing* exemption, the audit-first recipe is mandatory.
4. `gh pr diff <N>` and scan `Cargo.lock` for unexpected additions.
5. If the bump is a known crate and the diff looks clean, merge.
6. If anything looks off — typo-squat, sudden fan-out, unexplained
   git source change — ask the maintainer upstream before merging.

### Incident response (suspected compromise)

1. Disable auto-merge on all open PRs: `gh pr list --state open
   --json number --jq '.[].number' | xargs -I {} gh pr merge {}
   --disable-auto`.
2. Pin the suspected crate to its last-known-good version via
   `[patch.crates-io]` in workspace `Cargo.toml`.
3. Record a `cargo-vet` violation for the compromised version so
   future runs fail loudly:
   ```bash
   cargo vet record-violation --criteria safe-to-deploy \
     <crate> <bad-version>
   ```
4. Run `cargo audit` and `cargo deny check` locally for
   cross-confirmation.
5. Run `just geiger` and diff against the last monthly baseline —
   look for unexpected unsafe-count spikes in the suspected crate.
6. Rotate the repo's `GITHUB_TOKEN` (via GitHub settings) if any
   workflow had `write`-level scopes during the suspected window.

## References

### In-repo

- `deny.toml` — `cargo-deny` policy
- `supply-chain/config.toml` — `cargo-vet` imports + exemptions
- `supply-chain/audits.toml` — our local audit records (starts empty)
- `supply-chain/imports.lock` — pinned upstream audit snapshots
- `scripts/ci/check_vet_audit_discipline.sh` — Layer 1 + Layer 4 of the four-layer audit-discipline policy
- `scripts/ci/vet_bump.sh` + `just vet-bump` — Layer 2 guided audit driver
- `.github/CODEOWNERS` (`supply-chain/` line) — Layer 3 mandatory human review
- `.github/workflows/pr-fast.yml` — required per-PR gate (fmt, clippy, docs, tests, **windows-lint** [native `cargo clippy -- -D warnings` on `windows-latest` post-W5.5], `cargo-deny` + `cargo-vet check` in `security` job) (Tier 1)
- `.github/workflows/tier-2.yml` — weekly coverage / udeps / Miri (Windows compile check removed in PR #138 — superseded by `pr-fast.yml::windows-lint`'s per-PR strict clippy)
- `.github/workflows/release.yml` — SLSA attestation + ancestor check + CycloneDX SBOM
- `.github/workflows/codeql.yml` — Rust SAST (public preview)
- `.github/workflows/dependabot-review.yml` — dep-tree growth annotation
- `.github/workflows/dependabot-auto-merge.yml` — patch-level auto-merge gate
- `.github/workflows/cargo-vet-refresh.yml` — weekly imports refresh
- `.github/workflows/auto-tag-release.yml` — tagging bridge
- `just/analysis.just` — `just geiger` / `just geiger-summary` recipes

### External

- GitHub [Attestations API](https://docs.github.com/en/actions/security-guides/using-artifact-attestations-to-establish-provenance-for-builds)
- SLSA [build-provenance v1](https://slsa.dev/spec/v1.0/provenance)
- cargo-vet [user guide](https://mozilla.github.io/cargo-vet/)
- Mozilla's [supply-chain audits](https://github.com/mozilla/supply-chain)
- Google's [supply-chain audits](https://github.com/google/supply-chain)
- CycloneDX [1.5 specification](https://cyclonedx.org/specification/overview/)
- [cargo-cyclonedx](https://github.com/CycloneDX/cyclonedx-rust-cargo)
- CodeQL [Rust public preview changelog](https://github.blog/changelog/2025-07-02-codeql-2-22-1-bring-rust-support-to-public-preview/)
