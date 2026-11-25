// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Static `--help` text and `print_*_help` functions for every `uffs`
//! subcommand. Split out of `args.rs` (which owns argument *parsing*)
//! to keep that file under the workspace's 800-LOC-per-file policy —
//! this content is pure help strings with no parsing logic of its own.

/// Short help text.
const HELP: &str = "\
uffs - Ultra Fast File Search

USAGE:  uffs <PATTERN> [OPTIONS]
        uffs --<COMMAND> [ACTION] [OPTIONS]

Search-first: any first token that is NOT a `--command` is a search pattern,
so `uffs --update`, `uffs --status`, etc. search for those words. Management is
`--<command>` (below). To search a pattern that begins with `--`, use
`uffs -- <PATTERN>`.

EXAMPLES:
  uffs '*.txt'                        Find all .txt files
  uffs '>.*\\.log$' --drive C          Regex search on C:
  uffs '*' --mft-file C.bin            Offline MFT search
  uffs --ext rs,toml                   Find Rust project files
  uffs --type picture --min-size 10MB  Large images
  uffs --update doctor                 Self-update health check

COMMANDS:
  --search <PATTERN>   Explicit search (same as the bare default)
  --stats              Show filesystem statistics (the overview preset)
  --agg <PRESET>       Run aggregate analytics
  --deleted            Forensic tombstone read: recently-deleted files from an MFT
  --snapshot           Capture the live MFT to a baseline file (Windows, for --diff)
  --daemon <ACTION>    Manage the UFFS daemon (start/stop/load/status)
  --mcp <ACTION>       Manage the UFFS MCP server
  --update [ACTION]    Self-update (snapshot/acquire/apply/doctor/recover)
  --status             Show combined system status

COMMON OPTIONS:
  -v, --verbose           Verbose output (with --benchmark: also print the rows)
  -d, --drive <LETTER>    Drive letter (e.g. C or C:)
  --drives <A,B,...>      Multiple drive letters
  --mft-file <PATH>       Raw MFT file(s), comma-separated
  --data-dir <PATH>       Data directory with drive_* subdirs
  --files-only            Show only files
  --dirs-only             Show only directories
  --ext <EXT>             Filter by extension(s)
  --type <CATEGORY>       Filter by type: code, picture, video, etc.
  -n, --limit <N>         Max results (0 = unlimited, default: 0)
  -f, --format <FMT>      Output: table (default in a terminal), csv (default
                          when piped/redirected or with --out), json
  --sort <COL>            Sort by column, prefix - for desc
  --out <FILE>            Write to file instead of console
  --columns <COLS>        Columns to output (default: all)
  --newer <SPEC>          Modified after date/duration
  --older <SPEC>          Modified before date/duration
  --diff <BASELINE>       Search the DELETED set vs a baseline MFT capture
                          (combine with any filter: --diff C_old.bin --drive C
                          '*.txt' --newer 30d). Needs the drive loaded.
  --min-size <SIZE>       Minimum file size (e.g. 100KB, 10MB)
  --max-size <SIZE>       Maximum file size
  --profile               Show timing breakdown
  --benchmark             Time the whole pipeline incl. output formatting;
                          the rows go to a sink, not the screen (add -v to see
                          them)
  --no-output             Match only: the daemon builds no rows, so this times
                          'how many files match' and nothing else (auto-set
                          when stdout is NUL)
  --help                  Print this help
  --version               Print version
";

/// Print help and exit.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_help() {
    print!("{HELP}");
}

/// Print version and exit. The short line ties a running binary to the exact
/// source (`<name>[.exe] <semver> (<sha>)`, `-dirty` for an uncommitted tree);
/// `verbose` adds the multi-line build fingerprint (commit date, rustc, target,
/// profile) for bug reports. Shared with every UFFS binary via `uffs-version`.
#[expect(clippy::print_stdout, reason = "intentional version output")]
pub(crate) fn print_version(verbose: bool) {
    if verbose {
        println!("{}", uffs_version::version_long!("uffs"));
    } else {
        println!("{}", uffs_version::version_short!("uffs"));
    }
}

// ── Subcommand help texts ─────────────────────────────────────────────

/// Help text for `uffs --daemon`.
const DAEMON_HELP: &str = "\
uffs --daemon — Manage the UFFS background daemon

USAGE:  uffs --daemon <ACTION> [OPTIONS]

