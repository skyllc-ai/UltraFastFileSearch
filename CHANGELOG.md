<!--
SPDX-License-Identifier: MPL-2.0
Copyright (c) 2025-2026 SKY, LLC.

UFFS - Ultra Fast File Search
-->

# Changelog

All notable changes to UFFS will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.46] - 2026-10-29

### Fixed

- daemon: stop journal saves from taking the machine under heavy churn
- daemon: seed the startup cursor only after the rebuilt cache is written
- broker: keep a client that connected before ConnectNamedPipe
- status: report a re-warming drive truthfully and count its promotions
- cli: wait up to 10 minutes for a search, silently, and name what did not finish
- deps: rustls 0.23.46, rustls-webpki 0.103.16, ctutils 0.4.6 and the zstd 0.14 line for RUSTSEC advisories
- lints: give every #[deprecated] the version that deprecated it

### Performance

- daemon: re-warm a parked drive from its cache plus the journal
- daemon: write saves without copying the index; skip near-empty age saves
- cache: load caches with one buffer per stage, sized exactly

## [0.6.45] - 2026-10-13

### Fixed

- vss-requestor: retry the real freeze/flush timeouts, not two transposed codes
- lints: clear every non-upstream finding from the floating-nightly canary

## [0.6.44] - 2026-10-03

### Added

- cli: make --benchmark measure output production; -v shows the rows

### Changed

- daemon: never force a tier change on memory pressure

### Fixed

- client: report the client's own deadline as Timeout, never as a warm-up retry
- daemon: surface search timeouts as errors and cancel the orphaned scan
- daemon: log the per-tick USN refresh at debug, not info

## [0.6.43] - 2026-10-03

### Added

- cli: make --benchmark measure output production; -v shows the rows

### Changed

- daemon: never force a tier change on memory pressure

### Fixed

- client: report the client's own deadline as Timeout, never as a warm-up retry
- daemon: surface search timeouts as errors and cancel the orphaned scan
- daemon: log the per-tick USN refresh at debug, not info

## [0.6.42] - 2026-10-03

### Fixed

- winget: follow the demo binary rename, and never declare a file the zip lacks
- cli: honour --benchmark as "skip output", not only --no-output

## [0.6.41] - 2026-09-19

### Fixed

- mft: read the MFT bitmap and boot sector through the bounded overlapped path
- iocp: bound every completion wait and fail instead of stalling forever
- mft: stop logging the MFT layout from the extent-map constructor
- client: make the async RPC deadline cover the whole round trip
- broker: restart after a kill, and keep the service's own log
- logging: bound the daemon and MCP log files
- deps: take rustls 0.23.45 for RUSTSEC-2026-0285

## [0.6.40] - 2026-08-31

### Changed

- cli: move the four pre-#436 command entry points out of main.rs

## [0.6.38] - 2026-08-24

### Added

- daemon: warm_plan RPC - the bloom-aware cold-index answer external gates must use

### Fixed

- dev: single-pass install build — capture the artifact JSON from the real build
- daemon: lifecycle idle-timer tests on tokio's paused clock
- client: UFFS_NO_AUTOSTART kill-switch so tests never spawn uffsd
- core: compact-cache poisoning via delta-carrying saves and the empty-trigram reader gap

## [0.6.37] - 2026-08-22

### Fixed

- mcp: stamp SEP-2549 ttlMs/cacheScope on every result — protocol 2026-07-28 clients rejected tools/list
- dev: hermetic cache-key for cargo-spawned runs — kills the Keychain-prompt test hang

## [0.6.36] - 2026-08-22

### Fixed

- mft: offset-read fallback rung on the existing volume handle — broker-mode
  full MFT reads no longer dead-end when the file object's one-shot IOCP
  association is already claimed (the "drive stuck Cold under zero-UAC"
  incident of 2026-08-22)
- core: corruption-class compact-cache load failures quarantine the poisoned
  file as `<name>.corrupt` so the next load rebuilds from scratch instead of
  failing on the same bytes forever
- core: the parked-tier (bloom + trie) loader joins the same self-heal —
  corruption-class failures quarantine the file instead of folding into a
  silent `None`
