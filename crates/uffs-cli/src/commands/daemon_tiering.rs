// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs --daemon {hibernate, preload}` subcommand handlers.
//!
//! Phase 8-B / 8-C — operator-driven memory-tiering CLI commands.
//! Split off [`crate::commands::daemon_mgmt`] so the tiering cluster
//! stays under the 800-LOC policy ceiling without a file-size exception.
//! Forward-looking: sub-phases 8-D (`forget`) and 8-E
//! (`status_drives` table) will land their CLI shims in this same
//! file when the corresponding daemon RPCs come online.

use anyhow::{Context as _, Result};
use uffs_client::connect_sync::UffsClientSync;
use uffs_client::protocol::response::{
    DEFAULT_PRELOAD_PIN_MINUTES, DriveTierStatus, ForgetParams, HibernateParams, PreloadParams,
};

/// `uffs --daemon hibernate [DRIVES...]` — demote loaded shards to `Cold`.
///
/// Releases RAM but keeps the encrypted compact cache on disk so a
/// subsequent `preload` / search can re-warm without a full MFT
/// re-parse.
///
/// Empty `drives` ⇒ hibernate every drive the daemon currently knows
/// about.  The daemon's response carries the per-pre-call-tier
/// breakdown which we render so the operator sees what actually
/// changed (vs `already_cold` no-ops).
///
/// # Errors
///
/// Returns an error when the daemon is not running or the
/// `hibernate` RPC fails.
///
/// # Example
///
/// ```bash
/// $ uffs --daemon hibernate
/// Daemon hibernated 2 drive(s):
///   Hot     -> Cold:  C
///   Warm    -> Cold:  D
///   Already Cold:     (none)
/// ```
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn daemon_hibernate(drives: &[uffs_mft::platform::DriveLetter]) -> Result<()> {
    let mut client = UffsClientSync::connect_raw()
        .map_err(|err| anyhow::anyhow!("Daemon is not running: {err}"))?;

    let params = HibernateParams {
        drives: drives.to_vec(),
    };
    let response = client
        .hibernate(&params)
        .with_context(|| "hibernate RPC failed")?;

    let total_demoted =
        response.hot_demoted.len() + response.warm_demoted.len() + response.parked_demoted.len();
    println!("Daemon hibernated {total_demoted} drive(s):");
    println!(
        "  Hot     -> Cold:  {}",
        format_drive_list(&response.hot_demoted)
    );
    println!(
        "  Warm    -> Cold:  {}",
        format_drive_list(&response.warm_demoted)
    );
    println!(
        "  Parked  -> Cold:  {}",
        format_drive_list(&response.parked_demoted)
    );
    println!(
        "  Already Cold:     {}",
        format_drive_list(&response.already_cold)
    );
    Ok(())
}

/// `uffs --daemon preload <DRIVES...> [--pin-minutes N]` — promote
/// drive(s) to `Hot` and pin the tier against demote for
/// `pin_minutes` minutes (defaults to
/// [`DEFAULT_PRELOAD_PIN_MINUTES`] when `None`).
///
/// At least one drive is required; the CLI parser at
/// [`crate::args::parse_daemon_action`] enforces that pre-flight,
/// so by the time this function runs the `drives` slice is non-
/// empty.  Per-drive failures (drive not loaded, body load failure,
/// transient state) are reported in the daemon's `errors` field
/// rather than as a top-level `Result::Err`.
///
/// # Errors
///
/// Returns an error when the daemon is not running or the `preload`
/// RPC itself fails (network / serialisation).  Per-drive
/// preload failures land in stdout under `Errors:` and do not
/// fail the CLI.
///
/// # Example
///
/// ```bash
/// $ uffs --daemon preload C D --pin-minutes 60
/// Daemon preloaded (60-min pin):
///   Promoted to Hot:  C, D
///   Pin expires at:   in 60m
/// ```
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn daemon_preload(
    drives: &[uffs_mft::platform::DriveLetter],
    pin_minutes: Option<u32>,
) -> Result<()> {
    let mut client = UffsClientSync::connect_raw()
        .map_err(|err| anyhow::anyhow!("Daemon is not running: {err}"))?;

    let params = PreloadParams {
        drives: drives.to_vec(),
        pin_minutes,
    };
    let response = client
        .preload(&params)
        .with_context(|| "preload RPC failed")?;

    let effective_pin = pin_minutes.unwrap_or(DEFAULT_PRELOAD_PIN_MINUTES);
    println!("Daemon preloaded ({effective_pin}-min pin):");
    println!(
        "  Promoted to Hot:  {}",
        format_drive_list(&response.promoted)
    );
    if !response.already_hot.is_empty() {
        println!(
            "  Already Hot:      {}",
            format_drive_list(&response.already_hot)
        );
    }
    if response.pin_until_unix_ms > 0 {
        println!(
            "  Pin expires at:   {}",
            format_ms_until(response.pin_until_unix_ms)
        );
    }
    if !response.errors.is_empty() {
        println!("  Errors:");
        for err in &response.errors {
            println!("    {err}");
        }
    }
    Ok(())
}