ACTIONS:
  start              Start the daemon (any other flag is refused)
    --data-dir PATH    Data directory with drive_* subdirs
    --mft-file PATH    Raw MFT file(s), comma-separated; repeatable
    --drive LETTER     Live drive(s) to load (Windows); repeatable
    --no-cache         Skip cached index, re-parse MFT
    --elevate          Request a UAC prompt (Windows) if not elevated
                       [env: UFFS_ELEVATE=1]
  status             Show daemon status (running, drives, PID)
    -v, --verbose      Long view: build, broker mode, memory, paths, perf stats
    --json             Machine-readable status + drives + stats
  stop               Gracefully stop the daemon
  kill               Hard kill + remove PID/socket files
  restart            Stop then start with the same launch flags
  load               Hot-load additional MFT file(s) into running daemon
    --mft-file PATH    Raw MFT file(s) to load
    --data-dir PATH    Data directory with drive_* subdirs
    --drive LETTER     Drive letter(s) to load from data-dir
    --no-cache         Skip cache when loading
  hibernate          Demote shards to Cold (free RAM, encrypted cache stays)
    [DRIVE...]         Drive letter(s); omit to hibernate all loaded drives
    --drives A,B       Drive letter(s) as comma-separated list
  preload            Promote shard(s) to Hot and pin the tier
    [DRIVE...]         Drive letter(s); at least one required
    --drives A,B       Drive letter(s) as comma-separated list
    --pin-minutes N    Pin window in minutes (default: 30)
  forget             Evict drive(s) from registry and delete on-disk caches
    [DRIVE...]         Drive letter(s); at least one required
    --drives A,B       Drive letter(s) as comma-separated list
    --force            Auto-hibernate non-Cold drives first (default: refuse)
  resident           Keep UFFS permanently resident (login autostart, no idle retire)
    on                 Install the per-user login item (uffsd --no-retire) + start now
      --data-dir PATH    Data directory baked into the login item (non-Windows)
      --mft-file PATH    Raw MFT file(s) baked into the login item (non-Windows)
      --drive LETTER     Drive letter(s) baked into the login item (Windows)
    off                Remove the login item
    status             Show login-item + daemon state (default)
  status_drives      Per-drive tier + telemetry table (Hot/Warm/Parked/Cold)
  changed-since      USN-journal delta probe: files changed since a cursor
    DRIVE              Drive letter (exactly one)
    --journal-id N     Journal identity from a previous call (0 = bootstrap)
    --since-usn N      USN cursor from a previous call (0 = bootstrap)
    --max-records N    Bound raw journal records read in this one call
";

/// Print daemon help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_daemon_help() {
    print!("{DAEMON_HELP}");
}

/// Help text for `uffs --stats`.
const STATS_HELP: &str = "\
uffs --stats — Show filesystem statistics (the `overview` aggregate preset)

USAGE:  uffs --stats [OPTIONS]

OPTIONS:
  --data-dir <PATH>    Data directory with drive_* subdirs
  --mft-file <PATH>    Raw MFT file(s), comma-separated; repeatable
";

/// Print stats help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_stats_help() {
    print!("{STATS_HELP}");
}

/// Help text for `uffs --snapshot`.
const SNAPSHOT_HELP: &str = "\
uffs --snapshot — Capture the live MFT to a baseline file

Save the drive's current MFT so a later `uffs --diff <FILE> --drive C` can
report what was deleted since. Reads the live NTFS MFT: Windows + Administrator.

USAGE:  uffs --snapshot --drive <D> --out <FILE> [OPTIONS]

OPTIONS:
  -d, --drive <D>          Drive to capture (required, e.g. C).
  -o, --out <FILE>         Output .bin path (required).
  --no-compress            Store uncompressed (default: zstd-compressed).
  --compression-level <N>  zstd level 1-22 (default 3).
  --raw                    Headerless raw dump for other MFT tools; implies
                           --no-compress and is NOT loadable by `uffs --diff`.

EXAMPLE:
  uffs --snapshot --drive C --out C_baseline.bin
  uffs --diff C_baseline.bin --drive C '*.txt'   # later: what .txt was deleted
";

/// Print snapshot help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_snapshot_help() {
    print!("{SNAPSHOT_HELP}");
}

/// Help text for `uffs --deleted`.
const DELETED_HELP: &str = "\
uffs --deleted — Forensic tombstone read (recently-deleted files)