- mft: the `.uffs.tmp` sweep in `cache_dir()` is age-gated (1 h) — it no
  longer deletes temp files an in-flight background cache save is still
  writing (root cause of the intermittent "Background compact cache save
  failed" os error 2)

## [0.6.35] - 2026-08-21

### Added

- daemon: changed_since RPC — USN-journal delta as first-class daemon surface

### Fixed

- cli: remediate from_utf8_lossy decision sites (anti-pattern gate)
- mft: bounds-check truncated attribute headers — nightly fuzz find

## [0.6.33] - 2026-08-14

### Added

- mcp: report re-warm progress against a real record denominator
- daemon: report promotion_ms so a cold query stops looking instant

### Changed

- watchdog: cheap liveness probe and an interval that breathes

### Fixed

- mcp: cold-index queries return a warming contract instead of hanging
- mcp: expose per-drive tier so the warm-retry contract is followable
- mcp: report residency numbers beside the tier map in uffs_status
- dev: stop use-local killing AI-host MCP sessions to unlock binaries
- cli: name the real tier for a demoted drive, not always 'parked'
- dev: drop let-chains from install-bins (rust-script default edition)
- cli: make the MCP stdio session scan work on Windows
- cli: separate gateway, supervisor and worker in the stdio session list
- cli: show resident-vs-expected records while drives are demoted
- mcp: one readiness field, and name the drive that is loading
- version: re-stamp the -dirty flag when the tree changes, not just HEAD
- ci: clamp RLIMIT_NOFILE before zig links, unblocking toolchain-sync
- ci: clamp the fd limit when ulimit reports "unlimited" too
- ci: raise the fd limit for zig links — macOS defaults to 256

## [0.6.32] - 2026-08-13

### Added

- watchdog: user-level supervisor for the resident daemon + MCP
- watchdog: wire supervision into the service lifecycle

### Fixed

- cli: align the daemon status tables
- dev: use-local skips unchanged binaries and cycles the broker service
- dev: use-local restarts the daemon it stopped
- dev: use-local stops the MCP cleanly and restarts it
- cli: align the per-drive component breakdown
- cli: align the physical-drive table past the free-space column
- watchdog: record stop intent BEFORE the shutdown, not after
- watchdog: stop supervisor restarts erasing stop intent; add a log
- watchdog: read liveness per service so a stopped daemon stays stopped
- core: bound column growth so one new file cannot double the index
- aggregation: honour every search filter, not four of them

## [0.6.31] - 2026-08-11

### Added

- fetch: extract public uffs-fetch transport lib; dep refresh + rmcp 3 (#583)
- self-upgrading MCP supervisor, permanent daemon residency, and a repo health pass (#588)

## [0.6.30] - 2026-07-23

### Added

- mft: for_each_record — streaming fixed-up targeted record reads (#577)

### Fixed

- mft: real overlapped re-open for snapshot devices + snapshot-correct $MFT fallback (#578)
- mft: demote IOCP-fallback chain messages from WARN to INFO (#579)
- version: re-stamp build info on every HEAD movement via the reflog

## [0.6.29] - 2026-07-22

### Added

- core: expose apply_search_filters as a public API (#568)
- mft: extended $SI fields, parse status, and macOS target-cpu fix (#574)

### Fixed

- test: stop asserting concurrency via a wall-clock threshold (#566)
- ci: don't let a transient GitHub API error fail the commitlint gate (#565)
- mft: default-pipeline MFT correctness gaps + panic hardening (#569)
- core: drive-qualify aggregate rollup and duplicate keys (#575)

## [0.6.28] - 2026-07-19

### Added

- content: VSS-snapshot content-ingest pipeline for Docenta (UFI.0-UFI.6) (#563)

### Fixed

- test: deflake ensure_warm parallel-promote assertion (#562)

## [0.6.27] - 2026-07-14

### Added

- status: physical drive view + nightly-run fixes (fuzz, secure_remove, udeps) (#556)
- delete-visibility: --diff / --deleted / --snapshot — deleted-file search + forensic tombstone read (#559)

### Fixed

- mft: use bit_width() to satisfy clippy::manual_bit_width on newer nightly (#554)

## [0.6.26] - 2026-07-12

### Fixed

- daemon: verify PID identity so a recycled PID can't wedge startup (#551)

## [0.6.25] - 2026-07-08

### Changed

- uninstall: shared elevation gate + privilege-UX fixes (#546)
- build: Windows ship binaries strip the panic backtrace (build-std) (#547)

### Fixed

- non-elevated self-update + honest daemon stop/kill (privilege UX) (#544)

## [0.6.24] - 2026-07-07

### Changed — consistent `--version` across every UFFS binary

All 22 shipped and dev binaries now print the same `--version`: a short line
`<name>[.exe] <semver> (<sha>)` (with `-dirty` on an uncommitted tree), and a
multi-line build fingerprint with `--version --verbose` / `-v` (commit + date,
rustc, target triple, release profile) for bug reports. On Windows the name
carries its real `.exe` suffix (`uffsd.exe 0.6.23 (…)`). Previously only `uffs`
showed the build sha; the others were plainer and slightly inconsistent (e.g.
`uffsmcp` printed `uffs-mcp v…`). Backed by a new zero-dependency `uffs-version`
leaf crate (macros + a `build.rs` metadata stamp), so it can never drift again.

## [0.6.23] - 2026-07-04

### Fixed — calmer message when WinGet doesn't have the new version yet

Right after a release, the WinGet package manifest publishes a little later than
the GitHub release, so `uffs --update`'s winget step would print a scary
`winget upgrade did not complete (0x8a15002b)`. That exit code just means "no
newer version available yet." It is now recognised and narrated as a plain note
("WinGet doesn't have vX.Y.Z yet — the winget copy catches up on the next
`uffs --update`"), and any genuine winget failure still explains the likely
publish-lag cause rather than only showing a raw code.

### Fixed — self-update now sees a dormant WinGet install

`uffs --update` finds a WinGet-managed install even when nothing is running from
it. Root discovery used to rely only on live anchors (the invoking `uffs.exe`
plus any running daemon / broker / MCP), so with the daemon and broker stopped
and `--update` run from a hand-placed copy, the winget install had no anchor and
was invisible — never seen, never reconciled. Detection now also scans WinGet's
well-known package location, so the winget copy is found and updated regardless
of what is running.

### Fixed — the Access Broker note no longer fires when no broker is running

A non-elevated update skips (and warns about) the Access Broker only when it is
a *running* `LocalSystem` service — the case that genuinely needs elevation to
cycle. A plain `uffs-broker.exe` file with no running service is now updated in
place like any other binary, without the misleading "left running and NOT
updated" note.

## [0.6.22] - 2026-07-03

Consolidated notes for the 0.6.19–0.6.22 development cycle, which reworked the
full **install / update / uninstall** lifecycle on Windows and made the
macOS/Linux binaries portable.

### Added — zero-UAC Access Broker service

`uffs-broker --install` (one-time, from an elevated shell) registers a
`LocalSystem` Windows service that vends the daemon an elevated, duplicated
NTFS volume handle over a named pipe. After that, **every** later `uffs` search
runs non-elevated with **no UAC prompt**, surviving reboots — the daemon reads
the live MFT through the broker's handle. Install narrates each step with a
spinner; the one slow step (`sc create`, scanned by antivirus) is called out
honestly instead of looking hung.

### Added — self-update: `uffs --update` (and the `--upgrade` alias)

One command reconciles any starting state to a complete, current install —
behind, version-skewed, or even missing a binary — and does nothing when
already current. It downloads, SHA-256-verifies, and atomically swaps every
core binary (journaled, with auto-rollback). On a WinGet-managed install it
stops the daemon and package broker up front (one UAC), runs `winget upgrade`
against the now-unlocked images, and restarts them — so the bare `winget
upgrade` that fails on locked binaries is no longer the user's problem.

### Changed — `uffs --uninstall` reworked into a full, safe teardown

Uninstall discovers every UFFS copy across the system (PATH + standard bin
dirs on all platforms; on Windows also a deep, MFT-driven sweep for strays),
reloads a version-mismatched daemon before sweeping so it reports the truth,
and removes binaries, data, cache, config, and the broker service under clear
CORE / ALL confirmations. Elevation is requested once, only when actually
needed (the broker service), and the flow degrades cleanly if you decline. The
`uffs` binary removes itself last, on exit.

### Changed — Linux binary is now a static musl build

The Linux release binary links musl statically (built with cargo-zigbuild), so
it runs on old-glibc hosts and inside WSL without the previous
`GLIBC_2.xx not found` failure. macOS and Linux remain **offline MFT analysis
only** — they do not index the host filesystem (see the platform note in the
README).

### Changed — binaries renamed with a `uffs-` prefix

Every shipped binary now starts with `uffs` (e.g. `uffs-mcp-http`,
`uffs-bench`). The pre-rename names are recognized as retired stems, so
upgrades and uninstall still find and clean up older installs.

### Fixed — USN journal phantom root paths

Live index updates from the NTFS USN journal now apply parent-directory changes
before their children, so newly-created files resolve under their real parent
path instead of occasionally appearing at a bare drive root (`C:\name`).

### Fixed — honest winget-upgrade narration

Self-update no longer claims the Access Broker was "not updated" when the
winget package upgrade actually refreshes it; no longer fires a pointless
update + UAC when the install is already current (the broker is a
once-installed service, not required in every CLI folder); and no longer
reports a scary "failed to restart" for components the winget flow relaunches
itself.

### Fixed — one-line macOS/Linux installer hardening

`install.sh` resolves the latest version without the intermittent broken-pipe
failure, verifies every download against the release `SHA256SUMS` (exact match,
tolerating the `./` prefix), and installs transactionally — it downloads and
verifies the whole set before moving anything into place, so an interrupted run
never leaves a half-old / half-new install.

## [0.6.18] - 2026-06-29

### Changed: `uffs --uninstall` now finds every copy across the system

Uninstall discovers UFFS wherever it actually lives, not just the running copy.
It scans `PATH` and the standard binary directories (`~/bin`, `~/.local/bin`,
`~/.cargo/bin`, `/usr/local/bin`, `/opt/homebrew/bin`) with a stat-only,
which-style lookup (never a filesystem walk), reusing the real channel/scope
classifier so a WinGet copy is still delegated to `winget` and a root-owned
location is still flagged for elevation. Retired and optional binary names
(`uffs-daemon`, `uffs-mcp`, the legacy `uffs_*` names) are swept too. PATH
cleanup now only touches a directory that is exclusively UFFS, so a shared
`~/bin` full of your other tools is left untouched.

On Windows, where UFFS indexes the live drives, uninstall also offers to start
the daemon and index any missing NTFS drives, then asks UFFS itself for stray
copies anywhere on the system, versions them, and removes them under a separate
confirmation (a copy you placed yourself might be among them). The coverage
offer runs under `--dry-run` too, since starting and indexing are
non-destructive; only the deletions are withheld.

### Added — one-line macOS/Linux installer (`install.sh`)

`curl -fsSL https://raw.githubusercontent.com/skyllc-ai/UltraFastFileSearch/main/install.sh | bash`
installs UFFS on macOS/Linux with no sudo and no build toolchain: it detects the
platform, downloads the prebuilt binaries from the matching GitHub release,
**verifies each against the release `SHA256SUMS`**, and drops them in
`~/.local/bin`. Pin with `UFFS_VERSION`, retarget with `UFFS_INSTALL_DIR`. It is
the posix counterpart to `winget install SkyLLC.UFFS` on Windows, and the
symmetric companion to `uffs --uninstall`. PATH is never edited automatically
(the shell owns it); the script prints the one line to add if the dir is not on
PATH.

## [0.6.17] - 2026-06-29

### Added — `uffs --uninstall`: guided, complete removal of UFFS

A single command removes UFFS and all of its data from the machine, as carefully
as `uffs --update`. It analyzes the install (every binary shown in OS-resolution
order, with the `ACTIVE` copy flagged), inventories every artifact with sizes
(data, cache, legacy cache, config, the Windows broker service), runs a deep
sweep that uses UFFS's own search to find stray `uffs*` files elsewhere (listed
for review, never auto-removed), prints an itemized removal plan, and only
removes after explicit consent (or `--yes`).

- **Elevation-aware, and frugal about it.** It refuses up front (before any
  effect) when a removal needs privilege the run lacks. On macOS/Linux a normal
  user install needs **no `sudo`** (a real `access(W_OK)` check decides per
  root); only a root-owned location or the Windows broker service / machine
  install requires elevation.
- **Channel-aware.** WinGet roots are delegated to `winget uninstall`, never
  hand-deleted. Manual and dev-build installs are removed directly.
- **Safe + idempotent.** `--dry-run` reviews without changing anything;
  removal is best-effort (a locked/permission-denied item is reported, the rest
  proceed) and idempotent (re-run to finish an interrupted one). Flags:
  `--keep-config`, `--no-deep-sweep`, `--no-path`, `--scope`, `--json`. The
  running binary self-deletes on exit; a post-removal step verifies the result.
  See [docs/user-manual/uninstall.md](docs/user-manual/uninstall.md).

### Added — corrupt-name forensics: keep ill-formed names visible + `--normalize-malformed`

NTFS allows file and directory names that are ill-formed UTF-16 (unpaired
surrogates). UFFS now treats them as first-class:

- **Corrupt names keep their place.** A directory whose name is ill-formed no
  longer collapses out of the path: previously its children were re-parented to
  the drive root and the directory showed up as duplicate empty-named rows. Now
  the directory stays at its true position and its descendants resolve under it.
- **Display matches Everything / Explorer.** A corrupt run renders as one
  replacement char (`�`) per offending UTF-16 code unit, instead of the previous
  one-per-byte (`���`), so output lines up with the reference tools.
- **`--normalize-malformed`** (new flag) renders each corrupt code unit as a
  greppable, reversible `<BAD:HHHH>` marker instead (e.g. `<BAD:DCFF>` for an
  unpaired low surrogate). `<` / `>` cannot occur in a real NTFS name, so the
  marker never collides; the hex keeps two corrupt siblings distinct and lets
  downstream tooling spot, parse, or round-trip the true name. The default stays
  the lossy `�` rendering.

Filtering corrupt entries is unchanged: the `malformed` / `malformed_path`
columns and the malformed name filter still select them; `--normalize-malformed`
is display-only and never changes which rows match. Well-formed names pay nothing
(the lossy/normalized render only runs for the rare corrupt name).

### Fixed — ship: roll `## [Unreleased]` into a dated section on every release

`just ship` now rolls the changelog's `## [Unreleased]` section into a dated
`## [vX.Y.Z]` section as part of the release commit (right after the version
bump), and repoints the Keep-a-Changelog footer compare-links. Previously the
bump never touched the changelog, so `## [Unreleased]` silently accumulated
already-shipped work and every release in between went unrecorded (the drift
repaired in #490). The roll is a pure, unit-tested transform in
`scripts/ci-pipeline/src/changelog.rs`; an empty `## [Unreleased]` is a clean
no-op, so a release with no notes never creates an empty section.

## [0.6.15] - 2026-06-28

### Added

- **idxdelta** Incremental index maintenance — O(changed) USN apply (base + delta overlay) (#483)

### Fixed

- **usn** Coalesce a backlog of apply ticks into one body rebuild (#481)
- **release** Pre-create the tag via refs API so GITHUB_TOKEN can publish (#478)

### Performance — incremental index maintenance: live updates apply in O(changed)

The resident daemon's per-poll USN-journal apply now scales with the number of
**changed** records instead of the **total** drive size. On a live 4.3M-record
volume the per-apply cost drops from ~1367 ms to ~200 ms (−85%): path lengths,
the trigram / extension / children indexes, and the directory subtree all update
incrementally, with the occasional full refold confined to a rare compaction.

For everyday use this means **live search stays fresh under heavy churn**: new,
renamed, and deleted files become searchable in well under a second (1k/10k
files in ~0.1 s, 100k in ~0.5 s on a 4.3M-record drive, validated while the
recycle bin was being emptied). The apply cadence is a debounce + max-wait
(settle quickly when a burst goes quiet, but never let a busy drive starve the
CPU), so it feels snappy without thrashing.

## [0.6.14] - 2026-06-26

### Added

- **search** Path_excludes filter + teach the MCP keyword-OR / noise-dir exclusion (#471)

### Fixed

- **usn** Live journal correctness + near-live search freshness + ext-index/metadata backfill (#475)

## [0.6.13] - 2026-06-20

### Fixed

- **search** Survive the transient I/O abort (os 995) during a parked-drive re-warm (#469)
- **cli** Uffs --status broker section is "not applicable" off Windows (#468)

## [0.6.12] - 2026-06-20

### Added

- **self-update** Non-elevated update skips the broker (+ zero-UAC docs) (#466)

### Fixed

- **cli** Non-elevated daemon management when the broker is serving (Windows) (#464)

## [0.6.11] - 2026-06-19

### Added

- **mcp** Read-only freshness signal + honest stale-session messaging (#460)

### Fixed

- **client** Don't refuse the daemon when its launch path != resolved image path (#462)
- **self-update** Stop a non-elevated repair from fabricating a broker failure (#461)

## [0.6.10] - 2026-06-18

### Added

- **broker** Add --version/-V so the self-update probe shows its version (#458)

### Fixed

- **self-update** Remove '--' from manifest XML comment — the real SxS 14001 cause (#457)
- **self-update** Minimal asInvoker manifest — stop uffs-update.exe SxS crash (os error 14001) (#456)

## [0.6.9] - 2026-06-18

### Fixed

- **update** Embed asInvoker manifest in uffs-update.exe (Windows elevation) (#454)

## [0.6.8] - 2026-06-17

### Fixed

- **update** Make downloaded binaries executable (self-update apply on Unix) (#452)

## [0.6.7] - 2026-06-17

### Added

- **update** Doctor redirects to the update flow to fix update-class issues (#449)
- **update** Reconcile a missing core binary back into a complete install (#448)
- **update** Single source of truth for the core binary set + `uffs --update bins` (#447)
- **brand** Add 400x136 receipt wordmark variant (#445)

### Fixed

- **update** Stamp journal to_version, stop relaunching stdio mcp, add repair verb (#446)
- Correct Ko-fi sponsor handle to uffssearch (#444)

## [0.6.6] - 2026-06-17

### Added

- **update** Bare `uffs --update` updates end-to-end (ordinary-user default) (#441)

### Fixed

- **update** Apply quiesce stops services with the CURRENT uffs, not a stale sibling (#440)

## [0.6.5] - 2026-06-16

### Fixed

- **update** Self-update flow polish — Unix apply, clean errors, complete install gate, quiet output (#438)

## [0.6.4] - 2026-06-16

### Added

- **cli** Search-first grammar (`uffs --<command>`) + full doc/code alignment (#436)
- **winget** Auto-release the seeded PR — fully hands-off winget flow (#431)

### Fixed

- **update** Treat uffs-broker as Windows-only in self-update detection (#434)
- **build** Include uffs-update in local build/install/deploy flows (#433)

## [0.6.3] - 2026-06-16

### Added

- **winget** Auto-seed nested aliases into the komac PR (+ draft gate) (#429)
- Native Windows service control (uffs-winsvc) — broker --status/--start/--stop, uffs status broker, sc-free updater (#428)
- **update** Crash-proof self-update — apply resilience, doctor, per-binary release assets (#427)
- Self-update Phases B & C — snapshot + acquire (+ DRY Authenticode) (#426)
- **cli** Self-update Phase A — multi-anchor install detection (`uffs update`) (#425)

## [0.6.2] - 2026-06-15

### Fixed

- **search** Scope bare & path-anchored drive patterns (C:, C:\, C:\path) (#420)

## [0.6.1] - 2026-06-15

### Added

- **broker** Windows Service control dispatcher (FU-1) (#414)
- **broker** Multi-instance threaded serve loop + OwnedHandle (FU-5, SBB-2) (#413)

### Fixed

- **broker** FILE_FLAG_FIRST_PIPE_INSTANCE only on the first instance (FU-5 fix) (#415)
- **client** Probe the broker pipe without connecting (FU-6) (#411) (#412)
- **client** Probe the broker pipe without connecting (FU-6) (#411)
- **mft** Read $UpCase through the overlapped handle (FU-8) (#410)
- **mft** Bootstrap MFT extents from FRS 0 on the non-elevated path (FU-3) (#409)
- **mft** Open the USN journal through the broker handle (FU-2b) (#408)
- **daemon** Back off the journal poll instead of storming on failure (FU-2a) (#407)
- **daemon** Skip broker warm-up when the daemon is already elevated (#404)
- **broker** Make the Access Broker work end-to-end + production follow-up playbook (#403)
- **winget** Seeder also strips the unsupported portable Scope field (#402)

### Performance

- **daemon** Bound the live-drive load fan-out (large-estate safety) (#417)

### Added — Windows Access Broker: non-elevated search with no UAC prompts

`uffs-broker --install` (one-time, from an elevated shell) registers a small
`LocalSystem` Windows service that vends elevated NTFS volume handles to the
non-elevated daemon over a named pipe. After that, **every `uffs` search reads
the live MFT with no UAC prompt**, surviving reboots — removing the
"elevate every session" friction that Windows file-search tools normally carry.
The service verifies the client is the UFFS daemon (name allow-list +
Authenticode) before granting any handle, and is removed cleanly with
`uffs-broker --uninstall`.

On the non-elevated path this reaches full parity with an elevated read:

- **Complete MFT index, including fragmented MFTs** — the extent map is
  bootstrapped from the `$MFT` record's own run list when the file can't be
  opened directly.
- **Incremental USN-journal refresh** through the brokered handle (no more
  full rebuild on every refresh), with a backoff so an unavailable journal
  never floods the log.
- **In-process Authenticode verification** (`WinVerifyTrust`) replacing a
  per-request PowerShell spawn — faster, and no dependency on PowerShell.
- **Concurrent multi-instance pipe** and a real Windows service lifecycle
  (install / auto-start on boot / graceful `sc stop` / uninstall).


### Performance — bounded live-drive load fan-out

Loading many drives at once is now capped at roughly the CPU-core count, so a
large estate (≈25+ drives) no longer fans out into a memory / I-O / thread
spike during startup. Small estates (drives ≤ cores) load fully in parallel
exactly as before — no change for typical setups.

## [0.5.123] - 2026-06-13

### Fixed

- **winget** Generalize alias seeding so uffs-broker reaches the manifest (#399)

## [0.5.122] - 2026-06-13

### Added

- **demo** Wave-0 launch kit - video tapes, MCP query timing, fresh-VM fixes (#397)
- **release** Auto-dispatch binary build when a release PR merges (#396)

## [0.5.121] - 2026-06-12

### Added

- **bench** Full benchmark suite + v0.5.120 canonical promotion (#393)
- **release** Manual on-demand binary release trigger (Path B) (#390)

### Fixed

- **release** Restore the version bump inside just ship Phase 2 (#391)
- **release** Flip release-plz to registry baseline; re-enable push trigger (#389)
- **ci** Gate crates-io-publish on releases_created (#388)
- **toolchain** Invalidate target/ + sccache after a nightly bump (#384)
- **ci** Re-defer release-plz auto-trigger — cargo package bootstrap (#383)
- **ci** Release-plz packages never-publish crates — add release=false (#382)
- **ci** Release-plz cargo package fails — disable sccache wrapper (#380)

## [0.5.120] - 2026-05-08

> **Note on the v0.5.91 gap.**  v0.5.91 was prepared and tagged but never
> reached a published GitHub Release: the `release.yml` finalize step hit
> a server-side `pre_receive Repository rule violations found ... Cannot
> create ref due to creations being restricted` rejection, and after the
> partial release was deleted, the tag name became permanently locked by
> GitHub's *immutable releases* feature (the pre-receive hook refuses any
> future ref creation under that name even after a clean delete).  The
> public release sequence therefore jumps `v0.5.90 → v0.5.120`; all
> intended v0.5.91 changes are rolled forward into this release.

### Fixed
- **release-plz active-mode race with the bespoke `auto-tag-release.yml`
  flow.**  Without `release_always = false`, the `release-plz release`
  job ran on every push to `main` and competed with `auto-tag-release.yml`
  to create the same `v*` tag, producing duplicate workflow runs and
  occasional failed tag pushes during the R4 transition.  Setting
  `release_always = false` in `release-plz.toml` gates tag creation
  through `release-plz-*` PR merges only, so the bespoke flow remains
  the sole tag source until R5 retires it.  See
  `docs/architecture/release-automation-plan.md §R4` for the full
  rationale and the deviations log entry "R4 release-job race".
  (R4 active mode, PR #151)

### Carried over from the unreleased v0.5.91
- **macOS arm64 release binaries SIGKILLed at launch** under macOS 26+
  (`SIGKILL (Code Signature Invalid)` / `namespace=CODESIGNING` /
  `"Taskgated Invalid Signature"`).  `[profile.release].strip = "symbols"`
  in `Cargo.toml` strips the Mach-O symbol table **after** the linker has
  emitted an ad-hoc (linker-signed) `CodeDirectory`, leaving the embedded
  hash inconsistent with the on-disk file.  macOS 26+'s hardened
  taskgated then refuses to launch the binary.  In v0.5.72 this hit
  `uffsmcp` and `uffsd` deterministically; `uffs` and `uffs-mft` survived
  by binary-layout chance — a fragile guarantee that wouldn't hold on
  the next rebuild.

  Fix: add a `Re-codesign macOS binaries (post-strip)` step to
  `release.yml` that re-stamps the ad-hoc signature with `codesign
  --force --sign -` on every shipping `apple-darwin` binary after
  `cargo build --release` finishes.  The step is gated on
  `contains(matrix.target, 'apple-darwin')` so Windows / Linux artifact
  paths are untouched.  Each re-signed binary is then verified with
  `codesign --verify --verbose=2` so a regression here fails the
  workflow loudly instead of shipping broken artifacts.

  Workaround for users still on a v0.5.72 download:

  ```bash
  codesign --force --sign - ~/bin/uffsmcp ~/bin/uffsd
  ```

  Re-signs in place; macOS picks up the refreshed `CodeDirectory` on the
  next exec and the binaries launch normally.

  No code changes; release-only fix.  Recommended upgrade for every Mac
  user on macOS 26+.

## [0.5.119] - 2026-06-09

### Fixed

- **daemon** WARM guarded fast-load + HOT per-write icacls spawn removal (#373)

## [0.5.118] - 2026-06-08

### Added

- **bench** Bench robustness — TTL fixes, interleaved HOT rounds, path-set parity checks (#369)

## [0.5.117] - 2026-06-07

### Added

- **uffs-bench** Robust benchmark-suite orchestrator P1–P9 complete (#367)

## [0.5.116] - 2026-06-05

### Fixed

- **mft** Rebuild no-journal cache past age ceiling (resilience) (#364)

## [0.5.115] - 2026-06-05

### Fixed

- --malformed filter bypass (is_empty gate, DisplayRow predicate, limit leak) (#361)

## [0.5.114] - 2026-06-05

### Added

- Malformed-name forensics — surface & prove ill-formed names (WI-4.4 follow-up) (#359)
- **mft** Lossless filename storage — surrogate-named files can no longer hide (WI-4.4) (#358)

## [0.5.111] - 2026-06-05

### Fixed

- **broker** Open client process once, thread handle verify→duplicate (WI-8.1) (#352)
- **spawn** Pass daemon argv as OsString, decode Windows paths losslessly (WI-4.2) (#351)
- **mft** Harden all 5 NTFS parsers against panic-on-malformed-bytes (WI-5.2) (#349)
- **mft** Instrumented UTF-16 decoder (WI-4.1) + discard audit (WI-6.3) (#347)
- **daemon** Cross-platform elevation gate + demo tape line-wrap fix (#345)

### Security

- **security** Handle remaining from_utf8_lossy decision/scan sites (WI-4.3 follow-up) (#348)
- **security** Close bugs-rust-wont-catch Categories 1/2/3 + partial 4/5/6/8 (13 WIs) (#346)

## [0.5.110] - 2026-06-04

### Fixed

- **demo** Relatable + aggregation examples in cli-demo.tape (#343)
- **cli** Human-readable timestamps + suppress empty 'Already Hot' in daemon output (#342)
- **winget** Preserve CRLF line endings in uffs-tui alias patcher (#334)

## [0.5.109] - 2026-06-02

### Added

- **release** Min/normal/full bundle tiers + bundled uffs-tui demo (+ winget alias seed) (#332)

## [0.5.108] - 2026-06-02

### Fixed

- **ci** Auto-trigger winget submission from the release pipeline (#330)

## [0.5.107] - 2026-06-01

### Fixed

- **ci-tooling** Force cargo clean when rustc toolchain changed (#325)

### Fixed — ship pipeline: force `cargo clean` after a `rustc` toolchain bump

`just ship` step `00-toolchain-ensure` can bump the pinned nightly, but
step `02-clean-artifacts` only cleaned on disk-pressure heuristics — so a
fresh nightly reused a `target` dir built by the previous `rustc`, whose
version-specific cross-crate metadata then made every build explode with
`E0514` ("found crate compiled by a different version of rustc").

- **The clean step now fingerprints the toolchain.** `context::active_rustc_id`
  captures `rustc -vV`; the clean step stores it under
  `CARGO_TARGET_DIR/.uffs-ci-rustc-fingerprint` and, on every run, forces a
  `cargo clean` when the active toolchain differs from the one that built
  the cache (before any build step runs). The fingerprint is re-recorded
  after the decision so the next bump is detected.
- **Disk-pressure auto-clean and the `--clean` / `--no-clean` flags are
  preserved.** `--no-clean` downgrades a needed toolchain clean to a warning
  and leaves the stale fingerprint so the next run re-detects it. The clean
  policy is extracted into a pure, unit-tested `decide_clean` function.

## [0.5.106] - 2026-05-31

### Added

- **ci** Auto-submit winget manifest on release + promote winget install (#321)

### Fixed

- **ci-tooling** Keep release auto-commit on release branch, not main (#322)

### Fixed — ship pipeline: release auto-commit no longer drifts local `main`

The `just ship` flow (`scripts/ci-pipeline` → `git_ops::git_push`) used to
create the `chore: development vX.Y.Z` auto-commit on the **current
branch (`main`)** and then instruct the operator to run `git reset --hard
origin/main` after the release PR squash-merged (because the squash
rewrites the commit SHA).  That left local `main` ahead of / diverged
from `origin/main` after every ship and required a destructive reset to
recover.

- **Auto-commit now lives on the `release/vX.Y.Z` branch, never on
  `main`.** `git_push` switches onto the release branch
  (`ensure_on_release_branch`, replacing `detect_current_branch`),
  rebases it onto `origin/main`, opens the PR against `BASE_BRANCH`
  (`main`), and then returns the working tree to `main`
  (new `return_to_base_branch` helper).
- **Local `main` no longer drifts**, so the post-merge step is a plain
  `git pull --ff-only origin main` instead of `git reset --hard` — the
  operator hint printed at the end of `git push` is updated to match.
- `ensure_on_release_branch` uses a plain `git switch` (not `-C`) so a
  resumed ship lands on the existing release branch that already holds
  the auto-commit rather than resetting it to a commit-less `HEAD`.

## [0.5.103] - 2026-05-29

### Added

- **logging** Route all core binaries to shared native log dir (#315)
- **sponsorship** Launch Open Collective + Ko-fi donation channels (#313)

### Fixed

- **toolchain** Correct stale rust-toolchain.toml comment drift (#316)

## [0.5.102] - 2026-05-20

### Fixed

- **daemon** Wrap sync search file-output in block_in_place (#307)
- **concurrency** Snapshot lock state before awaits in status RPC + MCP dispatch (#304)
- **features** Gate 6 items on streamable-http/async + add lint-ci-no-default regression guard (Phase 8e, refs #295) (#296)
- **uffs-core** Drop per-row format!() allocations in two search hot paths (Phase 6d) (#283)
- **uffs-core** Drop defensive Vec<u16> clone in path-only / path-sorted top-N hot path (Phase 6c) (#282)
- **ci** Sign cargo-vet refresh PR commits via GITHUB_TOKEN REST API (refs #272) (#275)
- **mft** Unwrap FACILITY_WIN32 HRESULT at all four VolumeHandle Open sites (#274)
- **ci** Move cargo-mutants to windows-latest so cfg(windows) code is reachable (refs #267) (#271)
- **ci** Pin cargo-fuzz to glibc target so sanitizer can link (refs #267) (#270)
- **ci** Drop MSRV verification + workspace rust-version claim (closes #267) (#269)

### Security

- **security** Weekly cargo-vet import refresh (#272)
- **security** Unwrap FACILITY_WIN32 HRESULT so create_owner_only honors AlreadyExists contract (#273)

### Added — WinGet distribution: live install docs + auto-submission pipeline

`SkyLLC.UFFS` is published on the Windows Package Manager community
repository (microsoft/winget-pkgs#378294, v0.5.102), so `winget install
SkyLLC.UFFS` now works.

- **Docs promote WinGet as the recommended Windows install.** `README.md`
  Download section gains the `winget install SkyLLC.UFFS` one-liner and
  the stale "WinGet (coming)" note is removed;
  `docs/user-manual/installation.md` gains a new §1 *WinGet (Windows —
  Recommended)* and renumbers the downstream sections (Pre-Built Binaries
  → §2 … Verify Installation → §6).
- **`.github/workflows/winget-publish.yml`** — on every published
  release (`release: released`), `winget-releaser` (komac under the hood)
  opens a winget-pkgs PR bumping the manifest. The prior version's
  nested-installer structure (`InstallerType: zip` /
  `NestedInstallerType: portable`, the four `uffs`/`uffsd`/`uffsmcp`/
  `uffs-mft` command aliases) carries over automatically. A
  `workflow_dispatch` fallback re-submits a specific tag by hand.
  - **One-time setup required:** add a `WINGET_TOKEN` repository secret —
    a classic PAT with `public_repo` scope owned by the maintainer
    account that holds the winget-pkgs fork (the action's `fork-user` is
    pinned to `githubrobbi`, not the `skyllc-ai` org). The default
    `GITHUB_TOKEN` cannot fork an external repo, so the workflow no-ops
    until this secret exists.

## [0.5.100] - 2026-05-17

### Fixed

- **just** Replace Unicode in Windows PowerShell recipe strings (#260)

## [0.5.99] - 2026-05-15

### Added

- **bake** Add cross-platform just bake-day recipe for daily v0.6.0 bake-period checks (#248)
- **bench** Add --out summary CSV flag + verify v0.5.35 CLI hot-path baseline (#247)
- **manifest-audit** Permit per-crate keywords/categories override via allow-list (invariant 3.5) + apply on 4 library crates (#243)
- **manifest-audit** Permit per-crate readme override via allow-list (invariant 3.5) (#238)

### Fixed

- **ci** Fail-fast pre-flight + per-job permissions on release publish (#255)
- **ci** Repair rustup proxy AFTER cache restore on macOS release builds (#254)
- **ci** Keep release 'Show binary sizes' green on non-Windows targets (#251)
- **ci** Repair macOS rustup proxy in release workflows + workspace-wide actionlint cleanup (#245)

### Fixed — `release-cache-warm.yml` macOS runner-image flake + workspace-wide actionlint cleanup

The `release-cache-warm.yml` workflow had a ~50 % failure rate
on its `aarch64-apple-darwin` matrix leg over the past week.
Every failure shared the same fingerprint: `cargo build` exited
with `error: unexpected argument 'build' found` and a
`rustup_init::run_rustup_inner` stack backtrace.

**Root cause.**  On `macos-latest` runner images the
`~/.cargo/bin/cargo` proxy is occasionally a stale symlink
pointing at `rustup-init` (the installer binary) instead of the
proxy that forwards to the active toolchain's real cargo.
`rustup show` succeeded because it invoked rustup directly; the
very next step's `cargo build` hit the broken proxy and fell
through to the installer's argument parser, which has never
heard of `build`.  Linux + Windows runners were unaffected.

**Three changes to `release-cache-warm.yml::warm`, sized to the problem:**

- **Toolchain-install repair (also mirrored into `release.yml::build-release-binaries`).**
  The Install-step now appends a forced
  `rustup default "$(rustup show active-toolchain | awk '{print $1}')"`
  after `rustup show`, which rewrites the proxy binaries in
  `~/.cargo/bin` so the symlinks resolve to the active
  toolchain's real cargo.  A `cargo --version` /
  `rustc --version` smoke check at the end of the step
  surfaces a still-broken proxy in ~1 s instead of letting
  Swatinem/rust-cache's `cargo metadata` and a 30+ min
  `cargo build` silently waste runner minutes before failing
  on the same root cause.  The same repair is mirrored into
  `release.yml` because that workflow's tag-dispatched runs
  would hit the same broken proxy on a freshly-flaky runner
  image — and on the production release pipeline a 30+ min
  late failure delays the release by a full re-run cycle.

- **Single auto-retry on the cargo-build step.**  Even with the
  proxy repair, GitHub-hosted runners have transient
  network blips during dep download and occasional sccache
  flakes.  A 2-attempt bash loop with a 10-s sleep between
  attempts absorbs those without requiring maintainers to
  manually re-run the workflow.  Release.yml is NOT given a
  retry — it's the production critical path and we want it
  to fail loudly so the release pipeline's
  `notify-failure` issue-opener fires.

- **`continue-on-error: true` at the warm-job level.**  The
  workflow's own header comment already documented that
  cache-warming is best-effort by design.  `continue-on-error`
  makes that intent explicit to GitHub: a transient single-
  platform runner-image flake no longer paints the workflow
  ❌ in PR checks / branch protection.  Real regressions
  still surface via the workflow's run history + the per-job
  summary table.

**Adjacent actionlint cleanup, same PR.**  While auditing the
two release workflows, every other workflow in
`.github/workflows/` was actionlint-scanned and the
pre-existing shellcheck warnings (all info / style level —
none of them latent bugs) were resolved:

- **`release.yml`**: 5 clusters — SC2086 (unquoted
  `$GITHUB_OUTPUT` / `$GITHUB_STEP_SUMMARY` writes), SC2129
  (individual redirects grouped with `{ ... } >> "$out"`),
  SC2010 (`ls | grep` replaced with an explicit glob loop
  over the shipping-set binary names), SC2035 (`sha256sum *`
  / `shasum -a 256 *` switched to `./*` form to guard
  against future filenames starting with `-`).

- **`auto-rerun-transient.yml`**: 3 × SC2016 — false
  positives where literal backticks inside a single-quoted
  `printf` format string looked like command-substitution
  markers to shellcheck.  Rewritten as `echo` with
  backslash-escaped backticks, which is more idiomatic for
  "print this template string" and silences the warning
  without a per-line suppression directive.

- **`cargo-vet-refresh.yml`**: 1 × SC2129 — individual
  `echo … >> "$GITHUB_STEP_SUMMARY"` writes grouped into a
  single `{ … } >> "$GITHUB_STEP_SUMMARY"` block.

- **`dependabot-review.yml`**: 3 × SC2129 — same
  group-redirect fix in three locations (the delta-summary
  table, the newly-resolved-crates block, and the
  dropped-crates block).

Verification: `actionlint .github/workflows/*.yml` is now
clean across every workflow (zero warnings, zero errors).
All script behavior is byte-identical to before; bash test
runs of the rewritten echo / grouped-redirect blocks produce
the same output as the originals.


### Verified — v0.5.35 baseline CLI hot-path bench (2026-05-15)

Plan §1 goal-4 ("no regression on CLI hot path vs the v0.5.35
baseline") verified end-to-end on the Windows 7-drive reference
box.  Current v0.5.120 (post-Phase-8 tiered architecture) is
**universally faster** than v0.5.35 across every benchmarked
pattern, with the largest result set (`*.dll`, 44 529 rows)
showing a **2.7× speedup**:

```
Drive D, 7.07 M records, 30 rounds, HOT phase, p50 / p95 wall_ms:

                              v0.5.35      v0.5.120       Δ p50
    exact      (3 rows)       20 / 23   →  18 / 19      −10 %
    prefix     (8 732)        46 / 50   →  40 / 46      −13 %
    ext_rare   (11)           18 / 20   →  17 / 18       −6 %
    ext_dll    (44 529)       94 / 100  →  35 / 40      −63 %  (2.7×)
    substring  (12 458)       50 / 54   →  43 / 47      −14 %
```

Row-count parity preserved exactly across both versions on every
pattern (no result-set drift — the post-v0.5.35 path-resolver
fast path + Phase 3.2 single-buffer multi-column render path
both preserve the same filter semantics and column ordering).
The dramatic `ext_dll` win is dominated by those two surfaces:
both are big-N row-materialisation gains, exactly where 44 K
paths-into-CSV land in the wall-clock budget.

Capture command (run twice with `~/bin/uffs.exe` swapped between
the two binaries via `~/bin/uffs.exe daemon kill` +
`Remove-Item $env:LOCALAPPDATA\uffs\cache` between runs):

```powershell
rust-script scripts\windows\cross-tool-benchmark.rs `
    --skip-cold --drives D --rounds 30 `
    --patterns exact,prefix,ext_rare,ext_dll,substring `
    --tools uffs `
    --uffs-bin "$HOME\bin\uffs.exe"
```

Raw artefact: `LOG/memory output` (manual stdout paste).  The
**`--out <path>` flag was added to the bench script in this
release** so future captures can persist the summary table as a
structured CSV (columns: `tool,phase,sink,drive,pattern,p50_ms,
p95_ms,rows,bad,verdict,rounds_ok,rounds_total`) — see the
`Added — cross-tool benchmark script: --out summary CSV flag`
entry below.


### Added — cross-tool benchmark script: `--out` summary CSV flag

`scripts/windows/cross-tool-benchmark.rs` learned a new
`--out <path>` flag that writes the post-run summary as a
plain-CSV at the supplied path (creating intermediate
directories as needed).  Columns mirror the stdout summary
table but emit integer milliseconds and integer row counts so
the file is trivially regress-able by `pandas` / `polars` /
`awk`:

```
tool,phase,sink,drive,pattern,p50_ms,p95_ms,rows,bad,verdict,rounds_ok,rounds_total
UFFS,HOT,file,D,exact,18,19,3,0,PASS,30,30
UFFS,HOT,file,D,prefix,40,46,8732,0,PASS,30,30
…
```

The new flag is distinct from the existing
`<cwd>/uffs_bench_out.csv` daemon-side file (raw per-query row
dumps, overwritten every round) and never collides with it.
The banner block prints both paths up front so operators see
exactly where each artefact lands.  Unknown CLI flags now warn
on stderr (`warning: unknown flag "--foo" ignored`) instead of
silently swallowing — caught during the 2026-05-15 v0.5.35
baseline bench where a typo-`--out` in pre-flag-support runs
left the operator hunting a non-existent CSV file.

## [0.5.96] - 2026-05-15

### Fixed

- **ci-tooling** Bring scripts/ crates under workspace lints (closes #212) (#228)
- **ci** Upgrade #209 fix to Design C — relocate notify-failure to ci-failure-notify.yml (#226)
- **ci** Auto-close stale ci-failure issues after successful auto-rerun (#225)
- **soak** Close Phase 6 / Phase 7 24-h gates + pin daemon log strings (#218)

### Fixed — Phase 6 + Phase 7 24-h soak harness (PR #218)

Two harness-side bugs surfaced by the 2026-05-09 / 2026-05-10
24-h Windows-host soak runs.  Both are validator scrape-pattern
bugs — the daemon's actual contracts (`min_tier` floor, peer
demotes, USN-journal save pipeline, working-set bound, fatal-class
log lines) were satisfied in every run; the validator was just
hunting for the wrong things.

- **Phase 6 — `scripts/dev/long-soak.rs:746` `RUST_LOG` raised to
  `shard.ttl=trace`.**  The catch-all `below-ttl` event in
  `crate::index::transitions::evaluate_idle_demote` is emitted at
  TRACE — the level needed by the soak harness's
  `parse_max_ttl_field("warm_ttl_sec")` scrape during the
  synthetic-load window, when drive `C` is in Warm/Hot with
  `idle_secs ≈ 0` and the DEBUG-level `idle-demote` /
  `min-tier-clamp` arms never fire.  Cost: ~23 k extra trace
  events over 24 h (~3.5 MB) — marginal against the existing
  ~75 MB log volume.

- **Phase 7 — `scripts/dev/long-soak.rs:1244` regex re-anchored on
  `compact-cache save`.**  The pre-fix regex hunted for
  `USN refresh tick|trigger_save|threshold.*save|encrypted cache refresh`
  — **none** of those alternatives match the daemon's actual INFO
  message `Journal poll: triggered background compact-cache save`.
  Retroactively closes Phase 7 for the existing 24-h log
  (`grep -c 'compact-cache save'
  LOG/uffs_soak/phase7-20260510-214412/daemon.log` = 11; the save
  pipeline was healthy all along).

- **Two new daemon-side regression tests** pin the literal log-
  message strings + tracing target + level + structured fields so
  any future rename fails CI before reaching another 24-h soak
  gate:
  * `crate::cache::journal_loop::tests::save_log_message::
    compact_cache_save_log_message_pins_string_target_and_level`
    — pins target = `uffs_daemon::cache::journal_loop`, level =
    INFO, and the literal `compact-cache save` substring.
  * `crate::index::tests::shard_ttl_events::
    below_ttl_event_pins_target_level_message_and_reason` —
    pins target = `shard.ttl`, level = TRACE, message =
    `"Adaptive idle-demote evaluation: not yet idle past TTL"`,
    `reason="below-ttl"`, and the four TTL fields.

- **Visibility hoist** — the existing
  `crate::index::tests::tracing_capture::{EventLog, CapturedEvent}`
  scaffold flipped from `pub(super)` → `pub(crate)`, plus
  `pub(crate)` on `mod tests;` in `crate::index` and
  `mod tracing_capture;` in `crate::index::tests`, so the same
  scaffold serves both modules' contract pins.  No production-code
  visibility change.

- **Runbook + bake-criteria updates** —
  [`docs/architecture/memory-tiering-windows-host-validation.md`](docs/architecture/memory-tiering-windows-host-validation.md)
  §2, §3, and §6 (new §4.5b + §4.5c capture sub-sections under
  the §6 "Reference captures" parent) now reflect the
  post-2026-05-13 grep patterns + the 2026-05-11 capture
  findings.
  [`docs/architecture/memory-tiering-bake-criteria.md`](docs/architecture/memory-tiering-bake-criteria.md)
  ticks Phase 7 retroactively; Phase 6 stays open pending one
  more 24-h run with the trace-level harness fix.


### Verified — Phase 6 24-h Windows-host soak closes end-to-end (2026-05-14/15)

The last remaining v0.6.0 24-h-soak gate now closes against a
live Windows-host capture.  `LOG/uffs_soak/phase6-20260514-122946/`
ran for 24 h on the 7-drive reference box (2026-05-14 12:29:46Z
→ 2026-05-15 12:29:46Z) against the post-PR-218 harness fix and
the validator reported **9 of 9 assertions PASS** end-to-end.

The §4.5b adaptive-bonus deferral (recorded in the 2026-05-11
docs against the May 9-10 reference run, where the
`RUST_LOG=shard.ttl=debug` filter dropped every `below-ttl`
TRACE event carrying the bonused `warm_ttl_sec` field) now has
direct end-to-end evidence:

- **Drive C `min_tier=Warm` floor.**  0 `to=Parked` events on
  letter=C across 24 h; **2 870** `Demote target clamped by
  per-drive min_tier` debug events — proving the floor was
  actively applied thousands of times, not merely coincidentally
  not-tripped.
- **Peer-drive demotion.**  D / E / F / G / M / S each fired
  exactly 2 `Warm → Parked` transitions, confirming the
  controller drives non-floor drives through the demote ladder
  on the configured `warm_ttl_base_secs`.
- **Adaptive TTL bonus.**  `C.max_warm_ttl = 3 786 s` vs
  peer `max(warm_ttl_sec) = 300 s` — a **12.6× bonus** on the
  high-rate drive, matching the `+600·log2(rate)` formula in
  `crate::cache::policy::warm_ttl`.

Memory trajectory across the 24-h window also validates the
tiering machinery doing real work:

```
                          00h           23h         post-load
Working Set (WS) :   6 746 800 128 →    22 888 448 →    69 238 784  (308× WS trim, 3× post-load re-page)
Private Memory   :   8 293 457 920 → 1 791 188 992 → 1 669 558 272  (78 % real release as drives demoted)
Virtual Memory   :  28 172 120 064 → 28 168 974 336 → 28 168 974 336 (flat — no address-space leak)
NPM (non-paged)  :          26 736 →        26 328 →        26 328  (flat)
```

The 78 % private-memory release is materially different from
the §4.5d ws-trace soak (where `pm_bytes` stayed within 3 % for
24 h because the keep-warm worker held all 7 drives Warm).
Here the controller actively demoted the 6 peer drives, which
unloaded cold shards from memory and produced the real
private-bytes release — exactly the intended tiering behavior
under sustained idle.  The end-of-soak synthetic-load window
on drive C re-paged recently-needed shards back into WS without
re-allocating private memory (PM actually continued to fall
slightly), confirming the page-cache vs. private-bytes split is
healthy.

No `panic` / `OutOfMemoryError` / `FATAL` log lines across the
24-h window.  Full breakdown in
[`docs/architecture/memory-tiering-windows-host-validation.md`](docs/architecture/memory-tiering-windows-host-validation.md)
§6 sub-section §4.5e.

With this closure, **all three v0.6.0 24-h-soak gates are
green**:

| Gate | Source | Result | Closed |
|---|---|---|---|
| Phase 6 (`min_tier=Warm` floor + adaptive bonus) | `phase6-20260514-122946/` | 9 / 9 PASS | 2026-05-15 |
| Phase 7 (USN-journal churn) | `phase7-20260510-214412/` | 7 / 7 PASS (regex fix) | 2026-05-13 |
| ws-trace (Working-Set trajectory) | `wstrace-20260513-113344/` | 4 / 4 PASS | 2026-05-13 |

Only the one-week `main` bake remains per
[`docs/architecture/memory-tiering-bake-criteria.md`](docs/architecture/memory-tiering-bake-criteria.md).


### Verified — Phase 7 + ws-trace 24-h Windows-host soaks (2026-05-13/14)

Two of the three v0.6.0 24-h Windows-host soak gates close
retroactively from existing capture data, with daemon-side
regression tests pinning the wire-format contracts so future
log-message renames fail CI before reaching another 24-h soak.

- **Phase 7 USN-journal churn soak** —
  `LOG/uffs_soak/phase7-20260510-214412/` retroactively closes
  7 of 7 assertions with the PR #218 regex fix: the save
  pipeline emitted 11 `compact-cache save` events during the
  24-h soak; the pre-fix validator regex did not match the
  daemon's actual INFO line.  No new soak required.

- **ws-trace 24-h Working-Set trajectory** —
  `LOG/uffs_soak/wstrace-20260513-113344/` passes 4 of 4
  assertions: PID 50492 stable across 24 hourly samples,
  289 / 289 keep-warm probes fired, WS ratio 0.03× (first
  =5.37 GB, last=184 MB).  The 30× WS drop is the
  `EmptyWorkingSet` page-trim (Phase 5 G2 wiring), **not a
  leak**: `pm_bytes` decreased only 3 % (6.53 GB → 6.36 GB)
  while all 7 drives held Warm and the daemon's own RESIDENT
  accounting stayed at ~5.0 GiB across all 24 snapshots.  This
  resolves the "vacuous pass" concern raised in the Phase 7
  §4.5c footnote: both soaks' WS drops are the same benign
  page-trim, not silent idle-decay.

- **Doc pass landed under this entry** — new §4.5d in
  [`memory-tiering-windows-host-validation.md`](docs/architecture/memory-tiering-windows-host-validation.md)
  carries the full `ws_bytes`-vs-`pm_bytes` breakdown plus the
  recommended post-v0.6.0 refinement (re-anchor the soak
  validator on `pm_bytes`; the field is already captured in
  every snapshot).  The matching pass criteria in
  [`memory-tiering-bake-criteria.md`](docs/architecture/memory-tiering-bake-criteria.md)
  §1.7 ticks `[x]` retroactively and carries an inline note on
  the WS-bound semantics so future operators see the WS-vs-PM
  nuance up front.

- **Remaining v0.6.0 gate work** is the **Phase 6 24-h soak
  re-run** (one more capture with the post-PR-218
  `shard.ttl=trace` harness fix) plus the one-week `main` bake.
  Phase 8 closed 2026-05-05; Phase 7 and ws-trace close
  2026-05-13.  No new operator-surface features land on `main`
  until v0.6.0 ships.

## [0.5.95] - 2026-05-12

### Added

- **ci** Cargo-fuzz in Tier 2 weekly, scoped to uffs-mft (R3-07, advisory) (#184)
- **ci** MSRV verification in Tier 2 weekly (R3-06) (#183)
- **ci** Cargo-careful in Tier 2, scoped to unsafe-density hotspots (R3-03) (#179)
- **ci** Cargo-semver-checks as second pre-publish guard (R3-05) (#178)
- **ci** Cargo-mutants in Tier 2, scoped to uffs-security (R3-04, advisory) (#180)
- **ci** Cargo-hack --each-feature in Tier 2 (R3-02) (#177)
- **ci** Cargo-machete as a hard gate at pre-push + pr-fast (R3-01) (#174)

### Fixed

- **release** Surface GitHub immutability conflict in pre-flight (issue #150) (#168)

### Security

- **security** Mandate audits over blanket vet exemption bumps (#172)
- **security** Replace lazy vet exemption bumps from #166 with proper delta audits (#170)

## [0.5.94] - 2026-05-09

### Fixed

- **release-automation** Scope publishability to polars-free crates (R6 deviation row 5 — Path A) (#155)
- **release-automation** Converge CC regexes — drop top-level `security:` (#154)

## [0.5.92] - 2026-05-08

### Added

- **ci** Gates manifest Phase 3a — _lint_fast.sh codegen + fast-drift gate (#144)
- **ci** Gates manifest Phase 3 — gen-workflow structural validator + workflow-drift gate (#143)
- **ci** Gates manifest Phase 2 — gen-hooks Rust generator + auto-generated pre-push hook (#141)
- **ci** Gates manifest Phase 1 — source of truth + drift detector (#140)
- **ci** Flip Windows CI gate to clippy + add Linux zigbuild accelerator (W5 + L1) (#138)
- **soak** Add ws-trace subcommand for 24h Working-Set trajectory (#137)
- **soak** Phase 6 + Phase 7 24h Windows-host validation harness (#135)

### Fixed

- **release-automation** Release-plz `release_always = false` to gate tag creation through release-plz-* PRs only (#151)
- **client** `await_ready` retries on transient errors instead of bailing (#147)
- **daemon** Adaptive EMA integration + shard.ttl 3-threshold shape (Phase 6 soak) (#146)
- **ci** Keep release cache warm so releases stop paying cold-build tax (#132) (#136)

### Added — Gates manifest Phase 3a: `_lint_fast.sh` codegen + `fast-drift` gate (PR #144)

Phase 3a of [`docs/architecture/gates-manifest-plan.md`](docs/architecture/gates-manifest-plan.md).
The pre-commit hook (`scripts/hooks/_lint_fast.sh`) is now generated
from the canonical manifest by the same `gen-hooks` binary that
already owned `_lint_pre_push.sh`; manual edits to the hook are
caught by a paired drift detector that hard-blocks merge.

- **EXTENDED `scripts/ci/gen-hooks/`** — `EmitTarget::PreCommit`
  variant added alongside the existing `PrePush`.  One binary now
  owns both hook files.  Modules:
  * `src/emit.rs` — `render_dispatch_fast()` + four per-gate emit
    shapes:
    1. **always-on hard** (`file-size`) — unconditional `spawn`.
    2. **always-on soft-skip** (`typos`, `reuse`) — `if command -v
       $tool >/dev/null 2>&1; then spawn ...`.
    3. **rust-or-no-staged** (`fmt`) — special predicate
       `if has_staged_rs || ! has_any_staged; then` so manual `just
       lint-fast` runs on a clean worktree are still useful as a
       sanity pass.
    4. **rust-staged group** (`lint-prod` / `lint-tests` / `lint-ci`)
       — collapsed into a single `if has_staged_rs; then ... fi`
       block (3 spawns instead of 3 separate guard blocks).
    5. **bespoke `taplo`** — `if has_staged_toml_nonvet && command
       -v taplo; then`, with a `bash -c` invocation rewritten from
       the manifest's `{{STAGED_TOML}}` placeholder into a literal
       command-substitution over `$STAGED_TOML_NONVET`.
    6. **bespoke `vet-fmt`** — `if has_staged_vet && command -v
       cargo-vet; then` (the `command -v cargo-vet` guard is at the
       dispatch level because at pre-commit a missing `cargo-vet` is
       a soft-skip; the upstream pre-push `vet` gate is the hard
       backstop).
  * `src/main.rs` — new `--target {pre-push,pre-commit}` flag
    (default `pre-push` for back-compat with the existing
    `hooks-drift` gate).  `--check` failure messages name the right
    `just` recipe per target.
  * `templates/preamble_fast.sh` + `templates/footer_fast.sh` —
    embedded scaffolding for `_lint_fast.sh`: colors, staged-file
    inventory, `has_staged_*` helpers, `spawn` helper, wait loop,
    per-job report, optional-tool hint, failure dump.  Pure bash;
    no per-gate knowledge.
- **NEW manifest gate `fast-drift`** — self-referential gate at
  `tiers = ["pre-push", "pr-fast"]`, `bucket = "bg"`, `gate_when =
  "always"`, `hard = true`, order 28 (next to `workflow-drift`'s 27
  in Bucket 1).  Sibling of `hooks-drift` for the pre-commit tier
  — same binary, different `--target`.  Lives in pre-push + pr-fast
  (NOT pre-commit) so the validator's compile cost doesn't break
  the T1 sub-2 s budget; pre-push catches the drift before it
  reaches the remote.
- **NEW `pr-fast.yml::fast-drift` job** — mirror of `hooks-drift`'s
  shape (cache shared with `sanity`).  Wired into `required.needs:`,
  the bash R=() aggregator, AND `notify-failure.needs:` (otherwise
  it would itself fail Property 3 of `workflow-drift` on first run).
- **NEW `just gen-fast`** + **`just fast-drift`** recipes — manual
  entry points for the pre-commit hook generator and its drift
  detector.  Mirrors the `just gen-hooks` / `just hooks-drift` /
  `just gen-workflow` / `just workflow-drift` recipe shape.
- **REGENERATED `scripts/hooks/_lint_fast.sh`** — first-time
  `gen-hooks --target pre-commit` output.  AUTO-GENERATED banner +
  embedded preamble + manifest-driven dispatch + embedded footer.
  The legacy hand-written inline dispatch comments are now stored
  in `gates.toml` `notes` (single source of truth, surfaced by
  `gen-hooks --verbose`).  Behavior is preserved at the spawn level:
  every gate fires on the same predicate as before, in the same
  parallel-fan-out shape.
- **REGENERATED `scripts/hooks/_lint_pre_push.sh`** — picks up the
  new `fast-drift` gate as a Bucket-1 entry alongside `gates-drift`,
  `hooks-drift`, `workflow-drift` (4 drift detectors covering 4
  orthogonal drift axes: gate-set / pre-push-hook-content /
  workflow-structural / pre-commit-hook-content).
- **MODIFIED `docs/architecture/gates-manifest-plan.md`** — Status
  table updated (Phase 3a ✅ landed); §9 action log entry appended.

Tests (32 total in `gen-hooks`, 10 new):
- **All four `emit_fast` shapes** — `fast_dispatch_emits_all_six_shapes`
  asserts every gate (file-size, fmt, typos, reuse, taplo, vet-fmt,
  lint-ci/prod/tests) renders with the right predicate + spawn label
  + manifest-order sort within the rust-staged block.
- **Single rust-staged block** — `fast_dispatch_emits_exactly_one_rust_staged_block`
  guards against a buggy refactor that loses the `emitted_rust_block`
  flag (would emit three separate `if has_staged_rs; then` blocks
  instead of one).
- **Consumer-name override** — `fast_dispatch_honours_pre_commit_consumer_override`
  asserts the `fmt` → `fmt-check` legacy rename is preserved at
  emit time.
- **Fmt's wider predicate** — `fast_emit_fmt_spans_no_staged_branch`
  verifies the `|| ! has_any_staged` clause survives any future
  refactor of the fmt special case.
- **Empty rust-staged group** — `fast_emit_rust_staged_block_is_empty_when_no_rust_gates`
  edge case: a manifest with `fmt` but no other `rust_changed`
  gates must NOT emit a dangling `if has_staged_rs; then ... fi`
  block.
- **Idempotency** — `pre_commit_render_is_idempotent` asserts two
  consecutive `EmitTarget::PreCommit.render(&m)` calls return
  byte-identical strings (plan §4.4 contract).
- **Render distinctness** — `pre_commit_and_pre_push_render_distinct_files`
  asserts the same manifest emits two materially different bash
  files (no template leak between targets).
- **Default output paths + tier strings** — `emit_target_default_output_paths_are_distinct`
  guards against a refactor that aliases the two targets' output
  paths or tier names.
- **`{{STAGED_TOML}}` placeholder is rewritten** — covered inside
  `fast_dispatch_emits_all_six_shapes`; a leak would be caught by
  the assertion that the literal placeholder string is absent from
  the emitted dispatch.

Verification:
- `cargo test -p uffs-gen-hooks` — 32 / 32 unit tests pass.
- `cargo clippy -p uffs-gen-hooks --bins --tests -- -D pedantic -D
  nursery -D cargo -W unwrap_used -W expect_used -W
  missing_docs_in_private_items -D warnings` — exit 0, **zero
  per-item suppressions in non-test code**.
- `cargo run -q --release -p uffs-gen-hooks -- --target pre-commit
  --check` — exit 0 against the regenerated `_lint_fast.sh`.
- `cargo run -q --release -p uffs-gen-hooks -- --check` — exit 0
  against the regenerated `_lint_pre_push.sh`.
- `bash scripts/ci/check_gates_drift.sh` — 23 gates matched.
- `cargo run -q --release -p uffs-gen-workflow -- --check` —
  exit 0 (workflow-drift sees the new `fast-drift` job).
- `actionlint .github/workflows/pr-fast.yml` — exit 0.
- `bash -n scripts/hooks/_lint_fast.sh` + `shellcheck
  scripts/hooks/_lint_fast.sh` — both exit 0.
- `just lint-pre-push` — full 23-gate sweep green.


### Added — Gates manifest Phase 3: `gen-workflow` structural validator + `workflow-drift` gate (PR #143)

Phase 3 of [`docs/architecture/gates-manifest-plan.md`](docs/architecture/gates-manifest-plan.md).
Pivoted from the originally-planned YAML emitter design (see PR #142
for the plan revision) to a `--check`-only structural validator that
catches every drift class the codegen design promised, at the same
risk profile as Phase 1's `gates-drift` — the tool only reads files;
it cannot break the workflow.

- **NEW `scripts/ci/gen-workflow/`** — Rust workspace member.  Reads
  `scripts/ci/gates.toml` AND `.github/workflows/pr-fast.yml`,
  validates four structural properties per plan §4.2:
  1. **Job presence** — every manifest gate with `tier="pr-fast"`
     has a corresponding job in the workflow (resolved via
     `consumer_names["pr-fast"]` if present, else gate id).  Multiple
     gates may fold into one job (e.g. `rustdoc` + `doc-tests` →
     `docs`); the validator handles many-to-one correctly.
  2. **`if:` predicate alignment** — for each pr-fast job, the job's
     `if:` predicate must accept every change class the folded
     gates' `gate_when` values require.  Implemented as a
     `PermissiveSet` u8 bitset lattice (rust / dep / infra / always)
     with `union` and `contains` operations.  Wider predicates pass
     (over-runs are fine); narrower ones fail (drift would block a
     gate from running on its trigger).
  3. **Aggregator coverage** — every gate's resolved job-id must
     appear in `required.needs:`, the bash `declare -A R=(...)`
     aggregator inside the `required` job, AND `notify-failure.needs:`.
     This is the exact rename-bookkeeping failure mode (PR #138's
     windows-check → windows-lint rename touching 6 files) that
     motivated the whole gate-manifest plan.
  4. **Branch-protection guard** — the `required` job's `name:`
     field is exactly `PR Fast CI / required` — the literal string
     in the repo's branch-protection rule.  A future refactor that
     renamed it would silently break merge for every PR; the
     validator now hard-fails before that lands.
- **Hand-rolled minimal YAML extractor** — instead of pulling in
  `serde_yml` (archived 2024, active `RustSec` advisory in its
  `Serializer.emitter`), `serde_yaml_ng` / `serde_norway` (both
  depend on `unsafe-libyaml` C code), or one of the not-yet-
  battle-tested pure-Rust forks, the crate parses just the four
  fields it needs (`jobs:` keys, per-job `name:` / `if:` / `needs:`)
  with ~120 lines of focused string-matching Rust.  Zero new
  dependencies, zero advisory exposure, zero cargo-vet exemptions
  added.  All three `needs:` shapes (single string, flow-style
  list, block-style list) are handled correctly.
- **NEW manifest gate `workflow-drift`** — self-referential gate at
  `tiers = ["pre-push", "pr-fast"]`, `bucket = "bg"`, `gate_when =
  "always"`, `hard = true`.  Order 27 (next to `hooks-drift`'s 26
  in Bucket 1).  Pairs with Phase 1's `gates-drift` (gate-set
  drift) and Phase 2's `hooks-drift` (hook-content drift) to cover
  three orthogonal drift axes.
- **NEW `pr-fast.yml::workflow-drift` job** — same shape as
  `hooks-drift` (cache shared with `sanity` so the gen-workflow
  binary build piggybacks on the existing rust-cache).  Always-on,
  added to `required.needs:`, the bash R=() aggregator, AND
  `notify-failure.needs:` (otherwise it would itself fail Property 3
  on first run — a satisfying recursive consistency check).
- **NEW `just workflow-drift`** + **`just gen-workflow`** recipes —
  manual entry points for the validator.
- **MODIFIED `Cargo.toml`** — `scripts/ci/gen-workflow` added as a
  workspace member alongside `scripts/ci-pipeline` and
  `scripts/ci/gen-hooks`.
- **MODIFIED `scripts/hooks/_lint_pre_push.sh`** — regenerated by
  `gen-hooks` to pick up the new `workflow-drift` gate (Bucket 1,
  `cargo run -q --release -p uffs-gen-workflow -- --check`).
- **MODIFIED `docs/architecture/gates-manifest-plan.md`** — Status
  table updated (Phase 3 ✅ landed); §9 action log entry appended.

Tests (33 total in `uffs-gen-workflow`):
- **Manifest module** (7) — minimal-subset parsing, `pr_fast_gates`
  filtering, `consumer_names` override resolution, `gate_when` ⇄
  `when` rename round-trip, unknown-fields-ignored design choice
  (regression-guards the schema-drift safety net), missing-required-
  field-fails-noisily.
- **Workflow extractor** (12) — all three `needs:` shapes, quoted /
  unquoted field values, nested `with:` / `env:` / `run: |` blocks
  ignored cleanly, missing `jobs:` key fails with context, empty
  `jobs:` block fails, malformed flow list fails with helpful
  message, block-list with blank-line terminator, `job_key_at`
  rejects inline values + accepts trailing comments, `unquote`
  handles single/double/bare/whitespace/comment cases.
- **Validator** (14) — `PermissiveSet` lattice unit tests
  (gate_when→set, if_expr→set, mixed-class union semantics), happy-
  path consistent-fixture assertion, plus seven mutation tests:
  one per property × failure mode (P1 missing job, P2 too narrow,
  P2 wider predicates pass, P3 missing-from-required-needs, P3
  missing-from-aggregator, P3 missing-from-notify-needs, P4
  renamed required, P4 missing required job).  Aggregator-extraction
  helper tested on a realistic-shape table and confirmed to ignore
  unrelated brackets (`${{ matrix.os }}` substitutions, mid-line
  `[other-bracket]=` lines outside the table).

Verification:
- `cargo test -p uffs-gen-workflow` — 33 / 33 unit tests pass.
- `cargo clippy -p uffs-gen-workflow -- -D pedantic -D nursery -D
  cargo -W unwrap_used -W expect_used -W
  missing_docs_in_private_items -D warnings` — exit 0, **zero
  per-item suppressions**.
- `cargo run -q --release -p uffs-gen-workflow -- --check` — exit 0
  against the current `pr-fast.yml`.
- `bash scripts/ci/check_gates_drift.sh` — 22 gates matched
  (Phase 1 detector, +1 from Phase 2's 21).
- `cargo run -q --release -p uffs-gen-hooks -- --check` — exit 0
  (Phase 2 detector, regenerated hook in lockstep).
- `just lint-pre-push` — full 22-gate sweep green in 53 s warm
  (matches pre-Phase-3 budget; no regression from the +1 Bucket 1
  gate).
- `actionlint .github/workflows/pr-fast.yml` — exit 0.
- `cargo deny check` — exit 0 (no advisories from the `serde_yml`
  pivot since the dep was never added).


### Added — Gates manifest Phase 2: `gen-hooks` Rust generator + auto-generated pre-push hook (PR #141)

Phase 2 of [`docs/architecture/gates-manifest-plan.md`](docs/architecture/gates-manifest-plan.md).
The pre-push hook (`scripts/hooks/_lint_pre_push.sh`) is now
generated from the canonical manifest by a new Rust binary; manual
edits to the hook are caught by a paired drift detector that hard-
blocks merge.

- **NEW `scripts/ci/gen-hooks/`** — Rust workspace member implementing
  the `gen-hooks` binary per plan §4.1.  Modules:
  - `manifest.rs` — serde model + lightweight invariant validation
    (no duplicate ids, valid bucket per pre-push tier, valid
    `gate_when`, valid tier names).  The TOML-side `gate_when` field
    is bridged to the Rust-side `when` via `serde(rename)` so the
    schema stays unchanged while the Rust struct stays clean.
  - `emit.rs` — banner + dispatch generation.  Per-gate special cases
    are dispatched explicitly (no generic-template engine):
    `commit-subjects` (multi-line `bash -c` reading `COMMIT_RANGES`),
    `cargo-vet` (DEP_CHANGED + missing-tool hard-fail with install
    hint, closes the PR #43 loophole), soft-skip-with-`command -v`
    for any non-assumed tool, `dep_changed` inner guard for Bucket 2
    gates that need it.
  - `templates/preamble.sh` + `templates/footer.sh` — embedded bash
    scaffolding (colors, change-classification, `spawn_bg` /
    `run_seq` helpers, bucket reaping, optional-tool hint, failure
    dump).
  - 24 unit tests covering schema parsing, validation invariants,
    `gate_when` ⇄ `when` rename round-trip, `consumer_names` per-tier
    label override (regression-guards the `test-build` ⇄ `tests`
    legacy mapping), pr-fast-only gates legitimately omitting
    `bucket`, every special-case emission pattern, and the §4.4
    idempotency contract.
- **MODIFIED `scripts/hooks/_lint_pre_push.sh`** — now generated.
  Header carries the `AUTO-GENERATED by ... MANUAL EDITS WILL BE
  OVERWRITTEN` banner + a quick-link to the manifest and the regen
  recipe.  Total file shrinks from 406 to 322 lines because the
  per-gate documentation comments now live in the manifest's
  `notes` fields (single source of truth).
- **NEW manifest gate `hooks-drift`** — self-referential gate that
  runs `gen-hooks --check` to verify the on-disk hook is byte-for-
  byte equal to what the generator would emit.  Pairs with Phase
  1's `gates-drift`: the latter ensures the gate-set matches across
  consumers; this one ensures the emitted hook matches what the
  manifest would produce.  Both run as Bucket 1 spawn_bg jobs in
  the pre-push hook and as always-on jobs in `pr-fast.yml`.
- **NEW `pr-fast.yml::hooks-drift` job** — runs `cargo run -q
  --release -p uffs-gen-hooks -- --check` on every PR.  Cache key
  shared with `sanity` so the gen-hooks binary build piggybacks on
  the existing rust-cache.  Added to `required.needs:`,
  success-conditional aggregation, and `notify-failure.needs:`.
- **NEW `just gen-hooks`** recipe — manual regen entry point.
- **NEW `just hooks-drift`** recipe — manual drift-check entry
  point.
- **MODIFIED `Cargo.toml`** — `scripts/ci/gen-hooks` added as a
  workspace member alongside `scripts/ci-pipeline`.
- **MODIFIED `docs/architecture/gates-manifest-plan.md`** — Status
  table updated (Phase 1 ✅ landed, Phase 2 🟡 in flight); §9 action
  log appended.

Verification:
- `cargo test -p uffs-gen-hooks` — 24 / 24 unit tests pass.
- `cargo clippy -p uffs-gen-hooks -- -D clippy::pedantic -D
  clippy::nursery -D clippy::cargo -W clippy::unwrap_used -W
  clippy::expect_used -W clippy::missing_docs_in_private_items -D
  warnings` — exit 0, no per-item suppressions in non-test code.
- `bash scripts/ci/check_gates_drift.sh` — 21 gates correctly
  matched against the regenerated hook.
- `just hooks-drift` — exit 0 (idempotent regen).
- `just lint-pre-push` — full 21-gate sweep green in 53 s warm
  (matches pre-Phase-2 budget).
- `bash -n scripts/hooks/_lint_pre_push.sh` — syntax OK.
- `actionlint .github/workflows/pr-fast.yml` — exit 0.


### Added — Gates manifest Phase 1: source-of-truth + drift detector (PR #140)

Phase 1 of [`docs/architecture/gates-manifest-plan.md`](docs/architecture/gates-manifest-plan.md)
(itself the implementation companion to
[`docs/architecture/dev-flow-implementation-plan.md` §2.7](docs/architecture/dev-flow-implementation-plan.md)).
Closes the "documented but not implemented" status of §2.7 with the
foundation for the upcoming Phase 2 + Phase 3 codegen work.

- **NEW `scripts/ci/gates.toml`** — declarative source-of-truth for
  the workspace's PR-time gate set (20 entries covering pre-commit /
  pre-push / pr-fast tiers).  Each `[[gate]]` entry carries id,
  label, command, tier membership, change-classification trigger,
  hard/soft semantics, missing-tool detection key, expected runtime,
  pre-push bucket assignment, and free-form notes.  Per-tier
  consumer-name overrides via `consumer_names` table cover the few
  gates whose hook id differs from their pr-fast.yml job name (e.g.
  manifest `lint-ci` ⇄ pr-fast `clippy`, manifest `cargo-check` ⇄
  pr-fast `sanity`, manifest `vet` + `deny` ⇄ pr-fast `security`).
- **NEW `scripts/ci/check_gates_drift.sh`** — bidirectional drift
  detector.  Forward direction: every manifest `[[gate]]` entry must
  appear in its declared tiers' consumer files (pre-commit hook,
  pre-push hook, pr-fast.yml).  Reverse direction: every gate
  defined in a consumer (via `spawn`/`spawn_bg`/`run_seq` in the
  hooks, or top-level YAML job under `jobs:` in pr-fast.yml) must
  have a matching manifest entry — except for orchestration-only
  jobs (`classify`, `required`, `notify-failure`).  Bash-only;
  awk-based TOML parsing (no extra runtime deps).  Bypass once via
  `BYPASS_GATES_DRIFT=1 git push` for emergency landings; CI has no
  bypass — drift on `main` is a deliberate "fix-me-now" signal.
- **MODIFIED `scripts/hooks/_lint_pre_push.sh`** — drift check
  added as a new Bucket 1 step (cheap, parallel, fire-and-forget;
  same tier as `fmt` / `file-size`).
- **MODIFIED `.github/workflows/pr-fast.yml`** — NEW `gates-drift`
  job (always-on, no classify gating; sub-second; modeled after
  `file-size`).  Added to the `required` aggregator's `needs:`
  list, success-conditional declare-A array, and `notify-failure`'s
  `needs:` list so a manifest mismatch hard-blocks merge.
- **NEW `just gates-drift`** recipe — manual invocation surface.
- **MODIFIED docs/architecture/gates-manifest-plan.md** — Status
  table updated (Phase 0 ✅, Phase 1 🟡 in flight); §9 action log
  updated.

Verification:
- `bash scripts/ci/check_gates_drift.sh` — exit 0 against current
  `main` (20 gates correctly matched).
- Mutation tests: injecting a fake gate into the manifest fires the
  forward-direction error; injecting a `spawn_bg "ghost-gate"` into
  the pre-push hook fires the reverse-direction error; restoring
  the original state returns to clean.
- `BYPASS_GATES_DRIFT=1` exits 0 immediately with a tombstone log
  line.
- `actionlint` exit 0 on the modified `pr-fast.yml`.
- `shellcheck scripts/ci/check_gates_drift.sh` exit 0 (3 SC2016
  false-positive backtick warnings explicitly disabled with
  inline directives).


### Changed — Windows clippy CI/pre-push flip + Linux zigbuild accelerator (PR #138)

Closes Phases **W5** and **L1** of
[`docs/architecture/windows-clippy-and-linux-cross-plan.md`](docs/architecture/windows-clippy-and-linux-cross-plan.md).
Every plan phase (W0 baseline, W1 recipes, W2 prod, W3-W5 tests, W5.5
CI flip, W5.6 pre-push flip, W5 follow-on Tier-2 redundancy cleanup,
L1 zigbuild) now ✅; §8 acceptance items all checked.

- **`pr-fast.yml::windows-check` → `windows-lint`** (Phase W5.5).
  Renamed the job and switched the command from `cargo check` to
  `cargo clippy --workspace --all-targets --all-features --locked
  --no-deps -- -D warnings` natively on `windows-latest`.  PR #62
  cleared the Windows clippy backlog (W0 baseline: 1346 errors → 0)
  so the strict-clippy stack now exits 0.  Aggregator (`required`)
  + `notify-failure` `needs:` lists updated; `preview-artifacts.yml`
  comment refreshed.
- **`scripts/hooks/_lint_pre_push.sh`** dispatches `just lint-ci-windows`
  (cargo xwin clippy with the same `-D warnings` stack) instead of
  `just check-windows` (compile-only) (Phase W5.6).  W1.4 measurement
  pegs the upgrade at ~6 s warm; pre-push budget unchanged at
  ~25–60 s warm.  Net: a Windows-only `unwrap_used` /
  `cast_possible_truncation` regression now hard-fails BOTH the local
  pre-push hook AND the authoritative PR Fast CI job, on the same
  surface and flag stack.
- **NEW `just lint-ci-linux-zig` recipe** (Phase L1.2): native
  macOS → Linux clippy via `cargo-zigbuild` (no Docker required).
  ~50 s cold, sub-second warm.  Docker `lint-ci-linux` remains the
  authoritative gate (mirrors CI's `rust:latest` image exactly);
  zigbuild is a developer-loop accelerator for fast inner-loop sweeps.
  `check-all-targets` prefers zigbuild when `zig` + `cargo-zigbuild`
  are both on PATH, falls back to Docker, soft-skips when neither.
- **`install-dev-tools`** extended (macOS hosts only) to install
  `zig 0.14.1` from the official `ziglang.org` tarball into
  `~/.local/zig/0.14.1/`, symlink it into `~/.cargo/bin/zig`, install
  `cargo-zigbuild`, and add the `x86_64-unknown-linux-gnu` rustup
  target.  Pinned to 0.14.1 because Homebrew's `zig` formula tracks
  latest (currently 0.16.x) which has unrelated incompat issues with
  `psm`'s `src/arch/x86_64.s` ATT-syntax assembly.
- **Two non-obvious gotchas baked into the L1 recipe** (surfaced
  during empirical L1.3 verification):
  1. Recipe overrides `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS`
     to pin `target-cpu=x86-64-v3` (matches `release.yml`'s Linux
     baseline) for the cross-compile only.  `.cargo/config.toml`'s
     default `target-cpu=native` resolves to `apple-m4` on macOS,
     which `cargo-zigbuild` propagates to `zig cc -mcpu=native`,
     which corrupts zig's integrated-assembler dialect detection
     for hand-written x86_64 SIMD asm in `psm` and `blake3`.
  2. Recipe invokes `cargo-zigbuild clippy` directly (the binary
     exposes `clippy` / `check` / `test` as proper subcommands)
     rather than `cargo zigbuild clippy` (cargo plugin form), which
     always routes into the `zigbuild` build subcommand.
- **`tier-2.yml::windows-check` REMOVED** (W5 follow-on, plan §5).
  Pre-W5.5 it ran `cargo check --workspace --all-features
  --all-targets` weekly on `windows-latest` as the backstop catching
  Windows-only regressions before `just ship`.  With `windows-lint`
  now running strict clippy on every PR (which does a full
  type-check + executes every dep's `build.rs`), the weekly job
  became strictly redundant.  Tombstoned with an inline comment in
  `tier-2.yml` explaining the removal; references in the
  `tier-2-summary` and `notify-failure` `needs:` lists + the success
  conditional + the summary-table line all dropped.  Tier 2 stays
  the deep-assurance lane (coverage, miri, udeps).
- **Docs**: `CONTRIBUTING.md` four-layer table + cross-platform
  section refreshed for the new gate names + flag stack;
  `windows-clippy-and-linux-cross-plan.md` gets a `Status (2026-05-06)`
  header with every phase ✅ (including the Tier 2 follow-on),
  both L1 gotchas documented, §8 acceptance criteria all checked,
  §9 cross-references refreshed; `dev-flow-implementation-plan.md`
  + `dev-flow.md` + `supply-chain-posture.md` updated for the
  post-W5 job names and the Tier 2 removal.

## [0.5.90] - 2026-05-05

### Added

- **scripts** Enhance daemon-readiness with Phase 8 + 9 capability tests (#130)
- **daemon** Phase 9 — wire real promotions_total counter + decompose DriveStats (#129)
- **daemon** Phase 8-D + 8-E — forget/status_drives commands (#123)
- **daemon** Phase 8-B + 8-C — hibernate/preload commands with tier pinning (#122)
- **daemon** Phase 8-A — RPC scaffolding for hibernate/preload/forget/status_drives (#121)
- **daemon** Phase 8 — surgical body-patch via apply_usn_patch_to_body (#120)
- **daemon** Per-shard USN journal loops replace global 5-min tick (Phase 7 activation) (#118)

### Fixed

- **daemon/lifecycle** Disarm load-stall force-retire after initial loading completes (#116)

### Added — Phase 8: operator-driven memory tiering (v0.6.0 staging)

The full operator-facing memory-tiering surface — every command end-to-end
from CLI → typed JSON-RPC client → daemon handler → `IndexManager`
primitives → registry / cache / pin atomics.  See the
[Windows-host runbook](docs/architecture/memory-tiering-windows-host-validation.md)
for the operator-facing validation flow.

- **`uffs daemon hibernate [DRIVES…]`** (Phase 8-B, PR #122) — demote
  loaded shards to `Cold` in a single write-lock batch.  Empty drive
  list ⇒ every loaded drive.  Releases the in-memory body but keeps
  the encrypted compact cache on disk so a subsequent search or
  `preload` can re-warm without a full MFT re-parse.  Reports the
  per-pre-call-tier breakdown (`hot_demoted` / `warm_demoted` /
  `parked_demoted` / `already_cold`) so the operator audit trail
  captures what actually changed.

- **`uffs daemon preload <DRIVES…> [--pin-minutes N]`** (Phase 8-C,
  PR #122) — promote drive(s) to `Hot` and pin the tier against
  demote for `N` minutes (default 30).  Pin contracts:
  - Cold/Parked/Warm → Hot via single-flight body load + registry
    rebuild.
  - Already-Hot drives skip the rebuild entirely and atomically
    extend the pin via `ShardEntry::pin_until`.
  - Pinned shards survive idle-tick demote (`demote_idle_shards`)
    and pressure-cascade demote (`cascade_demote_one_step`).
  - Explicit `hibernate` and `forget --force` override the pin via
    registry rebuild (the rebuilt `ShardEntry` starts at
    `pin_until_ms = 0`).

- **`uffs daemon forget <DRIVES…> [--force]`** (Phase 8-D, PR #123) —
  evict drive(s) from the registry **and** delete every per-drive
  on-disk cache artefact (encrypted compact body, USN cursor, MFT
  index, lock file).  Three-phase orchestration:
  1. Read-lock detect — refuse the whole request with
     `ERR_DRIVE_BUSY` if any drive is non-`Cold` and `--force` is
     not set, so a typo on one of five drives cannot accidentally
     forget the other four.
  2. Optional auto-hibernate (`--force` only) — demote every
     non-`Cold` drive to `Cold` first via
     `OperatorHibernate`-tagged `demote_letter_with_reason` calls,
     clearing pins implicitly.
  3. Per-drive evict + clean — `freed_bytes > 0` ⇒ `forgotten`,
     idempotent re-runs land in `already_absent`.

- **`uffs daemon status_drives`** (Phase 8-E, PR #123) — per-drive
  tier + telemetry table.  Operator-facing companion to `daemon
  status`: surfaces tier, pin expiry, query rate (EWMA), resident
  bytes, and last-query timestamps for every drive the registry
  knows about — including `Cold` shards (encrypted cache on disk,
  zero RAM) so `forget` candidates are visible without
  cross-referencing tracing logs.  Output sorted ascending by drive
  letter so the table is stable across re-runs.

- **`ShardEntry::pin_until_ms`** atomic field (Phase 8-C) — new
  `AtomicU64` on every shard, initialised to `0` (unpinned) by all
  four constructors.  `is_pinned(now_ms)` predicate folds the
  `now_ms` comparison in for the demote-side gates;
  `pin_until_ms_value()` accessor (added in Phase 8-E) returns the
  raw timestamp for the `status_drives` wire output.

- **`OperatorHibernate` `DemoteReason` variant** — surfaces in the
  canonical `shard.transition` tracing event with
  `reason="operator-hibernate"` so operators can grep the audit
  trail to distinguish manual hibernation from idle-tick or
  pressure-cascade demotes.

- **`CacheCleaner` lifecycle hook** (Phase 8-D) — new
  `Arc<dyn CacheCleaner>` field on `LifecycleHooks`.
  `PlatformCacheCleaner` resolves the four canonical per-drive
  paths via `uffs_core::compact_cache` / `uffs_mft::cache` and
  unlinks each via `std::fs::remove_file`; `CountingCacheCleaner`
  test fake records the call sequence so registry-eviction
  behaviour can be verified without ever touching the host's real
  cache directory.

- **`promote_letter_to_hot` registry method** (Phase 8-C) — mirrors
  `promote_letter` but rebuilds the registry with a `Hot` shard.
  Accepts `Cold` / `Parked` / `Warm` source states; rejects `Hot`
  (caller extends pin via atomic store on the live `Arc`) and
  `Unknown` / `Evicting` (controller-only).


### Added — wire format

- **4 new JSON-RPC methods**: `hibernate`, `preload`, `forget`,
  `status_drives` (Phase 8-A scaffolding pre-existing on `main`).
- **2 new application error codes**: `ERR_DRIVE_BUSY = -4` (forget
  refused without `--force`), `ERR_NOT_IMPLEMENTED = -3` (retired in
  Phase 8-D once every Phase 8-A stub became a real handler).
- **6 new wire types** in `uffs-client::protocol::response_tiering`:
  `HibernateParams` / `HibernateResponse`, `PreloadParams` /
  `PreloadResponse`, `ForgetParams` / `ForgetResponse`,
  `StatusDrivesParams` / `StatusDrivesResponse`,
  `DriveTierStatus`, plus `DEFAULT_PRELOAD_PIN_MINUTES = 30`.


### Added — typed RPC client surface

- **`UffsClientSync::{hibernate, preload, forget, status_drives}`**
  in `uffs-client::connect_sync_tiering` — typed envelope helpers.
  Matches the existing `connect_sync` shape; sibling-module split
  keeps both files under the 800-LOC ceiling without an exception.


### Added — tests

- **22 new daemon-side integration tests** covering the full pin
  contract surface, the all-or-nothing forget refusal, the
  auto-hibernate-then-evict path, the deterministic `status_drives`
  sort, and per-tier `resident_bytes` calculation across the
  Hot/Warm/Parked/Cold ladder.  Plus 4 unit tests on the
  `delete_drive_cache_files` helper using `tempfile::TempDir` so
  the on-disk cleanup logic is exercised without ever touching the
  host's cache directory.


### Fixed — Dependabot pipeline (PR #126)

- **`dependabot.yml` cargo-ecosystem prefix** flipped from `deps` to
  `chore` so generated PR titles (`chore(deps): bump foo …`) pass
  `.github/workflows/commitlint.yml`'s Conventional Commits
  allowlist.  The previous `deps` prefix was never in the allowlist;
  every cargo Dependabot PR was failing the title check.
- **`commitlint.yml` advisory step** — pass `--repo` explicitly to
  `gh pr comment` so the workflow does not crash when invoked
  without an `actions/checkout` step.  The `gh` CLI was falling
  through to a `fatal: not a git repository` error, which Bash's
  `set -euo pipefail` propagated past the documented advisory-mode
  `exit 0` escape hatch — turning every non-conforming PR title
  into a red required check rather than the documented advisory
  warning.

## [0.5.86] - 2026-05-02

### Added

- **daemon** Phase 6 — adaptive TTL ladder + per-drive min_tier + shard.ttl tracing + index decomposition (#112)

### Fixed

- **daemon** PR-f — refresh last_query_at_ms on promote to prevent demote thrash (#111)

## [0.5.85] - 2026-05-01

### Added

- **daemon** Phase 5 — Win32 memory-pressure watcher thread (5.3 watcher landing) (#108)

### Fixed

- **daemon** PR-e — single-flight promote dedup eliminates Windows 36 GB thundering-herd (#109)

## [0.5.83] - 2026-04-30

### Added

- **daemon** Phase 5 — pressure signal + cascade demote + USN background priority (5.3, 5.6, 5.7, 5.10) (#106)
- **daemon** Phase 5 traits + wiring — WorkingSetTrim + Prefetch (5.1, 5.2, 5.4, 5.5, 5.8, 5.9) (#104)
- **daemon** Phase 5.11 — status RPC enumerates Parked/Cold shards with tier markers (#103)

### Fixed

- **core** Self-heal v0.4.23 legacy <DRIVE>_compact.uffs directories on Unix (#105)

## [0.5.82] - 2026-04-28

### Added

- **daemon** Shorter default idle TTLs for fast dev iteration (#100)

## [0.5.81] - 2026-04-28

### Added

- **daemon** Phase 5 — parallel re-promote + USN replay on warm load + background refresh timer (closes #93, #94, #95, #96) (#98)

### Fixed

- **ci** Windows release ZIP root no longer wraps in release-staging/ (#97)

## [0.5.80] - 2026-04-28

### Added

- **core,daemon** Phase 4 — bloom + path trie (memory tiering 🎯) (#90)
- **daemon** Phase 3 — state machine + lazy body load (memory tiering) (#89)

## [0.5.79] - 2026-04-27

### Added

- **core** Phase 2b memory tiering — runtime mmap for compact-cache hot path (#85)

### Fixed

- **ci** Release-pipeline followups (#80, #83) (#84)

## [0.5.78] - 2026-04-27

### Added

- **core** Phase 2a memory tiering — ColumnStorage<T> wrapper for compact-index columns (#81)
- **daemon** Phase 1 memory tiering — ShardRegistry + per-drive stats (#79)

### Fixed

- **daemon,build** Clamp mimalloc_committed underflow on macOS + treat empty dist/ as cache miss in just use (#78)

## [0.5.77] - 2026-04-27

### Added

- **daemon** Phase 0 memory tiering — surface RSS + mimalloc committed bytes (#76)

## [0.5.76] - 2026-04-26

### Fixed

- **api-validation** Dot-gated extension extraction so .bash_history sorts as empty (#73)

## [0.5.75] - 2026-04-26

### Fixed

- **daemon** Emit version on `uffsd --version` (#71)

## [0.5.74] - 2026-04-26

### Breaking Changes

- **release-automation** R2 git-cliff template + cliff.toml at workspace root (#66)

### Fixed

- **mcp** Dot-gated extension extraction so .bash_history sorts as empty (#69)

## [0.5.73] - 2026-04-25

### Fixed

- **release** Re-codesign macOS binaries after strip — v0.5.73 (#63)

## [0.5.72] - 2026-04-25

### Changed
- **W2–W5: Windows MSVC clippy strict-gate cleanup** (40 commits across
  `uffs-mft`, `uffs-broker`, `uffs-daemon`, `uffs-cli`, `uffs-client`,
  `uffs-core`, `uffs-mcp`, `uffs-diag`).  Brings the workspace to
  clippy-clean on the Windows MSVC strict gate (`cargo xwin clippy
  --workspace --target x86_64-pc-windows-msvc --all-targets -- -D
  warnings`) without weakening any lints, while preserving the existing
  macOS host clippy contract.  Highlights:
  - **`uffs-mft`** — exhaustive `indexing_slicing` cleanup across all
    IOCP / parallel / sliding-window readers, the `to_index` /
    `to_index_parallel` pipelines, and `multi_volume.rs`; per-call-site
    fixes only (no module-level allows).  Adopted `&raw mut`/`&raw
    const` for FFI call sites (Win32 IOCP + USN), eliminated all
    `borrow_as_ptr` lints, moved `?` outside `unsafe` blocks, and
    converted `u32`↔`u64` LCN casts to explicit
    `cast_signed`/`cast_unsigned`.  Replaced `default_numeric_fallback`
    via explicit type annotations across reader / IO / stats paths.
    Renamed all single-character bindings (closures, match arms,
    pattern destructuring) to descriptive names.  Adopted
    `Duration::div_ceil`, `u64::cast_signed()`, and `if let Some(x) =
    &foo` over `if let Some(ref x) = foo`.  Added `# Errors` sections
    to every public `Result`-returning MFT reader / volume / USN API.
    Backticked common Win32/NTFS identifiers in doc comments.
  - **`uffs-daemon`** — refactored nine functions over the
    `cognitive_complexity` threshold without weakening the lint:
    `ensure_drives_loaded`, `run_ipc_server` (unix), `handle_search`,
    `refresh`, `load_single_mft_file`, `load_from_data_dir`,
    `run_aggregations` (also dropped from 9-arg to 4-arg via new
    `AggregationRequest` struct), `run_idle_timer`, and the
    215-line `run_daemon` (97/25 → ≤25, split into thirteen named
    helpers covering panic-hook install, lifecycle bootstrap, MFT
    file gathering, drive-list resolution, IPC + stats spawn, load
    task, zero-drive shutdown guard, and graceful shutdown).
    Added unit tests pinning the contracts of `infer_drive_letter`,
    `is_live_drive_marker`, and `drive_letter_matches`.
    `resolve_refresh_mft_source` no longer needs an `anyhow::Result`
    wrapper — non-Windows guard moved to the `spawn_blocking`
    closure where `?` propagates a real error.
  - **`uffs-broker`** — surgical clippy cleanup; `broker.rs` is now
    0 lints under the Windows strict gate.
  - **Eliminated all transient `#[expect]`s introduced by the
    refactor**: the only suppressions remaining in the daemon crate
    are pre-existing maintainer-approved ones (FFI safety, JSON-RPC
    float arithmetic for stats, unstable-`error_in_core`,
    unstable-`Duration::from_mins`, the `[diag]` block tagged for
    removal after the D: drive issue is resolved).

### Removed
- **Stale file-size-policy exceptions** for `crates/uffs-cli/src/main.rs`
  and `crates/uffs-core/src/search/sorting.rs` — both are back under
  the 800-LOC cap after the args-extraction and dataframe-convert
  splits respectively.

### Added
- **`crates/uffs-daemon/src/lifecycle.rs`** added to the file-size
  exception list (827 LOC) with a documented rationale: the
  `LifecycleManager` + `LifecycleHandle` + `run_idle_timer` state
  machine forms a single cohesive unit (active-connection guard,
  load-stall heartbeat, session-tier deadline extension); splitting
  fragments shutdown semantics across files.

### Changed
- **Close stale `ci-failure-tier-1` issue notifications**
  (2026-04-24 — GitHub issues #44 and #19).  Housekeeping
  follow-up to the Phase 4 cutover: both issues were auto-
  generated by the retired `ci.yml` "🧪 UFFS Tier 1 Nightly CI"
  workflow.  #44 was the `cargo vet check --locked` red on PR
  #43's first run (`pastey:0.2.2` and `rustls:0.23.39` missing
  `safe-to-deploy`), resolved in PR #45 (`780c1dbb1`) by adding
  `[[audits.pastey]]` and `[[audits.rustls]]` entries to
  `supply-chain/audits.toml`; PR #43 subsequently went green on
  its later commits and landed as `6a4d572e0`.  #19 was the
  shared-bucket Tier-1 failure issue on the pre-org-move fork
  (`githubrobbi/UltraFastFileSearch`) that accumulated 117
  auto-comments before the repo moved to the `skyllc-ai` org.
  Since PR #48 (`6f99b86aa`) deleted `.github/workflows/ci.yml`
  and the surviving `pr-fast.yml` / `tier-2.yml` / `release.yml`
  notify-failure jobs use distinct `ci-failure-pr-fast` /
  `ci-failure-tier-2` / `ci-failure-release` labels and query
  `listForRepo` by those per-workflow labels, no future workflow
  can append to or reopen the `ci-failure-tier-1`-labelled
  issues.  Both closed as `completed` with comments linking the
  fix PRs.
- **Phase 4 CI cutover — `ci.yml` retired, `pr-fast.yml` is now
  the sole required lane** (2026-04-23 —
  `.github/workflows/ci.yml` deleted,
  `docs/architecture/dev-flow-implementation-plan.md` §4
  branch-protection checklist).  Completes the shift-left rollout
  scoped by the dev-flow implementation plan:
  - **Before**: two parallel CI lanes.  `ci.yml` (legacy Tier 1)
    with 6 required `Tier 1 / *` checks — Format, Clippy, Rustdoc,
    Security, File Size Policy, and the tests matrix — ran on
    every push to `main` / `develop` and on every PR to `main`
    regardless of what files changed.  `pr-fast.yml`
    (bucket-ordered PR-fast) was added in PR #45 and ran in
    parallel with `ci.yml` to validate equivalence.
  - **After**: single required lane.  `pr-fast.yml` reports exactly
    one required status check — `PR Fast CI / required` — which
    aggregates the 8 classify-gated downstream jobs (fmt, sanity,
    clippy, docs, test-build, tests, security, windows-check) plus
    the unconditional `file-size` job via `success|skipped` logic.
    Docs-only / dep-only / pure-infra-only PRs skip the heavy jobs
    and still report green, saving ~5-7 min of runner time per
    non-code PR.  The classify-aggregation branch explicitly
    depends on the classify job's own result, so a classify
    failure flips `required` red even though every downstream job
    would otherwise be `skipped` (validated live on PR #45 via
    broken-classify simulation — classify=red 4s, 8 downstream
    skipped, `required`=red 4s).
  - **Branch-protection ruleset** (`main-protection`, ID
    `11889528`) updated in the same window via the rulesets API
    (classic `/branches/main/protection` is 404 on this repo):
    required-checks list goes from the 7-entry parallel-window
    shape (6 `Tier 1 / *` + `PR Fast CI / required`) to a single
    `PR Fast CI / required` entry.  The context string is the
    job's `name:` attribute (`PR Fast CI / required`), NOT the
    UI-displayed `<workflow> / <job>` concatenation — a gotcha
    first hit on 2026-04-23 and documented in §4.4 of the plan
    doc.
  - **Bake-in evidence**: PR #45 (mixed rust + dep + infra,
    code=true) ran the full PR-fast matrix alongside `ci.yml` and
    both stayed green; PR #46 (docs-only, code=false) exercised
    the classify skip branch — downstream skipped 8 jobs,
    `required` green in ~4 s, `ci.yml` path-filter correctly
    didn't fire; PR #47 (infra-only Phase 4b retrofit) ran every
    PR-fast gate and stayed green.  Combined with the live broken-
    classify simulation on PR #45 (required=failure propagated
    correctly through 8 skipped downstream jobs), all four
    classification paths (mixed-code, docs-only, infra-only, and
    the broken-classify failure mode) are validated.  Zero
    disagreements between `pr-fast.yml` and `ci.yml` observed.
    The dev-flow implementation plan §10.3 originally scheduled
    a 7-day parallel-window bake; the cutover was brought forward
    because the confidence budget was already exhausted by the
    same-day evidence above — continuing to run `ci.yml` on every
    PR would burn ~5-7 min of runner time per PR with no
    additional signal.  See plan §10.5 "Deviations from the plan
    v1" for the decision log.
  - **Follow-ups NOT in this commit**: (1) stale `ci.yml`-
    referencing comments in `pr-fast.yml`, `release.yml`,
    `dependabot-review.yml`, and `scripts/hooks/_lint_pre_push.sh`
    — tracked as a separate housekeeping PR so this cutover
    commit stays minimal and reviewable; (2) Phase 4b release.yml
    workflow-level permissions refactor (workflow-level
    `contents: write` → per-job grants on `create-github-release`
    only) — still deferred per the Phase 4b PR's scope note;
    (3) plan-doc §10.2 / §10.3 reconciliation (tick the Phase 4
    "cutover" checkbox, flip the dashboard status to ✅) — handled
    as a docs-only follow-up PR per the pattern PR #45 → PR #46.
  - **Rollback**: `git revert` this commit restores `ci.yml`
    verbatim (full history preserved; no squash-merge loss), AND
    the ruleset needs to be PUT back to the 7-entry shape.  The
    revert alone is not sufficient — the ruleset change is
    separate state.  See §4.3 of the plan doc for the exact
    reverse sequence.

### Security
- **CI / release supply-chain hardening batch** (2026-04-22 —
  `.github/workflows/*.yml`, `SECURITY.md`,
  `docs/architecture/security/supply-chain-posture.md`).  Closes
  the gaps + nits from the 2026-04-22 supply-chain review:
  - **Concurrency groups** on `ci.yml` and `release.yml`.  Tier 1
    now cancels superseded PR runs (but queues on `main` pushes so
    branch-protection required checks stay stable); `release.yml`
    queues instead of cancelling, so a half-signed release asset
    can never ship.
  - **`optimized-ci.yml` → `tier-2.yml`** rename for clarity; the
    filename now matches the workflow's advertised "Tier 2" identity.
  - **Tier 2 / Windows Compile Check** runs
    `cargo check --workspace --all-features --all-targets` natively
    on `windows-latest` weekly.  Previously Windows-only build
    regressions only surfaced 10-15 minutes into a `just ship`
    release build; the earlier Linux-hosted MSVC cross-check was
    removed because ubuntu has no MSVC linker.  Tier 2 summary +
    `notify-failure` are now wired to this job AND to the
    pre-existing `file-size-policy` job (which was dangling before
    this change, so a file-size-policy Tier 2 failure used to be
    silent).
  - **CycloneDX 1.5 SBOMs on every release** via `cargo-cyclonedx`,
    emitted as `sbom-<crate>.cdx.json` into `final-release/` BEFORE
    `CHECKSUMS.txt` is regenerated and BEFORE the SLSA attestation
    step, so the SBOMs are in the checksum manifest AND covered by
    the Sigstore OIDC attestation.  Verify with the same
    `gh attestation verify` flow that already exists for binaries.
  - **CodeQL (Rust SAST)** workflow
    (`.github/workflows/codeql.yml`) on every PR and weekly
    Tuesday 06:30 UTC baseline.  Pinned to
    `github/codeql-action` v4.35.2.  Uses `build-mode: none` (the
    only mode Rust currently supports) so the extractor parses
    source directly without a cargo build — run budget is ~5-10 min
    rather than the 15-25 min a compiled-extraction pipeline would
    need.  Rust is in CodeQL's public
    preview since CodeQL 2.22.1 (July 2025) — findings are
    informational until a clean baseline settles, so this is NOT
    yet a required branch-protection gate.
  - **Narrowly-scoped Dependabot auto-merge**
    (`.github/workflows/dependabot-auto-merge.yml`) — only
    `version-update:semver-patch` bumps with no active security
    advisory queue for auto-merge, and only once every required
    check is green (`cargo-deny`, `cargo vet check --locked`,
    clippy, tests, doc-tests, file-size policy).  Branch
    protection (signed commits, required reviews) is NOT
    bypassed — this just saves the "merge when green" clickwork.
    Minor / major / security-advisory bumps keep the existing
    manual-review flow.  Updates
    `docs/architecture/security/supply-chain-posture.md` +
    `SECURITY.md` to reflect the narrowed policy.
  - **Free-up-disk-space step on the clippy job** matching the
    other heavy Tier 1 jobs, so future dep-tree fan-out does not
    tip `--all-features` clippy past ubuntu-22.04's default disk
    budget.
  - **Per-workflow `notify-failure` labels**
    (`ci-failure-tier-1`, `ci-failure-tier-2`,
    `ci-failure-release`) so a release failure is never buried
    as a comment on a week-old Tier 2 flake issue.  Keeps the
    legacy `ci-failure` label as a secondary label for
    backwards-compatible issue queries.
  - **Updated threat-model + layered-defences tables** in
    `docs/architecture/security/supply-chain-posture.md` with
    rows for SBOM, SAST, Windows regression check, and the split
    between manual-review (minor/major) vs gated auto-merge
    (patch) on Dependabot PRs.

### Added
- **Brand identity pass** (chore, 2026-04-21) — publishing-grade brand
  and trademark layer:
  - `assets/brand/` with logos (ICO, ICNS, 7 hicolor PNG sizes),
    wordmark, hero mark, web assets (favicons, Apple / Android touch
    icons, Safari pinned-tab SVG, web manifest), and source SVGs —
    23 files, ~600 KB.
  - `LICENSES/LicenseRef-UFFS-Brand.txt` and a second `REUSE.toml`
    annotation block carving `assets/brand/**` out of the MPL-2.0
    default under `LicenseRef-UFFS-Brand`. Trademark and copyright
    stay cleanly separated and machine-readable for REUSE lint.
  - `TRADEMARK.md` at the repo root — canonical policy separating the
    UFFS name and logo from the MPL-2.0-licensed source, modeled on
    the Rust Foundation and CNCF trademark policies.
  - README hero banner, centered header + 5-badge row, new
    "License & Trademarks" section, and new "Maintainership &
    Commercial" section crediting [Sky, LLC](https://github.com/skyllc-ai)
    as the maintaining organization and outlining commercial UFFS
    frontends currently in development.
  - `CONTRIBUTING.md` gets a one-line contribution-agreement note
    covering MPL-2.0 and TRADEMARK.md, plus a Contact section so
    `TRADEMARK.md`'s "contact in CONTRIBUTING.md" pointer resolves.
  - **Windows binary icon + `app.manifest`** (Phase 2 —
    `crates/uffs-cli/build.rs`, `crates/uffs-cli/Cargo.toml`,
    `crates/uffs-cli/app.manifest`).  The existing MSVC `/DELAYLOAD`
    build-script block is now augmented with a `winresource` resource
    embed on the same MSVC gate: icon from
    `assets/brand/icons/uffs.ico`, plus `ProductName` /
    `FileDescription` / `CompanyName` / `LegalCopyright` /
    `OriginalFilename` version-info fields.  The manifest declares
    `asInvoker`, `PerMonitorV2` DPI awareness, and long-path support.
    Critical: the manifest stays `asInvoker` — elevation policy lives
    in `uffs_client::daemon_ctl::ElevationPolicy` (v0.5.36 refactor);
    a `requireAdministrator` manifest would pop UAC on every
    `uffs <pattern>` invocation and defeat that work.  `winresource`
    added as a build-dep (MSVC-only effect; compiles inertly on
    other targets).  New `cargo:rerun-if-changed=app.manifest` +
    `cargo:rerun-if-changed=../../assets/brand/icons/uffs.ico` so
    edits retrigger the resource embed.  Clippy lint gate satisfied
    with `#![allow(clippy::expect_used, reason = "…")]` scoped to the
    build script — runtime code stays panic-free.
  - **UFFS wordmark on user manual landing** (Phase 6 —
    `docs/user-manual/index.md`).  Centred `uffs-wordmark.png` at
    560 px above the H1 so the published docs carry the brand
    consistently with the README.
  - **macOS `.app` bundle layout** (Phase 3 —
    `packaging/macos/Info.plist.in`,
    `packaging/macos/bundle.sh`, `just/packaging.just`,
    `justfile`).  New `just dist-macos` recipe: builds the release
    binary and wraps it in `dist/UFFS.app` with the UFFS icns,
    `LSUIElement` CLI-mode plist, and `CFBundleIdentifier =
    com.skyllc.uffs`.  `Info.plist.in` carries a `@@VERSION@@`
    placeholder that `bundle.sh` sed-substitutes from
    `cargo pkgid -p uffs-cli`, so the bundle version can never drift
    from `Cargo.toml`.  Output goes to the gitignored `dist/` tree;
    packaging configuration lives under the tracked `packaging/`
    root (new top-level folder).  End-to-end verified on macOS:
    `dist/UFFS.app/Contents/MacOS/uffs --version` returns
    `uffs 0.5.71` with the plist version fields templated correctly.
  - **Linux `.desktop` + installer** (Phase 4 —
    `packaging/linux/uffs.desktop`,
    `packaging/linux/install.sh`, `just/packaging.just`).  New
    `just install-linux` recipe (wraps `sudo
    packaging/linux/install.sh`): builds the release binary,
    drops it at `$PREFIX/bin/uffs` (default `/usr/local`), installs
    the freedesktop entry under
    `$PREFIX/share/applications/uffs.desktop`, and lays out the
    full hicolor icon tree under
    `$PREFIX/share/icons/hicolor/{16..512}/apps/uffs.png`.  `install.sh`
    uses a portable `mkdir -p` + `install -m` helper (GNU `install
    -D` is a GNU-only extension; the helper also works with BSD
    `install` on macOS, which lets the script smoke-test from a mac
    dev box).  `gtk-update-icon-cache` and `update-desktop-database`
    run best-effort; absent tools fail silently.  End-to-end smoke-
    tested from macOS against `PREFIX=/tmp/uffs-linux-install-test`:
    9 files installed, binary runs, `.desktop` fields correct.
  - **`just/packaging.just`** — new module imported from the root
    `justfile` alongside the existing `build` / `bench_ci` /
    `analysis` modules.  Keeps packaging concerns isolated so
    `just --list` groups them and `build.just` stays focused on
    compilation rather than distribution.
  - **Release workflow bundles brand assets with every tag** (Phase 8
    — `.github/workflows/release.yml`).  New `Stage release bundle`
    step runs per matrix target after the existing binary build,
    staging binaries + `README` / `LICENSE` / `TRADEMARK.md` /
    `CHANGELOG.md` + platform-specific brand assets + packaging
    helpers into `release-staging/<artifact-name>/`, then zipping
    into `release-artifacts/<artifact-name>.zip` (7z on Windows,
    `zip` on macOS / Linux).  macOS additionally runs
    `packaging/macos/bundle.sh` so the ZIP ships a ready-to-run
    `UFFS.app` — end users don't need to run the bundler themselves.
    Linux ZIP embeds the full `assets/brand/icons/hicolor/` tree and
    `packaging/linux/install.sh` so `sudo
    packaging/linux/install.sh` works from the unzipped directory
    with no extra downloads.  `Organize release assets` step updated
    to copy per-platform ZIPs into `final-release/` as-is (the
    platform suffix is already baked into
    `matrix.artifact-name`); raw-binary platform-suffix loop kept
    intact so existing automation that `wget`s a single binary keeps
    working.  `CHECKSUMS.txt` covers every asset (ZIPs + raw
    binaries).  Release notes rewritten to front the ZIP bundles as
    the recommended path with raw-binary URLs documented as the
    automation alternative.

- **Regex alternation → ExtensionIndex fast path** (Phase 4, 2026-04-21 —
  `crates/uffs-core/src/search/dispatch.rs`,
  `crates/uffs-client/src/protocol/cli_args_helpers.rs`,
  `crates/uffs-client/src/protocol/cli_args.rs`).  New
  `extract_extensions_from_regex` helper recognises the narrow regex
  shape `>^?(?i)?.*?\.(e1|e2|...)$` and rewrites it to
  `pattern="*" + extensions=[e1, e2, ...]` so the query routes through
  the same `ExtensionIndex` CSR fast path that `--ext e1,e2,e3` uses.
  Requires a trailing `$` anchor so the rewrite is semantically
  lossless (without `$` the regex matches `.ext` anywhere in the
  name, which the ext-index cannot replicate).  Rejects multi-segment
  extensions, wildcards, character classes, and literal-prefixed
  regex (so path-anchored forms stay on the regex scan path).
  Added as dispatch-time safety net #3 in `apply_dispatch_safety_nets`
  and as parse-time sugar in `RawCliArgs::into_search_params`.
  **Projected**: `>.*\.(jpg|png|heic)$` on a 3.5 M-record C: drive
  drops from 298 ms → ~95 ms (matches the equivalent
  `--ext jpg,png,heic` glob path).  **28 new regression tests** pin
  the accepted / rejected shapes across both layers.

### Changed
- **`--sort path_only` parallelised on the ext-index fast path**
  (Phase 4, 2026-04-21 —
  `crates/uffs-core/src/search/query/path_only_top_n.rs`,
  `crates/uffs-core/src/search/sorting.rs`).  Three targeted changes
  in the daemon's path_only pipeline:
  - **`collect_path_only_via_ext_index`** — per-candidate path
    resolution rewritten from a single-threaded `for` loop to
    `par_chunks(4096)` with per-worker `DirCache`, mirroring the
    pattern already used in
    `numeric_top_n::collect_global_top_n_numeric`.  Includes an
    explicit `(drive_idx, rec_idx)` locality re-sort upfront so
    multi-extension queries (e.g.
    `>.*\.(jpg|png|heic)$`) preserve MFT-adjacent DirCache hits.
  - **`sort_rows_with_fold`** — the Schwartzian decorate pass
    (`String`-alloc-per-row for each needed folded key) now runs on
    `into_par_iter` with a per-worker `fold_buf`, and the resulting
    sort uses `par_sort_unstable_by` when `rows.len() >= 16_384`
    (same threshold as the numeric fast path).
  - **`PhaseTimings` instrumentation** — the path_only fast path
    now populates `scan_ms`, `sort_ms`, `path_resolve_ms`,
    `path_candidates`, and `path_cache_entries`, so `--profile` no
    longer reports `scan=0 sort=0 path_resolve=0` on
    `--sort path_only` queries.  `collect_path_only_sorted_top_n`
    now returns `(Vec<DisplayRow>, Option<PhaseTimings>)` — the
    tree-walk branch still returns `None` because its single
    traversal interleaves every phase.

  **Projected**: `*.dll --sort path_only` on a 167 K-row C: drive
  drops from 221 ms → ~60 ms daemon-side (closes the 172 ms gap
  vs the default Modified sort observed during the v0.5.62 validation run; full capture in
  [`docs/benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt`](docs/benchmarks/raw/2026-04-v0.5.62_aggregate-baseline.txt) and related internal logs).

### Fixed
- **`ext_rare` 543 ms outlier on drives with zero matching extensions**
  (Run 12, 2026-04-21 — `crates/uffs-core/src/search/query/numeric_top_n.rs`,
  `crates/uffs-core/src/search/filters/mod.rs`,
  `crates/uffs-core/src/search/filters/apply.rs`,
  `crates/uffs-core/src/search/sorting.rs`).  Two compounding bugs
  in the `*.<extension>` pipeline:
  - **Bug A (perf)** — `numeric_top_n::search_index` fell through to
    a full-drive scan when `resolve_ext_ids_for_drive` produced an
    empty ID set.  On a 3.5 M-record drive with zero `.dbt` files,
    `C:*.dbt --hide-system --hide-ads` cost 543 ms of pure scan
    plus a spurious row from Bug B.  **Fixed** by adding an explicit
    short-circuit arm that skips the drive entirely when the
    resolved-ID set is empty.
  - **Bug B (correctness)** — the `matches_record` /
    `row_passes_filters` fallback extracted extensions via
    `name.rsplit('.').next().unwrap_or("")`, which returns the
    whole name for dotless inputs.  A directory literally named
    `dbt` therefore matched `--ext dbt` even though the MFT
    indexer's `intern_extension` had already assigned it
    `extension_id = 0` (no extension bucket).  **Fixed** by adding
    a shared `extract_extension_after_dot` helper that matches
    `intern_extension` semantics exactly (dotless, dotfile, and
    trailing-dot names all return `""`), and replacing the buggy
    extraction in `matches_record`, `row_passes_filters`, and the
    `search::sorting` sort-key builder.  The sort-key fix closes
    a latent data-leak where dotless names leaked between
    extension groups on extension-sorted result sets.
  - **11 new regression tests** pin the fixes:
    `search::filters::tests::extract_extension_after_dot_*` (5 —
    helper semantics), `filter_extension_fallback_*` (4 —
    end-to-end `matches_record` fallback), and
    `search::backend::tests::search_index_ext_rare_*` (2 —
    end-to-end `*.dbt` on a drive with zero `.dbt` files).
- **`--profile` per-drive match counts rewritten O(rows×drives) →
  O(rows)** (`crates/uffs-daemon/src/index/search.rs:282-310`).
  The previous implementation nested `filter(|row| row.drive == D)
  .count()` inside a per-drive loop, producing quadratic work in
  the result cross product.  Single-pass `HashMap<char, usize>` tally
  then projects back over `drive_info` to preserve the existing
  (drive, count) ordering contract.  Cuts `--profile` overhead on
  wide result sets (e.g. 100 K rows × 4 drives) from ~400 K
  predicate evaluations to ~100 K hash inserts.

### Changed
- **`scripts/windows/cross-tool-benchmark.rs` no longer hard-codes
  `--profile`** in the default UFFS invocation (Run 12, 2026-04-21).
  The bench now measures the exact command shape a normal user
  types; `daemon_ms` is still captured on an opt-in basis via
  `UFFS_EXTRA_ARGS="--profile"` (environment variable).  Previous
  runs paid <0.2% overhead from `--profile`, so summary numbers
  remain comparable — change is primarily methodological
  cleanliness for public-facing benchmarks.

### Added
- **Phase 3 — `--columns parity` / `--parity-compat` and `--format custom`
  now take the daemon pre-format fast path**
  (`crates/uffs-daemon/src/handler.rs::RequestHandler::try_pack_csv_blob`).
  Both exclusions from Phase 2 are lifted; the daemon now produces the
  full 25-column legacy parity layout and the `Drives? … / MMMmmm …`
  drive footer server-side, leaving the CLI a pure `write_all` on
  the received blob.  Specifically:
  - `--columns parity` and `--parity-compat` both route through
    `uffs_format::write_rows` with `parity_compat=true` — the new
    behaviour that `build_output_config` auto-promotes `columns ==
    "parity"` into `parity_compat = true` keeps the CLI's
    `write_parity` (always rewrites dir rows) and the daemon's
    `write_rows` (rewrites only when flag is set) emitting
    byte-identical output even for `--columns parity` queries that
    omit `--parity-compat`.
  - `--format custom` accepts the CSV body through the shared
    writer, then appends the legacy footer via
    `uffs_format::write_legacy_drive_footer`.  The drive letters
    come from the new `SearchParams::output_drive_targets` wire
    field; empty targets skip the footer entirely, matching the
    CLI's baseline behaviour.
  - Parity always emits the 25-column header even when
    `output_header=false`: the daemon explicitly overrides
    `cfg.header=true` when `parity_compat` is active so the CLI's
    hand-rolled `write_parity` (which ignores the header flag) and
    the daemon fast path stay byte-identical on
    `--parity-compat --noheader` queries.
- **`uffs-format::footer` module — canonical legacy drive footer writer**
  (`crates/uffs-format/src/footer.rs`).  Carves the
  `write_legacy_drive_footer` + `DriveFooterContext` +
  `is_full_scan_pattern` helpers out of the CLI-private `parity.rs`
  into the shared crate so the CLI slow path
  (`write_native_results("custom", …)`) and the daemon fast path
  (`try_pack_csv_blob` with `output_format == "custom"`) share a
  single implementation.  Includes a self-test suite
  (`uffs_format::footer::tests::*`) that pins the CRLF shape, the
  `"MMMmmm that was FAST"` heuristic, the row-count threshold
  (`FAST_SCAN_ROW_LIMIT = 20 000`), the pipe-joined drive-letter
  formatting, and the full-scan pattern classifier.  Re-exported
  from `uffs-client::output` so the CLI preserves its thin-client
  invariant of depending only on `uffs-client`.
- **`SearchParams::output_drive_targets` wire field**
  (`crates/uffs-client/src/protocol/mod.rs`).  Carries the CLI's
  local `targets: Vec<char>` computation (from `--drive`,
  `--drives`, and the thin-client passthrough `--mft-file` path) to
  the daemon so `try_pack_csv_blob` can reproduce the footer
  exactly.  Intentionally separate from `SearchParams::drives`
  because "drives to search" and "drives to show in footer" are
  semantically distinct — e.g. `--mft-file D.mft` targets D for the
  footer but leaves `drives` empty.  Absent / empty → footer
  omitted (matches `uffs_format::write_legacy_drive_footer`'s
  empty-targets short-circuit).
- **CLI `write_columnar` now emits canonical byte-parity output**
  (`crates/uffs-cli/src/commands/output/mod.rs`).  The slow path
  that runs when the daemon returns `InlineRows` has been aligned
  with `uffs_format::write_rows` in three places so the CLI
  fallback and the daemon fast path cannot drift:
  - **Quote policy:** only string-shaped columns (`Path` / `Name` /
    `PathOnly` / `Type` / `Extension`) get quote-wrapped; numeric,
    datetime, and boolean-flag columns emit raw.  Matches the match
    arms in `uffs_format::writer::write_row` — the new helper
    `is_quoted_column` is the single authority both sites check.
  - **Timezone:** `extract_field` now takes a `tz_offset_secs`
    parameter fed from the parity context, and
    `format_filetime_with_tz` mirrors
    `uffs_format::append_datetime_native` exactly.  The older
    `format_filetime_local` is retained for the `--format table`
    human-display path (intentionally host-local for that surface).
  - **Header terminator:** the header row is now closed with
    `\n\n` (header + blank separator line) instead of a single
    `\n`, matching `uffs_format::write_rows` and the legacy
    baseline that `uffs-core::output::tests::format_parity_*`
    already pin.
- **Datetime zero-sentinel alignment across CLI and daemon**
  (`crates/uffs-cli/src/commands/output/{mod.rs,parity.rs}`).  Both
  `format_filetime_local` and `append_datetime_tz` now emit
  `"0000-00-00 00:00:00"` on an unset FILETIME (zero ticks, for
  which `uffs_time::filetime_to_calendar` returns `None`).  The
  previous empty-string behaviour diverged from
  `uffs_format::append_datetime_native` and silently produced
  different bytes between the CLI slow path and the daemon fast
  path on rows with zero Created/Modified/Accessed values — a
  latent Phase 2 inconsistency.
- **Six new byte-parity regression tests across CLI writers**
  (`crates/uffs-cli/src/commands/output/output_tests.rs`).  Pin
  every axis the Phase 3 lift depends on:
  - `parity_byte_parity_basic_file_zero_filetime` — datetime
    sentinel agreement.
  - `parity_byte_parity_directory_rewrite` — Path / Name /
    `PathOnly` / Size / `SizeOnDisk` parity-dir rewrite for
    directory rows.
  - `parity_byte_parity_all_flag_bits` — 15-column flag dispatch
    and `ParityAttributes` final column agree for every
    `PARITY_MASK` bit.
  - `parity_byte_parity_multi_row` — row ordering and header /
    blank-separator structure.
  - `columnar_byte_parity_zero_filetime_date_columns` +
    `columnar_byte_parity_nonzero_filetime` — pins the
    `write_columnar` ↔ `uffs_format::write_rows` alignment
    (quote policy, TZ, `\n\n` header) end-to-end.
- **Six new daemon regression tests for the Phase 3 gate lift**
  (`crates/uffs-daemon/src/handler.rs::tests`).  Replace the
  Phase 2 `skips_columns_parity` / `skips_parity_compat_flag`
  tests (which pinned the old exclusions) with positive-assertion
  coverage of the new behaviour:
  - `accepts_columns_parity` — `--columns parity` lands on
    `InlineBlob`, header matches the canonical 25-column legacy
    layout + `\n\n`, and the sample directory row gets the
    parity-dir rewrite (`\"C:\\\\Program Files\\\\app\\\\\",\"\",`).
  - `accepts_parity_compat_flag` — `--parity-compat` on a
    non-parity projection still rewrites dir rows (Path gets
    trailing `\`, Size swapped to `treesize`).
  - `parity_forces_header_when_disabled` — parity overrides
    `output_header=false` so the fast/slow paths agree on
    `--parity-compat --noheader`.
  - `custom_appends_footer_when_drives_set` — `--format custom`
    with non-empty `output_drive_targets` produces a blob whose
    tail contains the CRLF `Drives? \t1\tC:\r\n` footer and the
    `MMMmmm that was FAST` warning for a full-scan pattern under
    the row threshold.
  - `custom_omits_footer_when_no_drives` — empty
    `output_drive_targets` skips the footer entirely (matches the
    CLI's baseline).
  - `skips_non_csv_format` (updated) — `"json"` / `"table"` /
    `"CSV "` (trailing-space garbage) still skip; the old
    `"custom"` entry is removed because it is now accepted.
- **Daemon-side multi-column CSV pre-format fast path**
  (`crates/uffs-daemon/src/handler.rs::RequestHandler::try_pack_csv_blob`).
  Extends the existing path-only blob fast path (`try_pack_paths_blob`)
  to every multi-column CSV projection the daemon's formatter can
  reproduce byte-for-byte.  When the gate accepts the request, the
  handler consumes the inline `Vec<SearchRow>`, feeds it through
  `uffs_format::write_rows` with the same `OutputConfig` the
  `--out=file` path uses (via the newly `pub(crate)`
  `uffs_daemon::index::search::build_output_config`), and replaces
  `SearchResponse::payload` with `SearchPayload::InlineBlob` for
  payloads ≤ 512 KB or `SearchPayload::ShmemBlob` above that
  threshold.  The CLI then writes the buffer verbatim with a single
  `write_all`, skipping per-row JSON deserialisation, the
  client-side `extract_field` dispatch, and the `write_columnar`
  per-column render loop on the medium-to-large result sets where
  that dispatch dominates end-to-end latency.
- **New `SearchParams::output_format` wire field**
  (`crates/uffs-client/src/protocol/mod.rs`).  Carries the CLI's
  `--format` value (`"csv"`, `"json"`, `"custom"`, `"table"`) to
  the daemon so `try_pack_csv_blob` can gate correctly — the
  pre-format path only runs when the CLI will actually consume CSV
  output, and defers to the local formatter for JSON / table /
  `custom` (which appends a legacy drive footer the daemon does not
  emit).  Filled from `CliArgs::format` in `from_cli_args` and
  handled everywhere else by serde defaults — the field is optional
  and absent means "CLI default (csv)".
- **Nine new regression tests for `try_pack_csv_blob`**
  (`crates/uffs-daemon/src/handler.rs::tests::try_pack_csv_blob_*`).
  Mirror the path-only test layout:
  - **`happy_path_multi_column`** — pins the default CSV projection
    case (`output_format: None`, multi-column projection) lands on
    `InlineBlob` with the expected header + separator + row
    structure.
  - **`accepts_explicit_csv_format`** — `output_format =
    Some("csv")` in every case combination (lowercase, uppercase,
    mixed) is accepted.
  - **`skips_json_response_mode`**, **`skips_non_csv_format`**,
    **`skips_aggregations`**, **`skips_when_output_file_set`**,
    **`skips_columns_parity`**, **`skips_parity_compat_flag`**,
    **`skips_empty_response`** — each gate bullet in the method
    docstring has a dedicated test that keeps the payload as
    `InlineRows` instead of pre-formatting.
  - **`offloads_large_blob_to_shmem`** — 5 000-row fixture with
    padded paths produces a >512 KB blob, verifies the handler
    lands on `ShmemBlob`, streams the file back via
    `stream_paths_blob_into`, and compares the streamed bytes
    against a fresh in-memory `uffs_format::write_rows` reference
    call.  The file is deleted after the stream, mirroring the
    `try_pack_paths_blob` shmem test's lifecycle check.
- **`uffs-format` crate — unified CSV/columnar output formatter**
  (`crates/uffs-format/`).  Carves the shared CSV writer that both the
  daemon's `--out=file` path (`DisplayRow`) and the thin CLI's stdout
  path (`SearchRow`) now delegate to, so the two sites are byte-identical
  by construction rather than by accident.  The crate is polars-free,
  tokio-free, and depends only on `uffs-time` + `uffs-mft` + `itoa` +
  `rayon` + `serde` + the narrow `chrono` `clock` feature, preserving
  the thin-client binary-size invariant.  The public surface is
  `FormatRow` (trait abstracting over `DisplayRow` / `SearchRow`),
  `OutputConfig` (builder), `OutputColumn` (narrow enum mirroring the
  subset of `FieldId` the formatter needs), and `write_rows` (the
  entry point).  `uffs-client::output::write_search_rows` is a thin
  re-export used by CLI consumers that already depend on `uffs-client`.
- **Byte-parity regression tests for the formatter unification**
  (`crates/uffs-core/src/output/tests.rs::format_parity_*`).  Four
  tests — basic file row, parity-compat directory row, `--columns all`
  baseline, and 20 000-row parallel branch — pin that
  `uffs_format::write_rows(&[DisplayRow], …)` emits byte-identical
  output to the legacy `OutputConfig::write_display_rows(&[DisplayRow], …)`.
  Any future drift in either implementation trips at least one test
  before it reaches end-to-end parity suites.
- **`FieldId` ↔ `OutputColumn` drift-guard tests**
  (`crates/uffs-core/src/search/field/field_tests.rs::field_id_matches_output_column_*`).
  Three tests pin that every `FieldId` variant has a matching
  `uffs_format::OutputColumn` variant with identical `canonical_name`
  and `display_name`.  The `field_id_to_format_column` bridge in
  `uffs_core::output::display_rows_format_bridge` is an exhaustive
  `const fn` match, so variant-set drift trips at compile time; these
  tests cover the remaining metadata-drift surface at run time.
- **Phase 3 output-path optimization** (`docs/research/perf-phase3-output-optimization.md`)
  - **3.1 NUL fast path** — CLI detects `> NUL` / `> /dev/null` via the new
    `uffs_client::stdout_kind` module (Unix `fstat` + `/dev/null` device-id
    match; Windows `GetFileType` + `GetConsoleMode`) and auto-injects
    `--no-output`.  The daemon gates `SearchRow` materialisation on
    `include_rows`, so `paths_blob` packing, shmem offload, and IPC row
    transfer all no-op on suppressed queries.  Expected saving: 20–30 ms
    on medium result sets piped to NUL.
  - **3.2 Single-buffer multi-column console render** — the console branch
    of `write_native_results` now renders CSV / JSON / table / parity output
    into a `Vec<u8>` and issues one `stdout.lock().write_all`, replacing the
    previous `BufWriter<StdoutLock>` + per-row `writeln!` pattern.  Guarded
    by a 50 MiB cap via the pure `choose_console_strategy(row_count, cap,
    est)` helper — falls back to streaming on pathological result sets.
  - **3.3 Windows `WriteConsoleW` direct path** — when stdout is a real
    console on Windows, `uffs_client::stdout_kind::write_stdout_buffer`
    transcodes the rendered buffer to UTF-16 once and issues chunked
    `WriteConsoleW` calls, bypassing the narrow-CRT codepage translation
    that otherwise mangles non-ASCII output on legacy conhost.
- **Async `UffsClient` wire-protocol test coverage**
  (`crates/uffs-client/src/connect_tests.rs`) — six behavioural regression
  pins mirroring the sync suite (`status`-method contract,
  `ConnectionFailed` remediation text, `cached_status` short-circuit in
  both directions).  Drives the client through in-memory tokio
  `AsyncRead`/`AsyncWrite` doubles — no real socket, no daemon.

### Changed
- **Bulkiness sort-key eliminates per-candidate `DisplayRow` allocation**
  (`crates/uffs-core/src/search/query/numeric_top_n.rs`).  Added
  `bulkiness_for_record(&CompactRecord)` as a sibling of `bulkiness_for_row`;
  both forward to a shared private `bulkiness_from_sizes` so they cannot
  drift.  On the numeric top-N hot path this shaves an 18-line
  `DisplayRow::new(..., String::new(), ...)` dance — ~μs per candidate —
  measured impact ≈ 45 ms on a 45K-row `--sort bulkiness *.dll` query.

### Fixed
- **`shmem::tests` race** — `concurrent_writes_get_unique_paths` and
  `gc_cleans_orphaned_bins_and_preserves_non_bins` shared the global
  `shmem_dir()`; the GC test's `cleanup_stale_shmem_files()` sweep could
  wipe in-flight files written by the concurrent-writes test when cargo's
  threadpool scheduled both in parallel.  Serialised via a file-local
  `Mutex<()>`.  Production never hit this — GC only runs at daemon
  startup, and the PID file prevents overlap in real usage.
- **Two miswritten `#[expect(clippy::cognitive_complexity)]` reason strings**
  in `crates/uffs-daemon/src/index/mod.rs` had been copy-pasted from
  unrelated functions (`load_single_mft_file` tagged as "multi-drive
  search"; `ensure_drives_loaded` as "tree metrics computation").
  Replaced with accurate per-function justifications.

## [0.5.71] - 2026-04-19

### Added
- **Phase 2 performance measurement series** (closed): 11 instrumented
  runs comparing UFFS to Everything / UltraSearch / ES across cold-warm-hot
  phases.  Shipped `docs/research/perf-phase2-measurement-plan.md` as
  the permanent record.
- **`paths_blob` single-buffer fast path (v0.5.35)** — daemon packs
  path-only projections into a newline-terminated UTF-8 buffer; CLI
  writes with one `write_all`, skipping per-row JSON deserialisation.
  Inline for ≤ `SHMEM_THRESHOLD` rows; large results fall back to the
  shmem transport.
- **UAC refactor (v0.5.36)** — `ElevationPolicy::RequireExistingElevation`
  default, `--elevate` opt-in, `UFFS_ELEVATE=1` session override, plus an
  actionable error surface listing all three recovery paths (elevated
  shell, explicit UAC, broker install).
- **Deep health check (Run 10 Part B)** — `UffsClientSync` /
  `UffsClient` consolidate the connect-time liveness probe and
  pre-search readiness poll into a single `status` RPC, with a
  `cached_status` short-circuit in `await_ready`.  ~5–10 ms saved per
  CLI invocation on Windows named pipes.
- **Shared-memory transport for bulk results** (`uffs-client::shmem`) —
  results beyond `SHMEM_THRESHOLD` bypass JSON and memory-map a temp
  file.  Includes format v2 binary header, best-effort GC of stale
  `.bin` files on daemon startup.
- **Cross-tool benchmark harness**
  (`scripts/windows/cross-tool-benchmark.rs`) — drives UFFS, Everything,
  UltraSearch, and the legacy `uffs.com` C++ build through an
  apples-to-apples workload with cold/warm/hot phases and per-drive
  isolation.

### Changed
- **`cli_args.rs` refactored** — 11 stateless parsers extracted to
  `cli_args_helpers.rs`; `search.rs` tests moved to `search_tests.rs`
  via `#[path]` module re-attach.  Keeps both files under the 800-LOC
  file-size policy with no suppression.
- **Startup profiling** — `UFFS_PROFILE_STARTUP=1` prints per-phase
  wall-clock from `main()` through first `write_all`, driving the
  Phase 2 + Phase 3 measurement work.

### Fixed
- **`*.<ext>` and `<letter>:*` CLI sugar** — parse-time promotion to
  `pattern="*" + ext=<ext>` (and drive-prefix extraction) was briefly
  regressed during the fat→thin CLI split; restored plus a dispatch-time
  safety net in `uffs_core::search::backend::search_index` for direct
  JSON-RPC callers.
- **PathOnly sort** — now matches Windows Folder-column semantics
  (directories compare before files at equal path prefix; case-folded
  via the drive's upcase table).
- **Lifecycle / PID file** — stale-PID detection, `--no-retire` flag
  for long-running CI sessions, and session-tier upgrades (TUI / GUI /
  MCP at tier 1 get 3× the idle timeout of CLI at tier 0).

## [0.5.0] - 2026-03-15

Major architectural milestone — daemon-first CLI, MCP adapter, and
aggregate engine all ship together.

### Added
- **Aggregate engine** (`uffs_core::aggregate`) — Stages 0-5 complete:
  scaffolding + `AggregateMeta`; protocol + daemon + CLI integration;
  rollup, duplicates, parser, presets; pagination + CSV/TSV export;
  cache; `--agg` flag surface; MCP aggregation tools; 10-test
  validation suite (T119–T128).
- **MCP (Model Context Protocol) gateway** (`uffs-mcp`) — stdio adapter
  that bridges Claude, Cursor, Windsurf, and other AI agents to the
  daemon via JSON-RPC.  D3.4.5 notifications, D4.3 E2E tests, MCP
  resources + prompts.
- **Security hardening** — S1 cache DACL / file permissions, S2.2.2
  Windows DPAPI keystore, S4 daemon IPC hardening (peer credentials,
  input validation, limit caps), S4.3 client-side daemon identity
  verification (macOS codesign / Windows Authenticode), S4.4 rate
  limiting + idle timeout + shutdown nonce, S5 Access Broker hardening.
- **`uffs-broker`** — optional Windows service providing elevated MFT
  handles so the daemon itself can run `asInvoker` with no UAC prompt.
- **Scenario M** — incremental MFT hot-load validation scenario; exercises
  the daemon's `load_drive` + `info` + `refresh` paths against live
  drives.

### Changed
- **`--parity-compat` mode** — `CPP_COLUMN_ORDER` for exact C++-binary
  output shape, `parity_attributes()` mask for the 15 baseline NTFS
  flag bits.  Lets the Rust daemon drop into legacy automation with
  zero ini changes.

## [0.4.0] - 2026-02-12

Daemon-first architecture lands — CLI / TUI / GUI / MCP are now all
thin clients over a unified `uffsd` process.

### Added
- **Daemon foundation (D2)** — `IndexManager` holds the compact index +
  trigrams; `IpcServer` over Unix domain socket (macOS/Linux) or named
  pipe (Windows); RPC handler; lifecycle manager with idle auto-retire.
- **Client library (D3)** — `UffsClient` (async, tokio) and
  `UffsClientSync` (blocking, tokio-free) with auto-start, keepalive,
  reconnect, structured error types.
- **MCP adapter scaffolding (D4)** — stdio bridge, initial tool
  definitions, handler dispatch.
- **Windows Access Broker scaffold (D7)** — `uffs-broker` service,
  client, shared handle passing via Win32 named pipes; unblocks the
  "no UAC prompt for search" target posture.
- **Thin-client CLI / TUI / GUI** — `uffs`, `uffs_tui`, `uffs_gui` now
  delegate all heavy lifting to the daemon.  TUI drops from ~7 GiB
  peak RSS to < 50 MB.

## [0.3.0] - 2026-02-01

### Added
- **Compact index** — 72 bytes/record `CompactRecord` (`repr(C)`,
  `bytemuck::Pod/Zeroable`) replaces the full `MftIndex` after cache
  build.  ~72% memory reduction (7.5 GB → 2.1 GB for 25.9M records
  across 7 drives).
- **TUI** (`uffs_tui`) with ratatui — search box, paginated table,
  multi-tier sort (seven columns), file/dir/all filter, drive colour
  palette.  Wave 1 (trigram index, textarea, devicons) and Wave 2
  (table, sort, filter) complete.
- **Tree-based path search** — children index + segment decomposition
  for `C:\foo\bar`-style queries; glob matching with `*`, `?`, `**`.
- **On-demand full record lookup** — 25-column max view via seek+read
  from the `.uffs` cache, no need to keep full records in memory.
- **`.uffs` cache on macOS** — mirrors the Windows cache flow so MFT
  files captured on Windows can be searched on macOS.
- **Persistent search history** (`Ctrl+P` / `Ctrl+N`) — platform
  config dir, deduplicated, survives restarts.
- **Keymap system** — `~/.config/uffs/keys.toml`, embedded
  `PRESET_WINDOWS` and `PRESET_EMACS`, `--keys emacs` CLI override.

### Fixed
- **NTFS flags refactor** — `StandardInfo.flags` now stores raw
  `FILE_ATTRIBUTE_*` bits matching Windows semantics (`IS_READONLY=0x0001`,
  `IS_HIDDEN=0x0002`, etc.) instead of an internal remapping.  Cache
  format v9 (v8 auto-converts via `v8_flags_to_raw_ntfs()`).  Unblocks
  downstream parity work.

## [0.2.208] - 2026-01-27

### Added
- Baseline CI validation for modernization effort
- Windows cross-compilation for all binaries (uffs, uffs-mft, uffs_tui, uffs_gui)
- Modernization tracker and wave guides

### Changed
- Updated Polars to commit 8b99db82

## [0.2.114] - 2026-01-26

### Added
- Initial UFFS Rust implementation
- MFT reading and parsing with Polars DataFrames
- Path resolution during MFT digestion
- Hard link expansion (default on)
- Multi-drive parallel indexing support
- Cache architecture with zstd compression

### Fixed
- Various MFT parsing edge cases

[Unreleased]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.46...HEAD
[0.6.46]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.45...v0.6.46
[0.6.45]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.44...v0.6.45
[0.6.44]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.43...v0.6.44
[0.6.43]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.42...v0.6.43
[0.6.42]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.41...v0.6.42
[0.6.41]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.40...v0.6.41
[0.6.40]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.38...v0.6.40
[0.6.38]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.37...v0.6.38
[0.6.37]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.36...v0.6.37
[0.6.36]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.35...v0.6.36
[0.6.35]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.33...v0.6.35
[0.6.33]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.32...v0.6.33
[0.6.32]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.31...v0.6.32
[0.6.31]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.30...v0.6.31
[0.6.30]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.29...v0.6.30
[0.6.29]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.28...v0.6.29
[0.6.28]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.27...v0.6.28
[0.6.27]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.26...v0.6.27
[0.6.26]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.25...v0.6.26
[0.6.25]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.24...v0.6.25
[0.6.24]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.23...v0.6.24
[0.6.23]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.22...v0.6.23
[0.6.18]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.17...v0.6.18
[0.6.17]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.15...v0.6.17
[0.6.15]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.14...v0.6.15
[0.6.14]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.13...v0.6.14
[0.6.13]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.12...v0.6.13
[0.6.12]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.11...v0.6.12
[0.6.11]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.10...v0.6.11
[0.6.10]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.9...v0.6.10
[0.6.9]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.8...v0.6.9
[0.6.8]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.7...v0.6.8
[0.6.7]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.6...v0.6.7
[0.6.6]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.5...v0.6.6
[0.6.5]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.4...v0.6.5
[0.6.4]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.3...v0.6.4
[0.6.3]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.123...v0.6.1
[0.5.123]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.122...v0.5.123
[0.5.122]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.121...v0.5.122
[0.5.121]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.120...v0.5.121
[0.5.119]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.118...v0.5.119
[0.5.118]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.117...v0.5.118
[0.5.117]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.116...v0.5.117
[0.5.116]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.115...v0.5.116
[0.5.115]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.114...v0.5.115
[0.5.114]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.113...v0.5.114
[0.5.111]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.110...v0.5.111
[0.5.110]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.109...v0.5.110
[0.5.109]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.108...v0.5.109
[0.5.108]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.107...v0.5.108
[0.5.107]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.106...v0.5.107
[0.5.106]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.105...v0.5.106
[0.5.103]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.102...v0.5.103
[0.5.102]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.100...v0.5.102
[0.5.100]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.99...v0.5.100
[0.5.99]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.96...v0.5.99
[0.5.96]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.95...v0.5.96
[0.5.95]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.94...v0.5.95
[0.5.94]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.92...v0.5.94
[0.5.92]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.90...v0.5.92
[0.5.90]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.89...v0.5.90
[0.5.86]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.85...v0.5.86
[0.5.85]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.83...v0.5.85
[0.5.83]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.82...v0.5.83
[0.5.82]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.81...v0.5.82
[0.5.81]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.80...v0.5.81
[0.5.80]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.79...v0.5.80
[0.5.79]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.78...v0.5.79
[0.5.78]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.77...v0.5.78
[0.5.77]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.76...v0.5.77
[0.5.76]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.75...v0.5.76
[0.5.75]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.74...v0.5.75
[0.5.74]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.73...v0.5.74
[0.5.73]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.72...v0.5.73
[0.5.71]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.5.0...v0.5.71
[0.5.0]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.2.208...v0.3.0
[0.2.208]: https://github.com/skyllc-ai/UltraFastFileSearch/compare/v0.2.114...v0.2.208
[0.2.114]: https://github.com/skyllc-ai/UltraFastFileSearch/releases/tag/v0.2.114