/// `uffs --daemon forget <DRIVES...> [--force]` — evict drive(s) from
/// the registry and delete every per-drive on-disk cache artefact.
///
/// Without `--force` the daemon refuses non-`Cold` drives with
/// `ERR_DRIVE_BUSY` (`-4`); the CLI surfaces that message verbatim
/// so the operator sees which drives are blocking the call.  With
/// `--force` the daemon auto-hibernates each non-`Cold` drive
/// first (clearing pins implicitly via the registry rebuild),
/// then deletes the cache files.
///
/// # Errors
///
/// Returns an error when the daemon is not running, the `forget`
/// RPC fails (network / serialisation / `ERR_DRIVE_BUSY`), or any
/// per-drive errors land in the response's `errors` field — the
/// CLI surfaces those as part of stdout, but a non-empty `errors`
/// list is still surfaced as a non-zero exit so scripted callers
/// can branch on success.
///
/// # Example
///
/// ```bash
/// $ uffs --daemon forget C --force
/// Daemon forgot 1 drive(s); freed 12.4 MiB:
///   Forgotten:        C
///   Already absent:   (none)
/// ```
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn daemon_forget(drives: &[uffs_mft::platform::DriveLetter], force: bool) -> Result<()> {
    let mut client = UffsClientSync::connect_raw()
        .map_err(|err| anyhow::anyhow!("Daemon is not running: {err}"))?;

    let params = ForgetParams {
        drives: drives.to_vec(),
        force,
    };
    let response = client
        .forget(&params)
        .with_context(|| "forget RPC failed")?;

    let total = response.forgotten.len() + response.already_absent.len();
    println!(
        "Daemon forgot {total} drive(s); freed {}:",
        format_bytes(response.freed_bytes)
    );
    println!(
        "  Forgotten:        {}",
        format_drive_list(&response.forgotten)
    );
    println!(
        "  Already absent:   {}",
        format_drive_list(&response.already_absent)
    );
    if !response.errors.is_empty() {
        println!("  Errors:");
        for err in &response.errors {
            println!("    {err}");
        }
        anyhow::bail!("forget completed with errors (see stdout above)");
    }
    Ok(())
}