When NTFS deletes a file it clears the in-use flag but leaves the record (name,
parent, timestamps) intact until the MFT slot is reused. This surfaces those
not-in-use records as recently-deleted tombstones and reconstructs each path
from the surviving parent chain. No baseline needed.

USAGE:  uffs --deleted (--mft-file <PATH> | --drive <D>) [OPTIONS]

SOURCE (one required):
  --mft-file <PATH>    Offline MFT capture to scan.
  -d, --drive <D>      Live volume scan (Windows, elevated). With --mft-file,
                       just labels reconstructed paths.

OPTIONS:
  -n, --limit <N>      Max tombstones to print (0 = all).
  --json               Emit JSON instead of a table.

LIMITS (best-effort by nature):
  - Only deletes whose MFT slot has NOT been recycled are visible.
  - The timestamp is the file's last-write time, NOT the deletion time.
  - A path is unreliable if a parent directory's slot was itself reused
    (such paths are prefixed with `…`).

EXAMPLE:
  uffs --deleted --mft-file C_mft.bin --drive C --limit 50
";

/// Print deleted help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_deleted_help() {
    print!("{DELETED_HELP}");
}

/// Help text for `uffs --agg`.
const AGGREGATE_HELP: &str = "\
uffs --agg — Run aggregate analytics on the filesystem index

USAGE:  uffs --agg <PRESET> [OPTIONS]

ARGUMENTS:
  <PRESET>             a preset (listed below), `count`, or a raw spec
                       such as 'terms:extension,top=2000'

              Each preset has a built-in top-N cap (e.g. by_extension
              caps at 50 buckets). --agg-cursor/--agg-page-size page
              through that cap; they do not raise it. To see more
              than a preset's cap, use the raw terms syntax instead:
                uffs --agg 'terms:extension,top=2000' --format json

OPTIONS:
  --format <FMT>       Output format: table (default), csv, json
  --data-dir <PATH>    Data directory with drive_* subdirs
  --mft-file <PATH>    Raw MFT file(s)
  --agg-cursor <TOK>   Continue from previous page
  --agg-page-size <N>  Max buckets per page (within the preset's cap)
";

/// Print aggregate help: [`AGGREGATE_HELP`], then the presets from
/// `uffs_client::format::AGGREGATE_PRESETS`, the list the parser itself
/// accepts, so the help cannot name a preset that is not there or miss one
/// that is.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_aggregate_help() {
    print!("{AGGREGATE_HELP}{}", aggregate_presets_help());
}

/// The `PRESETS:` section of `uffs --agg --help`, one line per preset with
/// its other spellings.
fn aggregate_presets_help() -> String {
    let lines: String = uffs_client::format::AGGREGATE_PRESETS
        .iter()
        .map(|preset| {
            if preset.aliases.is_empty() {
                format!("  {}\n", preset.name)
            } else {
                format!(
                    "  {:<20} (also: {})\n",
                    preset.name,
                    preset.aliases.join(", ")
                )
            }
        })
        .collect();
    format!("\nPRESETS:\n{lines}")
}

/// Help text for `uffs --status`.
const STATUS_HELP: &str = "\
uffs --status — Show combined system status (daemon + broker + MCP)

USAGE:  uffs --status [OPTIONS]

OPTIONS:
  -v, --verbose   Expand every section (build, broker mode, live-update,
                  memory, paths; broker binary + uptime on Windows)
  --json          Machine-readable superset of all sections
  --brief         With --json: liveness booleans only. Skips the broker
                  pipe probe and the MCP process scan, so it costs a
                  socket connect instead of several subprocesses — for
                  supervisors and scripts that poll on a timer.
";

/// Print status help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub(crate) fn print_status_help() {
    print!("{STATUS_HELP}");
}

#[cfg(test)]
mod tests {
    /// The help lists every preset the parser accepts.
    #[test]
    fn aggregate_help_lists_every_preset() {
        let help = super::aggregate_presets_help();
        for preset in uffs_client::format::AGGREGATE_PRESETS {
            assert!(help.contains(preset.name), "help misses {}", preset.name);
        }
        for name in [
            "storage",
            "activity",
            "top_folders",
            "duplicates",
            "media",
            "cleanup",
        ] {
            assert!(help.contains(name), "help misses {name}");
        }
    }
}