/// `uffs --daemon status_drives` — render the per-drive tier +
/// telemetry table.
///
/// Operator-facing companion to `daemon status`: surfaces tier,
/// pin expiry, query rate (EWMA), resident-bytes, and last-query
/// timestamps for every drive the registry knows about — including
/// `Cold` shards (encrypted cache on disk, zero RAM) so `forget`
/// candidates are visible without cross-referencing tracing logs.
///
/// Output is a fixed-width table with one row per drive, sorted by
/// drive letter (ASCII ascending) so the order is stable across
/// re-runs.
///
/// # Errors
///
/// Returns an error when the daemon is not running or the
/// `status_drives` RPC fails (network / serialisation).  An empty
/// registry produces a "no drives loaded" hint instead of an
/// empty table.
///
/// # Example
///
/// ```bash
/// $ uffs --daemon status_drives
/// DRIVE  TIER    RESIDENT     QPM   LAST QUERY        PIN UNTIL        PROMOTIONS
/// C      hot     1.20 GiB   45.30   3m ago            in 57m                    3
/// D      warm    843 MiB     2.10   4m ago            -                         0
/// E      parked  12 MiB      0.00   8m ago            -                         1
/// F      cold    0 B         0.00   -                 -                         0
/// ```
///
/// The `PROMOTIONS` column counts each drive's promotions back from
/// `Parked` or `Cold` since daemon start, by a search or a `preload`
/// (see
/// [`uffs_client::protocol::response::DriveTierStatus::promotions_total`]).
/// Each one re-reads the drive's index from the on-disk cache; an
/// already-Warm `preload` (a tier-marker flip) does not count.
///
/// A drive whose index is being paged back in right now shows
/// `loading` in the `TIER` column instead of its pre-promote tier
/// ([`uffs_client::protocol::response::DriveTierStatus::loading`]).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn daemon_status_drives() -> Result<()> {
    // Read-only commands match `daemon status`'s graceful "daemon
    // down" rendering on connection failure — same stdout shape,
    // same exit 0 — so an operator pipeline like
    //   `uffs --daemon status_drives | grep cold`
    // doesn't crash on a stopped daemon.  Mutating commands
    // (`hibernate` / `preload` / `forget`) deliberately stay on the
    // bail-with-error path because the operator needs to know their
    // requested mutation didn't run.
    //
    // Note: only the **connect** failure is gracefully handled.  An
    // RPC dispatch failure (e.g. stale daemon returning
    // `ERR_METHOD_NOT_FOUND` for a method introduced after the
    // daemon was last rebuilt, or a serde decode error) surfaces the
    // real error so the operator sees the actual cause — rather than
    // a misleading "daemon is not running" when the daemon is
    // actually up but speaking an older protocol.
    let Ok(mut client) = UffsClientSync::connect_raw() else {
        crate::commands::daemon_status::print_not_running();
        return Ok(());
    };

    let response = client
        .status_drives()
        .with_context(|| "status_drives RPC failed")?;

    if response.drives.is_empty() {
        println!("(no drives loaded)");
        return Ok(());
    }

    println!(
        "{:<6} {:<7} {:<10} {:<7} {:<17} {:<16} {:>10}",
        "DRIVE", "TIER", "RESIDENT", "QPM", "LAST QUERY", "PIN UNTIL", "PROMOTIONS",
    );
    for drive in &response.drives {
        print_status_drive_row(drive);
    }
    Ok(())
}

/// Render a single [`DriveTierStatus`] row in the same layout as
/// the header in [`daemon_status_drives`].
///
/// Split out so the column widths stay co-located between the
/// header and the per-row writer — easier to keep aligned when the
/// schema gains a new column in a future sub-phase.  The
/// `PROMOTIONS` column right-aligns its integer (the `{:>10}`
/// format spec) since it is a count, not a label — this matches
/// the convention CLI tools use for numeric tail columns
/// (e.g. `du -h` puts the byte count on the left, but a counter
/// column reads better right-aligned for at-a-glance comparison).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_status_drive_row(drive: &DriveTierStatus) {
    let last_query = if drive.last_query_at_ms > 0 {
        format_ms_ago(drive.last_query_at_ms)
    } else {
        "-".to_owned()
    };
    let pin_until = if drive.pin_until_unix_ms > 0 {
        format_ms_until(drive.pin_until_unix_ms)
    } else {
        "-".to_owned()
    };
    // Mid-promote, the daemon still reports the pre-promote tier; say what
    // is actually happening instead of an idle-looking `parked`.
    let tier = if drive.loading {
        "loading"
    } else {
        drive.tier.as_str()
    };
    println!(
        "{:<6} {:<7} {:<10} {:<7.2} {:<17} {:<16} {:>10}",
        drive.letter,
        tier,
        format_bytes(drive.resident_bytes),
        drive.query_rate_per_min,
        last_query,
        pin_until,
        drive.promotions_total,
    );
}

/// Humanise a byte count into a fixed-width string.  Uses binary
/// units (KiB / MiB / GiB) since the underlying `resident_bytes`
/// reports `Vec::capacity * size_of`, which is naturally
/// power-of-two-aligned.
///
/// Implemented with pure integer arithmetic (no floats) so the
/// strict `clippy::float_arithmetic` gate stays satisfied — the
/// `.2` GiB rendering uses `(bytes % GIB) * 100 / GIB` to compute
/// the hundredths digit directly.
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    // Fixed 10-column cell: a 6-wide RIGHT-aligned magnitude then a
    // 3-wide left-aligned unit, so every row lines up on the decimal
    // point rather than ragging left off the unit:
    //
    //     1.070 GiB
    //       509 MiB
    //         2 MiB
    //
    // Left-aligning the whole cell (the previous behaviour) put `2 MiB`
    // and `1.07 GiB` at the same start column, which reads as noise in
    // a column you scan vertically to compare sizes.
    let (magnitude, unit) = if bytes >= GIB {
        let whole = bytes / GIB;
        let thousandths = (bytes % GIB).saturating_mul(1000) / GIB;
        (format!("{whole}.{thousandths:03}"), "GiB")
    } else if bytes >= MIB {
        ((bytes / MIB).to_string(), "MiB")
    } else if bytes >= KIB {
        ((bytes / KIB).to_string(), "KiB")
    } else {
        (bytes.to_string(), "B")
    };
    format!("{magnitude:>6} {unit:<3}")
}

/// Format a Unix-millisecond timestamp as a human-readable elapsed string
/// relative to now (e.g. `"just now"`, `"5s ago"`, `"3m ago"`, `"2h ago"`).
///
/// Used for the `LAST QUERY` column in [`daemon_status_drives`].  A
/// clock-read failure falls back to `"?"` so the operator always sees
/// *something* rather than a panic or a raw millisecond value.
fn format_ms_ago(unix_ms: i64) -> String {
    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|dur| i64::try_from(dur.as_millis()).ok())
        .unwrap_or(0);
    let secs = now_ms.saturating_sub(unix_ms) / 1_000;
    if secs < 5 {
        "just now".to_owned()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Format a Unix-millisecond future timestamp as a human-readable countdown
/// (e.g. `"in 60m"`, `"in 2h 30m"`, `"expired"`).
///
/// Used for the `PIN UNTIL` column in [`daemon_status_drives`] and for the
/// `Pin expires at:` line in [`daemon_preload`].  An already-expired pin or
/// a clock-read failure renders as `"expired"`.
fn format_ms_until(unix_ms: i64) -> String {
    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|dur| i64::try_from(dur.as_millis()).ok())
        .unwrap_or(0);
    let diff = unix_ms.saturating_sub(now_ms);
    if diff <= 0 {
        return "expired".to_owned();
    }
    let secs = diff / 1_000;
    if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3_600 {
        format!("in {}m", secs / 60)
    } else {
        let hours = secs / 3_600;
        let mins = (secs % 3_600) / 60;
        if mins == 0 {
            format!("in {hours}h")
        } else {
            format!("in {hours}h {mins}m")
        }
    }
}

/// Render a slice of drive letters as a comma-separated list, or
/// `"(none)"` when the slice is empty.  Keeps the per-line output
/// in [`daemon_hibernate`] / [`daemon_preload`] / [`daemon_forget`]
/// visually aligned even on no-op calls.
fn format_drive_list(drives: &[uffs_mft::platform::DriveLetter]) -> String {
    if drives.is_empty() {
        "(none)".to_owned()
    } else {
        // `DriveLetter` implements `Display` (renders as the
        // uppercase ASCII char), so `ToString::to_string` produces
        // the same output as the previous `char::to_string`.
        drives
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}
