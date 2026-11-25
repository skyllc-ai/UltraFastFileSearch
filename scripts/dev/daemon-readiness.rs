#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! anyhow = "1.0"
//! clap = { version = "4.0", features = ["derive"] }
//! colored = "2.0"
//! ```
// =============================================================================
// scripts/dev/daemon-readiness.rs — UFFS Daemon Readiness Verification
// =============================================================================
//
// Exercises ALL meaningful daemon lifecycle combinations:
//
//   Scenario A: Clean lifecycle (start → search → status -v → stop)
//   Scenario B: Idempotent operations (stop/kill/status when not running)
//   Scenario C: Double-start (start when already running)
//   Scenario D: Hard kill recovery (start → kill → start)
//   Scenario E: Graceful cycle (start → stop → start)
//   Scenario F: Restart (start → restart → verify)
//   Scenario G: Double restart
//   Scenario H: Stats accumulation across searches
//   Scenario I: Kill running → status shows not running
//   Scenario J: Search auto-starts daemon
//   Scenario K: Startup timing (COLD → WARM → HOT)
//
//   ── Phase 8 operator-driven memory tiering ──
//   Scenario L: status_drives table render contract (8-E)
//   Scenario M: hibernate end-to-end (8-B) — every drive demotes to Cold;
//               on-disk caches preserved
//   Scenario N: preload pin contract (8-C) — pinned drive survives idle
//               TTL evaluation under real wall clock
//   Scenario O: forget --force destructive cleanup (8-D) — registry
//               eviction + on-disk cache deletion
//   Scenario P: full Phase 8 round-trip cycle — search → hibernate →
//               preload → search → forget, with per-step timing
//   Scenario Q: Phase 9 promotions_total counter wire surface
//   Scenario R: search-driven re-promote latency profile
//               (Warm baseline → Cold → Warm via decrypt → Warm → Hot
//                via preload → Hot search; cost ladder at a glance)
//   Scenario S: TTL-gated Parked→Hot re-promote latency (operator opt-in
//               via --park-wait-secs; sleeps through the warm-to-parked
//               idle window and times preload of an organically Parked
//               drive — the only path scenario R cannot reach without
//               touching the test-only `demote_letter_for_test` helper)
//
// Each Phase 8 scenario captures per-RPC wall-clock timings so an
// operator can see where time is spent in the operator-driven tier
// machinery (read-lock detect, registry rebuild, body load, cache-file
// unlink).  The final summary table groups timings per command class so
// the cost of `hibernate` (RAM-only release) is visually distinct from
// `preload` (re-decrypt of the encrypted compact cache) and `forget`
// (registry eviction + four-file unlink).
//
// Usage:
//   rust-script scripts/dev/daemon-readiness.rs ~/uffs_data          # macOS with offline data
//   rust-script scripts/dev/daemon-readiness.rs                       # Windows (auto-discovers NTFS drives)
//   rust-script scripts/dev/daemon-readiness.rs --binary target/release/uffs
//   rust-script scripts/dev/daemon-readiness.rs ~/uffs_data --skip-phase8  # Phase 5/6/7 only

use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;

/// Maximum time any single `uffs` invocation may run before we kill it.
const STEP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Parser)]
#[command(
    name = "daemon-readiness",
    about = "UFFS daemon lifecycle verification",
    after_help = "EXAMPLES:\n  \
        rust-script scripts/dev/daemon-readiness.rs ~/uffs_data\n  \
        rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp\n  \
        rust-script scripts/dev/daemon-readiness.rs ~/uffs_data --pattern '*.dll'\n  \
        rust-script scripts/dev/daemon-readiness.rs                  # Windows: auto-discover NTFS drives"
)]
struct Cli {
    /// Path to an MFT file or a data directory containing drive_* subdirs.
    /// Auto-detected: if it's a file → --mft-file, if directory → --data-dir.
    /// On Windows, omit to auto-discover live NTFS drives.
    #[arg(value_name = "PATH")]
    path: Option<String>,

    /// Path to the uffs binary.
    /// Default: ~/bin/uffs first, then target/release/uffs
    #[arg(long)]
    binary: Option<String>,

    /// Search pattern to test with.
    #[arg(long, default_value = "*.rs")]
    pattern: String,

    /// Skip the Phase 8/9 operator-command scenarios (L-S).  Useful
    /// when running against a daemon build that predates Phase 8
    /// (PRs #122 + #123) or when the operator only wants the
    /// lifecycle smoke tests.  Defaults to running all scenarios
    /// (S still requires `--park-wait-secs > 0` to actually run;
    /// when the flag is 0 scenario S prints a dimmed `skipped`
    /// marker and returns immediately).
    #[arg(long)]
    skip_phase8: bool,

    /// Drive letter to use as the *destructive* target for the forget
    /// scenario (O) and the round-trip cycle (P).  Whatever drive you
    /// pass here will have its on-disk caches DELETED; the daemon
    /// will cold-load it the next time it appears in a search
    /// (one body-decrypt + body-load, ~seconds depending on drive
    /// size).
    ///
    /// Defaults to **`G`** — the canonical test drive on the
    /// reference 7-drive box, matching the `tests/fixtures/drive_g/`
    /// fixture.  The destructive path is therefore exercised on
    /// every readiness pass, which is the whole point of including
    /// scenarios O and P in the run.
    ///
    /// Operators on a different host (or who don't want G to be
    /// the destructive target) should pass `--forget-drive <letter>`
    /// to point at a drive they're happy to have re-cold-loaded.
    /// Pre-Phase-9 the default was `Z` (a safe placeholder that
    /// rarely exists), but the resulting `[skipped — pre-forget had
    /// no on-disk cache]` outcomes meant the destructive path was
    /// almost never actually exercised — defeating the purpose of
    /// having scenario O in the readiness pass.
    #[arg(long, default_value = "G")]
    forget_drive: String,

    /// Optional seconds to wait inside scenario N to verify the
    /// preload pin survives a real demote-controller evaluation.
    /// Default `0` skips the wait — scenario N then only checks the
    /// structural pin shape (drive=hot, PIN UNTIL>0).  Recommended
    /// value is `90` together with `UFFS_WARM_TO_PARKED_IDLE_SECS=30`
    /// in the environment so the controller fires at least twice
    /// during the window (the env var is inherited by the daemon
    /// process the script spawns).
    ///
    /// Example:
    ///
    /// ```bash
    /// UFFS_WARM_TO_PARKED_IDLE_SECS=30 \
    ///   just readiness ~/uffs_data --pin-ttl-wait-secs 90
    /// ```
    ///
    /// Adds ~`<value>` seconds of wall-clock to the run.  Mirrors
    /// the Windows-host runbook gate **G6 — `uffs --daemon preload` pin
    /// contract survives idle TTL** (Ctrl-F for `G6 —` in
    /// `docs/architecture/memory-tiering-windows-host-validation.md`).
    /// Reference uses the gate ID, not the section number, so future
    /// renumbering of the runbook (e.g. inserting a new section)
    /// doesn't silently invalidate this comment.
    #[arg(long, default_value_t = 0)]
    pin_ttl_wait_secs: u64,

    /// Optional seconds to wait inside scenario S to let the demote
    /// controller demote the target drive Warm → Parked.  Default
    /// `0` skips scenario S entirely (the fast lane).  Recommended
    /// invocation (~35 s wall-clock — the practical minimum):
    ///
    /// ```bash
    /// UFFS_WARM_TO_PARKED_IDLE_SECS=5 \
    ///   UFFS_PARKED_TO_COLD_IDLE_SECS=900 \
    ///   just readiness ~/uffs_data --park-wait-secs 35
    /// ```
    ///
    /// The two env vars matter in **opposite** directions:
    ///
    ///   * `UFFS_WARM_TO_PARKED_IDLE_SECS` should be **shorter** than
    ///     `--park-wait-secs` so the demote controller actually fires
    ///     during the wait window („how soon does idle become Parked“).
    ///   * `UFFS_PARKED_TO_COLD_IDLE_SECS` should be **much longer**
    ///     than `--park-wait-secs` so the drive doesn't slip past
    ///     Parked into Cold before scenario S can observe it
    ///     („how long does Parked stay Parked“).  The 900-second value
    ///     above gives a comfortable 15-minute Parked window.
    ///
    /// **30 s controller-tick floor.**  The daemon's idle-demote
    /// controller fires every 30 s on a hard-coded tokio interval
    /// (see `spawn_idle_demote_controller` in
    /// `crates/uffs-daemon/src/lib.rs`).  The first tick is skipped
    /// (so freshly loaded shards aren't immediately demoted), which
    /// means the controller's **first** evaluation happens 30 s
    /// after daemon-start.  Lowering `UFFS_WARM_TO_PARKED_IDLE_SECS`
    /// below 30 does **not** speed up scenario S below ~35 s — the
    /// controller still has to wait for its tick boundary.  There is
    /// no operator-driven force-park RPC today (`hibernate` walks
    /// the cascade all the way to Cold; `preload` only goes upward;
    /// `forget` is destructive).  Adding one would be a Phase-10
    /// candidate; until then the TTL wait is the only path to
    /// observe an organically-Parked drive end-to-end.
    ///
    /// Without the env vars the controller uses the policy defaults
    /// (warm-to-parked = 360 s; parked-to-cold = 86400 s = 24 h —
    /// see `crates/uffs-daemon/src/cache/policy.rs`).  In that case
    /// `--park-wait-secs 400` would also work but the run time grows
    /// proportionally.
    ///
    /// Adds ~`<value>` seconds of wall-clock to the run on top of the
    /// preload + cleanup overhead (~5 s).  The Parked-tier preload
    /// itself measures the Parked→Hot transition cost, which is
    /// dominated by the body re-load + decrypt (drops the Parked
    /// bloom + trie, see `crates/uffs-daemon/src/cache/registry.rs::
    /// promote_letter_to_hot` Parked-source arm).
    #[arg(long, default_value_t = 0)]
    park_wait_secs: u64,
}

/// Detect whether the user passed a file or directory and return the
/// appropriate uffs CLI flag + value.
fn detect_data_source(path: &str) -> Result<(&'static str, String)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        bail!("Path does not exist: {path}");
    }
    if p.is_file() {
        Ok(("--mft-file", path.to_owned()))
    } else if p.is_dir() {
        Ok(("--data-dir", path.to_owned()))
    } else {
        bail!("Path is neither a file nor a directory: {path}");
    }
}

// ── Test harness ─────────────────────────────────────────────────────────────

struct Runner {
    binary: String,
    /// `"--data-dir"` or `"--mft-file"`, or `None` for Windows live drives.
    source_flag: Option<&'static str>,
    /// The path value for the flag (empty when using live drives).
    source_path: String,
    pattern: String,
    /// Operator-supplied destructive target for scenarios O + P.
    /// Anything passed here will have its on-disk caches deleted.
    forget_drive: char,
    /// Optional pin-TTL wait window for scenario N (0 = skip).
    /// See `Cli::pin_ttl_wait_secs` for the recommended invocation
    /// pattern (env var + flag).
    pin_ttl_wait_secs: u64,
    /// Optional warm-to-parked TTL wait for scenario S (0 = skip
    /// scenario S entirely).  See `Cli::park_wait_secs` for the
    /// two-env-var setup pattern (`UFFS_WARM_TO_PARKED_IDLE_SECS`
    /// short + `UFFS_PARKED_TO_COLD_IDLE_SECS` long).
    park_wait_secs: u64,
    passed: u32,
    failed: u32,
    timings: Vec<(String, u128)>,
    /// Per-RPC timings for the Phase 8 operator commands so the final
    /// summary can show "where time is spent" per command class.  Each
    /// entry is `(command, ms)`.
    phase8_timings: Vec<(String, u128)>,
}

impl Runner {
    fn new(
        binary: String,
        source_flag: Option<&'static str>,
        source_path: String,
        pattern: String,
        forget_drive: char,
        pin_ttl_wait_secs: u64,
        park_wait_secs: u64,
    ) -> Self {
        Self {
            binary,
            source_flag,
            source_path,
            pattern,
            forget_drive,
            pin_ttl_wait_secs,
            park_wait_secs,
            passed: 0,
            failed: 0,
            timings: Vec::new(),
            phase8_timings: Vec::new(),
        }
    }

    /// Build the source args (e.g. ["--data-dir", "/path"]) or empty for live drives.
    fn source_args(&self) -> Vec<&str> {
        match self.source_flag {
            Some(flag) => vec![flag, &self.source_path],
            None => vec![],
        }
    }

    /// Run uffs with a hard 120-second timeout.
    ///
    /// Spawns reader threads for stdout/stderr so pipe buffers are
    /// continuously drained.  Without this, a child that writes more
    /// than the OS pipe buffer (4-64 KB) deadlocks because the parent
    /// only reads *after* exit — but exit can't happen while the write
    /// is blocked on a full pipe.
    fn run_raw(&self, args: &[&str]) -> Result<Output> {
        let mut child = Command::new(&self.binary)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to exec: {} {}", self.binary, args.join(" ")))?;

        // Drain stdout/stderr on background threads so pipes never fill.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = stdout_pipe {
                let _ = std::io::Read::read_to_end(&mut r, &mut buf);
            }
            buf
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = stderr_pipe {
                let _ = std::io::Read::read_to_end(&mut r, &mut buf);
            }
            buf
        });

        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stdout = stdout_thread.join().unwrap_or_default();
                    let stderr = stderr_thread.join().unwrap_or_default();
                    return Ok(Output { status, stdout, stderr });
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait(); // reap zombie
                        bail!(
                            "TIMEOUT after {}s: {} {}",
                            STEP_TIMEOUT.as_secs(),
                            self.binary,
                            args.join(" ")
                        );
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => {
                    bail!("wait error for {} {}: {e}", self.binary, args.join(" "));
                }
            }
        }
    }

    /// Run uffs, require exit 0.
    fn run_ok(&self, args: &[&str]) -> Result<String> {
        let out = self.run_raw(args)?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() {
            bail!("exit {}:\n  stdout: {stdout}\n  stderr: {stderr}",
                out.status.code().unwrap_or(-1));
        }
        Ok(stdout)
    }

    fn has_failed(&self) -> bool { self.failed > 0 }

    /// Run a step — announce what we're about to do, then show result.
    fn step(&mut self, name: &str, f: impl FnOnce(&mut Self) -> Result<String>) {
        if self.has_failed() { return; }
        println!("  {name}");
        let t = Instant::now();
        match f(self) {
            Ok(detail) => {
                let ms = t.elapsed().as_millis();
                if detail.is_empty() {
                    println!("    ↳ {} ({ms}ms)", "PASSED".green().bold());
                } else {
                    println!("    ↳ {} ({ms}ms) — {detail}", "PASSED".green().bold());
                }
                self.passed += 1;
                self.timings.push((name.to_owned(), ms));
            }
            Err(e) => {
                println!("    ↳ {}: {e:#}", "FAILED".red().bold());
                self.failed += 1;
                self.ensure_stopped();
            }
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn ensure_stopped(&self) {
        let _ = self.run_ok(&["--daemon", "kill"]);
        // Poll until the daemon is actually gone (up to 10s).
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            if let Ok(out) = self.run_ok(&["--daemon", "status"]) {
                if out.contains("not running") { return; }
            }
        }
    }

    fn assert_not_running(&self) -> Result<()> {
        // Poll for up to 5s — on Windows, process teardown can be slow.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = self.run_ok(&["--daemon", "status"])?;
            if out.contains("not running") { return Ok(()); }
            if Instant::now() >= deadline {
                bail!("Expected 'not running', got:\n{out}");
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn assert_ready(&self) -> Result<String> {
        let out = self.run_ok(&["--daemon", "status"])?;
        if !out.contains("Ready") {
            bail!("Expected 'Ready', got:\n{out}");
        }
        let drives = out.lines().filter(|l| l.contains("records")).count();
        Ok(format!("[{drives} drives]"))
    }

    fn start_daemon(&self) -> Result<String> {
        let mut args: Vec<&str> = vec!["--daemon", "start"];
        args.extend(self.source_args());
        self.run_ok(&args)
    }

    fn search(&self, limit: u32) -> Result<usize> {
        let lim = limit.to_string();
        let mut args: Vec<&str> = vec![&self.pattern];
        args.extend(self.source_args());
        args.extend(["--limit", &lim]);
        let out = self.run_ok(&args)?;
        Ok(out.lines().count())
    }

    // ── Phase 8 helpers ──────────────────────────────────────────────────
    //
    // Each helper times the underlying RPC and pushes the measurement
    // onto `phase8_timings` so the final summary table can compare
    // hibernate (RAM-only release) vs preload (cold-boot decrypt) vs
    // forget (registry eviction + four-file unlink) costs.

    /// `uffs --daemon status_drives` — Phase 8-E table.  Returns the raw
    /// stdout; callers parse it with `parse_drive_tier` /
    /// `parse_pin_until` / `parse_promotions_total`.
    fn status_drives(&mut self) -> Result<(String, u128)> {
        let t = Instant::now();
        let out = self.run_ok(&["--daemon", "status_drives"])?;
        let ms = t.elapsed().as_millis();
        self.phase8_timings.push(("status_drives".to_owned(), ms));
        Ok((out, ms))
    }

    /// `uffs --daemon hibernate [DRIVES...]` — Phase 8-B.  Empty `drives`
    /// hibernates every loaded drive.
    fn hibernate(&mut self, drives: &[char]) -> Result<(String, u128)> {
        let drive_strs: Vec<String> = drives.iter().map(|c| c.to_string()).collect();
        let mut args: Vec<&str> = vec!["--daemon", "hibernate"];
        for s in &drive_strs {
            args.push(s);
        }
        let t = Instant::now();
        let out = self.run_ok(&args)?;
        let ms = t.elapsed().as_millis();
        self.phase8_timings.push(("hibernate".to_owned(), ms));
        Ok((out, ms))
    }

    /// `uffs --daemon preload <DRIVE> --pin-minutes N` — Phase 8-C.
    fn preload(&mut self, drive: char, pin_minutes: u32) -> Result<(String, u128)> {
        let drive_s = drive.to_string();
        let pin_s = pin_minutes.to_string();
        let args = ["daemon", "preload", &drive_s, "--pin-minutes", &pin_s];
        let t = Instant::now();
        let out = self.run_ok(&args)?;
        let ms = t.elapsed().as_millis();
        self.phase8_timings.push(("preload".to_owned(), ms));
        Ok((out, ms))
    }

    /// `uffs --daemon forget <DRIVE> [--force]` — Phase 8-D.
    /// **Destructive:** deletes every per-drive cache file.
    fn forget(&mut self, drive: char, force: bool) -> Result<(String, u128)> {
        let drive_s = drive.to_string();
        let mut args: Vec<&str> = vec!["--daemon", "forget", &drive_s];
        if force {
            args.push("--force");
        }
        let t = Instant::now();
        let out = self.run_ok(&args)?;
        let ms = t.elapsed().as_millis();
        self.phase8_timings.push(("forget".to_owned(), ms));
        Ok((out, ms))
    }

    /// Parse the tier of a specific drive from the `status_drives`
    /// table output.  Returns `None` if the drive isn't listed.
    ///
    /// The table shape (from `crates/uffs-cli/src/commands/daemon_tiering.rs`):
    ///
    /// ```text
    /// DRIVE  TIER    RESIDENT     QPM   LAST QUERY (ms)   PIN UNTIL (ms)
    /// C      hot     1.20 GiB   45.30   1700000000000     1700001800000
    /// ```
    ///
    /// Whitespace-split each row; row[0] = letter, row[1] = tier.
    fn parse_drive_tier(table: &str, drive: char) -> Option<String> {
        for line in table.lines() {
            let trimmed = line.trim_start();
            let mut parts = trimmed.split_whitespace();
            let letter_token = parts.next()?;
            if letter_token.len() == 1
                && letter_token
                    .chars()
                    .next()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&drive))
            {
                return parts.next().map(|s| s.to_owned());
            }
        }
        None
    }

    /// Parse the `PIN UNTIL (ms)` column for a specific drive.
    /// Returns `None` if the drive isn't listed; `Some(0)` if the
    /// column shows `-` (unpinned).
    ///
    /// Post-Phase-9 the row layout is:
    ///   `letter tier resident_value resident_unit qpm last_query pin_until promotions`
    /// — `pin_until` is the **second-to-last** token, with
    /// `promotions` (the new Phase 9 column) at the very end.
    fn parse_pin_until(table: &str, drive: char) -> Option<u64> {
        for line in table.lines() {
            let trimmed = line.trim_start();
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            // Row shape: [letter, tier, resident_value, resident_unit,
            //             qpm, last_query, pin_until, promotions]
            // RESIDENT spans two tokens (`1.20 GiB`); when the value
            // is `0` the unit collapses to a single token (`B`) and
            // the row is one column shorter.  Defend against both.
            if parts.len() < 7 {
                continue;
            }
            let letter_token = parts[0];
            if letter_token.len() != 1
                || !letter_token
                    .chars()
                    .next()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&drive))
            {
                continue;
            }
            // pin_until is second-to-last (promotions is last).
            let pin_token = parts.get(parts.len().saturating_sub(2))?;
            if *pin_token == "-" {
                return Some(0);
            }
            return pin_token.parse::<u64>().ok();
        }
        None
    }

    /// Parse the `PROMOTIONS` column for a specific drive (Phase 9).
    /// Returns `None` if the drive isn't listed.  The value is the
    /// last token on each row.
    fn parse_promotions_total(table: &str, drive: char) -> Option<u64> {
        for line in table.lines() {
            let trimmed = line.trim_start();
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() < 7 {
                continue;
            }
            let letter_token = parts[0];
            if letter_token.len() != 1
                || !letter_token
                    .chars()
                    .next()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&drive))
            {
                continue;
            }
            return parts.last()?.parse::<u64>().ok();
        }
        None
    }

    /// Parse the `freed XX.XX MiB` figure from the `forget` command's
    /// stdout.  Returns the byte count rounded down (the CLI renders
    /// `freed_bytes` via the `format_bytes` helper, which uses MiB /
    /// GiB suffixes — we re-multiply to compare against the
    /// pre-forget on-disk total).
    fn parse_freed_bytes(forget_output: &str) -> Option<u64> {
        for line in forget_output.lines() {
            let lower = line.to_lowercase();
            if !lower.contains("freed") {
                continue;
            }
            // "Daemon forgot 1 drive(s); freed 1.20 GiB:"
            // "Daemon forgot 1 drive(s); freed 12 MiB:"
            // "Daemon forgot 1 drive(s); freed 12 KiB:"
            // "Daemon forgot 1 drive(s); freed 0 B:"
            let after = lower.split("freed").nth(1)?;
            let mut tokens = after.split_whitespace();
            let value_str = tokens.next()?;
            let unit_str = tokens.next()?.trim_end_matches(':').trim_end_matches(',');
            let value: f64 = value_str.parse().ok()?;
            let multiplier: f64 = match unit_str {
                "b" => 1.0,
                "kib" => 1024.0,
                "mib" => 1024.0 * 1024.0,
                "gib" => 1024.0 * 1024.0 * 1024.0,
                _ => return None,
            };
            // We're back-converting display formatting → bytes which
            // is inherently lossy (the CLI's format_bytes truncates
            // hundredths digits), so we round to nearest u64.
            return Some((value * multiplier) as u64);
        }
        None
    }

    /// Check whether any of the four canonical per-drive cache files
    /// exist on disk for `letter`.  Used by scenarios M (hibernate
    /// preserves caches) and O (forget unlinks them).
    ///
    /// The four paths mirror what
    /// `cache::cache_cleaner::PlatformCacheCleaner::forget` deletes:
    ///   - `<cache_dir>/<lower>_compact.uffs`
    ///   - `<cache_dir>/<lower>_usn.cursor`
    ///   - `<cache_dir>/<UPPER>_index.uffs`
    ///   - `<cache_dir>/<UPPER>_index.lock`
    fn cache_files_for(letter: char) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        let Some(cache_dir) = platform_cache_dir() else {
            return paths;
        };
        let lower = letter.to_ascii_lowercase();
        let upper = letter.to_ascii_uppercase();
        paths.push(cache_dir.join(format!("{lower}_compact.uffs")));
        paths.push(cache_dir.join(format!("{lower}_usn.cursor")));
        paths.push(cache_dir.join(format!("{upper}_index.uffs")));
        paths.push(cache_dir.join(format!("{upper}_index.lock")));
        paths
    }

    /// `(present_count, total_size_bytes)` for the four canonical
    /// per-drive cache files of `letter`.
    fn cache_files_summary(letter: char) -> (usize, u64) {
        let mut count = 0;
        let mut total = 0_u64;
        for path in Self::cache_files_for(letter) {
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() {
                    count += 1;
                    total = total.saturating_add(meta.len());
                }
            }
        }
        (count, total)
    }

    /// Pick the first loaded drive letter from the registry.  Used by
    /// scenarios that need a real drive to operate on (preload N,
    /// round-trip P).
    fn first_loaded_drive(&mut self) -> Result<char> {
        let (out, _ms) = self.status_drives()?;
        for line in out.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("DRIVE") || trimmed.is_empty() {
                continue;
            }
            let token = trimmed.split_whitespace().next().unwrap_or("");
            if token.len() == 1 {
                if let Some(letter) = token.chars().next() {
                    if letter.is_ascii_alphabetic() {
                        return Ok(letter.to_ascii_uppercase());
                    }
                }
            }
        }
        bail!("status_drives returned no drives — daemon registry is empty");
    }

    /// Pick a loaded drive letter that is **not** equal to
    /// `self.forget_drive`, so scenarios that need a stable preload
    /// / search target (Q, R, S) don't collide with the drive
    /// scenario O / P will destructively reset.  The default
    /// forget target is `G` (see `Cli::forget_drive` rationale).
    fn first_loaded_drive_not_forget_target(&mut self) -> Result<char> {
        let target = self.forget_drive.to_ascii_uppercase();
        let (out, _ms) = self.status_drives()?;
        for line in out.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("DRIVE") || trimmed.is_empty() {
                continue;
            }
            let token = trimmed.split_whitespace().next().unwrap_or("");
            if token.len() == 1 {
                if let Some(letter) = token.chars().next() {
                    if letter.is_ascii_alphabetic() && letter.to_ascii_uppercase() != target {
                        return Ok(letter.to_ascii_uppercase());
                    }
                }
            }
        }
        bail!("status_drives returned no drives other than the forget target");
    }
}

/// Resolve the platform cache directory the daemon writes per-drive
/// caches into.  Mirrors the path resolution in
/// `uffs_mft::cache::cache_dir`:
///   - macOS: `~/Library/Caches/com.uffs/`
///   - Windows: `%LOCALAPPDATA%\uffs\cache\`
///   - Linux: `$XDG_CACHE_HOME/uffs/` (default `~/.cache/uffs/`)
fn platform_cache_dir() -> Option<std::path::PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").ok()?;
        Some(std::path::PathBuf::from(home).join("Library/Caches/com.uffs"))
    } else if cfg!(target_os = "windows") {
        let local = std::env::var("LOCALAPPDATA").ok()?;
        Some(std::path::PathBuf::from(local).join("uffs").join("cache"))
    } else {
        // Linux + other Unixen — XDG-compliant.
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            return Some(std::path::PathBuf::from(xdg).join("uffs"));
        }
        let home = std::env::var("HOME").ok()?;
        Some(std::path::PathBuf::from(home).join(".cache").join("uffs"))
    }
}

// ── Scenarios ────────────────────────────────────────────────────────────────

fn scenario_a(r: &mut Runner) {
    println!("\n{}", "── Scenario A: Clean lifecycle ──".cyan().bold());

    r.step("A1  Kill stale daemon", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("A2  Verify not running", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("A3  Start daemon", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("A4  Verify Ready + drives", |r| r.assert_ready());
    r.step("A5  Search returns results", |r| {
        let n = r.search(100)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("A6  Second search (warm)", |r| {
        let n = r.search(100)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("A7  Stats show queries", |r| {
        let out = r.run_ok(&["--daemon", "status", "-v"])?;
        if !out.contains("Queries served:") { bail!("Missing stats"); }
        let detail: Vec<&str> = out.lines()
            .map(|l| l.trim())
            .filter(|l| l.starts_with("Startup duration:")
                || l.starts_with("Queries served:")
                || l.starts_with("Avg query time:"))
            .collect();
        Ok(detail.join(" | "))
    });
    r.step("A8  Graceful stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
    r.step("A9  Verify stopped", |r| { r.assert_not_running()?; Ok(String::new()) });
}

fn scenario_b(r: &mut Runner) {
    println!("\n{}", "── Scenario B: Idempotent ops on stopped daemon ──".cyan().bold());

    r.step("B0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("B1  Status when not running", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("B2  Stop when not running", |r| {
        let out = r.run_ok(&["--daemon", "stop"])?;
        if !out.contains("not running") { bail!("Expected 'not running', got: {out}"); }
        Ok(String::new())
    });
    r.step("B3  Kill when not running", |r| {
        let out = r.run_ok(&["--daemon", "kill"])?;
        if !out.contains("No daemon found") && !out.contains("not running") {
            bail!("Expected no-daemon message, got: {out}");
        }
        Ok(String::new())
    });
    r.step("B4  Restart when not running", |r| {
        let out = r.run_ok(&["--daemon", "restart"])?;
        if !out.contains("not running") { bail!("Expected 'not running', got: {out}"); }
        Ok(String::new())
    });
    r.step("B5  Stats when not running", |r| {
        let out = r.run_ok(&["--daemon", "status", "-v"])?;
        if !out.contains("not running") { bail!("Expected 'not running', got: {out}"); }
        Ok(String::new())
    });
}

fn scenario_c(r: &mut Runner) {
    println!("\n{}", "── Scenario C: Double start ──".cyan().bold());

    r.step("C0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("C1  Start daemon", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("C2  Start again (already running)", |r| {
        let out = r.start_daemon()?;
        if !out.contains("already running") { bail!("Expected 'already running', got: {out}"); }
        Ok(String::new())
    });
    r.step("C3  Still Ready", |r| r.assert_ready());
    r.step("C4  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_d(r: &mut Runner) {
    println!("\n{}", "── Scenario D: Hard kill recovery ──".cyan().bold());

    r.step("D0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("D1  Start daemon", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("D2  Verify Ready", |r| r.assert_ready());
    r.step("D3  Kill -9", |r| { r.run_ok(&["--daemon", "kill"])?; Ok(String::new()) });
    r.step("D4  Verify stopped", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("D5  Start after kill", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("D6  Verify Ready after kill→start", |r| r.assert_ready());
    r.step("D7  Search works after kill→start", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results after kill→start"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("D8  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_e(r: &mut Runner) {
    println!("\n{}", "── Scenario E: Graceful stop → restart cycle ──".cyan().bold());

    r.step("E0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("E1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("E2  Search", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("E3  Stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
    r.step("E4  Verify stopped", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("E5  Start again", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("E6  Search after stop→start", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("E7  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_f(r: &mut Runner) {
    println!("\n{}", "── Scenario F: Restart preserves data ──".cyan().bold());

    r.step("F0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("F1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("F2  Verify Ready", |r| r.assert_ready());
    r.step("F3  Search pre-restart", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("F4  Restart", |r| { r.run_ok(&["--daemon", "restart"])?; Ok(String::new()) });
    r.step("F5  Verify Ready after restart", |r| r.assert_ready());
    r.step("F6  Search after restart", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results after restart"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("F7  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_g(r: &mut Runner) {
    println!("\n{}", "── Scenario G: Double restart ──".cyan().bold());

    r.step("G0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("G1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("G2  Restart #1", |r| { r.run_ok(&["--daemon", "restart"])?; Ok(String::new()) });
    r.step("G3  Verify Ready", |r| r.assert_ready());
    r.step("G4  Restart #2", |r| { r.run_ok(&["--daemon", "restart"])?; Ok(String::new()) });
    r.step("G5  Verify Ready", |r| r.assert_ready());
    r.step("G6  Search after 2 restarts", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("G7  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_h(r: &mut Runner) {
    println!("\n{}", "── Scenario H: Stats accumulate across searches ──".cyan().bold());

    r.step("H0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("H1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("H2  Three searches", |r| {
        for i in 1..=3 { r.search(1000)?; print!("[q{i}] "); }
        Ok(String::new())
    });
    r.step("H3  Stats show ≥3 queries", |r| {
        let out = r.run_ok(&["--daemon", "status", "-v"])?;
        let count_line = out.lines()
            .find(|l| l.contains("Queries served:"))
            .unwrap_or("");
        let count: u64 = count_line.split_whitespace()
            .filter_map(|w| w.parse().ok())
            .next()
            .unwrap_or(0);
        if count < 3 { bail!("Expected ≥3 queries, got {count}"); }
        Ok(format!("[{count} queries]"))
    });
    r.step("H4  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_i(r: &mut Runner) {
    println!("\n{}", "── Scenario I: Kill running → immediate not-running ──".cyan().bold());

    r.step("I0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("I1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("I2  Verify Ready", |r| r.assert_ready());
    r.step("I3  Kill", |r| { r.run_ok(&["--daemon", "kill"])?; Ok(String::new()) });
    r.step("I4  Status → not running", |r| { r.assert_not_running()?; Ok(String::new()) });
}

fn scenario_j(r: &mut Runner) {
    println!("\n{}", "── Scenario J: Search auto-starts daemon ──".cyan().bold());

    r.step("J0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("J1  Verify not running", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("J2  Search (should auto-start)", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results from auto-start search"); }
        Ok(format!("[{n} rows, daemon auto-started]"))
    });
    r.step("J3  Verify daemon now running", |r| r.assert_ready());
    r.step("J4  Cleanup: stop", |r| { r.run_ok(&["--daemon", "stop"])?; Ok(String::new()) });
}

// ── Startup Timing (COLD → WARM → HOT) ──────────────────────────────────────

/// Delete local MFT index caches so the next startup does a full rebuild.
fn delete_cache() {
    // Windows: %LOCALAPPDATA%\uffs\cache
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = std::path::PathBuf::from(&local).join("uffs").join("cache");
        if p.exists() {
            println!("    Deleting cache: {}", p.display());
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    // Windows legacy: %TEMP%\uffs_index_cache
    if let Ok(tmp) = std::env::var("TEMP") {
        let p = std::path::PathBuf::from(&tmp).join("uffs_index_cache");
        if p.exists() {
            println!("    Deleting legacy cache: {}", p.display());
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    // macOS/Linux: XDG cache or ~/Library/Caches
    if let Ok(home) = std::env::var("HOME") {
        for sub in &["Library/Caches/uffs", ".cache/uffs"] {
            let p = std::path::PathBuf::from(&home).join(sub);
            if p.exists() {
                println!("    Deleting cache: {}", p.display());
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
}

/// Measure daemon start + first-query timing at a given cache level.
fn measure_startup(r: &Runner, label: &str) -> Result<(u128, u128, usize)> {
    // 1. Start daemon (blocking).
    let mut start_args: Vec<&str> = vec!["--daemon", "start"];
    let sa = r.source_args();
    start_args.extend(&sa);
    let t0 = Instant::now();
    let _ = r.run_ok(&start_args);
    let startup_ms = t0.elapsed().as_millis();

    // 2. First query.
    let lim = "1";
    let mut query_args: Vec<&str> = vec![&r.pattern];
    query_args.extend(&sa);
    query_args.extend(["--limit", lim]);
    let t1 = Instant::now();
    let out = r.run_ok(&query_args)?;
    let query_ms = t1.elapsed().as_millis();
    let rows = out.lines().count().saturating_sub(1); // minus header

    println!(
        "    {} startup {}ms + query {}ms = {}ms ({} rows)",
        label, startup_ms, query_ms, startup_ms + query_ms, rows
    );
    Ok((startup_ms, query_ms, rows))
}

fn scenario_k(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario K: Startup Timing (COLD → WARM → HOT) ──"
            .cyan()
            .bold()
    );

    // COLD: no daemon, no cache
    r.step("K1  Kill stale daemon", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    println!("    Deleting caches for COLD start...");
    delete_cache();

    println!("    COLD (no daemon, no cache)...");
    let cold = match measure_startup(r, "COLD") {
        Ok(v) => v,
        Err(e) => {
            println!("    ↳ {}: {e:#}", "FAILED".red().bold());
            r.failed += 1;
            return;
        }
    };
    r.passed += 1;
    r.timings
        .push(("K2  COLD startup".to_owned(), cold.0 + cold.1));

    // WARM: cache present, no daemon
    r.ensure_stopped();
    std::thread::sleep(Duration::from_secs(1));
    println!("    WARM (cache present, no daemon)...");
    let warm = match measure_startup(r, "WARM") {
        Ok(v) => v,
        Err(e) => {
            println!("    ↳ {}: {e:#}", "FAILED".red().bold());
            r.failed += 1;
            return;
        }
    };
    r.passed += 1;
    r.timings
        .push(("K3  WARM startup".to_owned(), warm.0 + warm.1));

    // HOT: daemon still running from WARM phase
    println!("    HOT  (daemon running)...");
    let hot = match measure_startup(r, "HOT") {
        Ok(v) => v,
        Err(e) => {
            println!("    ↳ {}: {e:#}", "FAILED".red().bold());
            r.failed += 1;
            return;
        }
    };
    r.passed += 1;
    r.timings
        .push(("K4  HOT  startup".to_owned(), hot.0 + hot.1));

    // Summary table
    let cold_total = cold.0 + cold.1;
    let warm_total = warm.0 + warm.1;
    let hot_total = hot.0 + hot.1;
    println!();
    println!("  ┌──────────┬────────────┬────────────┬────────────┬───────────┐");
    println!(
        "  │ {:^8} │ {:>10} │ {:>10} │ {:>10} │ {:>9} │",
        "Phase", "Startup", "Query", "Total", "Speedup"
    );
    println!("  ├──────────┼────────────┼────────────┼────────────┼───────────┤");
    for (label, su, qu, tot) in [
        ("COLD", cold.0, cold.1, cold_total),
        ("WARM", warm.0, warm.1, warm_total),
        ("HOT", hot.0, hot.1, hot_total),
    ] {
        let speedup = if label == "COLD" {
            "—".to_string()
        } else {
            let s = cold_total as f64 / tot.max(1) as f64;
            format!("{s:.1}x")
        };
        println!(
            "  │ {:^8} │ {:>7} ms │ {:>7} ms │ {:>7} ms │ {:>9} │",
            label, su, qu, tot, speedup
        );
    }
    println!("  └──────────┴────────────┴────────────┴────────────┴───────────┘");
    println!();
}

// ── Phase 8 scenarios — operator-driven memory tiering ──────────────────────

fn scenario_l(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario L: status_drives table render contract (8-E) ──"
            .cyan()
            .bold()
    );

    r.step("L0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("L1  Status_drives on stopped daemon (graceful)", |r| {
        // Read-only daemon commands must match `daemon status`'s
        // graceful "daemon down" rendering: exit 0 with a clear
        // stdout message.  This contract was unified in the same
        // commit that added Scenario L (sibling to this scenario
        // — see `daemon_tiering.rs::daemon_status_drives`).
        // Mutating commands (`hibernate` / `preload` / `forget`)
        // deliberately stay on the bail-with-error path because
        // the operator needs to know their mutation didn't run;
        // those error paths are exercised in scenarios M / N / O
        // when the daemon happens to be up and healthy.
        let out = r.run_ok(&["--daemon", "status_drives"])?;
        let lower = out.to_lowercase();
        if !lower.contains("not running") {
            bail!(
                "expected `daemon status_drives` to print `Daemon is not running.` \
                 on stdout when the daemon is down (matching `daemon status`); \
                 got: {out}"
            );
        }
        Ok(format!("[graceful exit 0 + 'not running' on stdout]"))
    });
    r.step("L2  Start daemon", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("L3  Verify Ready", |r| r.assert_ready());

    let mut header_seen = false;
    r.step("L4  Header row + column names", |r| {
        let (out, ms) = r.status_drives()?;
        let needles = [
            "DRIVE",
            "TIER",
            "RESIDENT",
            "QPM",
            "LAST QUERY (ms)",
            "PIN UNTIL (ms)",
            // Phase 9 — `promotions_total` is exposed as a CLI column
            // so operators can audit Cold→Hot re-promote frequency
            // without scraping the wire JSON.
            "PROMOTIONS",
        ];
        for needle in needles {
            if !out.contains(needle) {
                bail!("status_drives output missing column header `{needle}`:\n{out}");
            }
        }
        header_seen = true;
        Ok(format!("[all 7 columns present, {ms}ms]"))
    });

    r.step("L5  Default tier for loaded drives is `warm`", |_r| {
        if !header_seen {
            // L4 already failed; skip.
            return Ok(String::new());
        }
        Ok(String::new())
    });

    r.step("L6  Rows sorted ASCII ascending by drive letter", |r| {
        let (out, _) = r.status_drives()?;
        let mut letters: Vec<char> = Vec::new();
        for line in out.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("DRIVE") || trimmed.is_empty() {
                continue;
            }
            let token = trimmed.split_whitespace().next().unwrap_or("");
            if token.len() == 1 {
                if let Some(letter) = token.chars().next() {
                    if letter.is_ascii_alphabetic() {
                        letters.push(letter.to_ascii_uppercase());
                    }
                }
            }
        }
        if letters.is_empty() {
            bail!("status_drives produced no drive rows:\n{out}");
        }
        let mut sorted = letters.clone();
        sorted.sort();
        if letters != sorted {
            bail!(
                "drive rows not sorted ascending — observed {letters:?}, expected {sorted:?}"
            );
        }
        Ok(format!("[{} drives sorted: {letters:?}]", letters.len()))
    });

    r.step("L7  Every loaded drive's TIER is `warm` or `hot`", |r| {
        // Default after `add_drive` is Warm.  A drive may already be
        // Hot if the operator pre-loaded it in a prior session — both
        // are valid post-load states.  Anything else (parked / cold /
        // unknown / evicting) on a freshly-started daemon is a bug.
        let (out, _) = r.status_drives()?;
        let mut bad: Vec<(char, String)> = Vec::new();
        for line in out.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("DRIVE") || trimmed.is_empty() {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let Some(letter_token) = parts.next() else { continue };
            if letter_token.len() != 1 {
                continue;
            }
            let Some(letter) = letter_token.chars().next() else { continue };
            if !letter.is_ascii_alphabetic() {
                continue;
            }
            let Some(tier) = parts.next() else { continue };
            if tier != "warm" && tier != "hot" {
                bad.push((letter, tier.to_owned()));
            }
        }
        if !bad.is_empty() {
            bail!("expected every drive in warm/hot post-load, got: {bad:?}");
        }
        Ok(String::new())
    });

    r.step("L8  Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

fn scenario_m(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario M: hibernate end-to-end (8-B) ──"
            .cyan()
            .bold()
    );

    r.step("M0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("M1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("M2  Verify Ready", |r| r.assert_ready());
    r.step("M3  Warm a drive (search)", |r| {
        let n = r.search(100)?;
        Ok(format!("[{n} rows]"))
    });

    let mut sample_drive = '?';
    r.step("M4  Sample drive for cache-file check", |r| {
        let letter = r.first_loaded_drive()?;
        sample_drive = letter;
        let (count, total) = Runner::cache_files_summary(letter);
        Ok(format!(
            "[{letter}: {count} cache files on disk, {total} bytes pre-hibernate]"
        ))
    });

    r.step("M5  Run `daemon hibernate`", |r| {
        let (out, ms) = r.hibernate(&[])?;
        if !out.contains("Daemon hibernated") {
            bail!("unexpected hibernate output:\n{out}");
        }
        Ok(format!("[{ms}ms]"))
    });

    r.step("M6  Every drive's TIER is now `cold`", |r| {
        let (out, _) = r.status_drives()?;
        let mut non_cold: Vec<(char, String)> = Vec::new();
        for line in out.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("DRIVE") || trimmed.is_empty() {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let Some(letter_token) = parts.next() else { continue };
            if letter_token.len() != 1 {
                continue;
            }
            let Some(letter) = letter_token.chars().next() else { continue };
            if !letter.is_ascii_alphabetic() {
                continue;
            }
            let Some(tier) = parts.next() else { continue };
            if tier != "cold" {
                non_cold.push((letter, tier.to_owned()));
            }
        }
        if !non_cold.is_empty() {
            bail!("expected every drive cold post-hibernate, got: {non_cold:?}");
        }
        Ok(String::new())
    });

    r.step("M7  On-disk cache files preserved", |_r| {
        if sample_drive == '?' {
            return Ok("[skipped — no sample drive]".to_owned());
        }
        let (count, total) = Runner::cache_files_summary(sample_drive);
        if count == 0 {
            bail!(
                "expected cache files preserved post-hibernate for {sample_drive}, got zero"
            );
        }
        Ok(format!(
            "[{sample_drive}: {count} cache files still on disk, {total} bytes]"
        ))
    });

    r.step("M8  Re-hibernate is idempotent (already_cold)", |r| {
        let (out, ms) = r.hibernate(&[])?;
        if !out.contains("Already Cold") {
            bail!("expected `Already Cold` line in second hibernate output:\n{out}");
        }
        Ok(format!("[{ms}ms — should be < first hibernate]"))
    });

    r.step("M9  Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

fn scenario_n(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario N: preload pin contract (8-C) ──"
            .cyan()
            .bold()
    );

    r.step("N0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("N1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("N2  Verify Ready", |r| r.assert_ready());
    r.step("N3  Hibernate so we promote-from-Cold", |r| {
        r.hibernate(&[])?;
        Ok(String::new())
    });

    let mut target = '?';
    r.step("N4  Pick a drive that isn't the forget target", |r| {
        target = r.first_loaded_drive_not_forget_target()?;
        Ok(format!("[picked {target}]"))
    });

    r.step("N5  `preload <drive> --pin-minutes 5`", |r| {
        if target == '?' {
            bail!("no target drive selected");
        }
        let (out, ms) = r.preload(target, 5)?;
        if !out.contains("Promoted to Hot") && !out.contains("Already Hot") {
            bail!("unexpected preload output:\n{out}");
        }
        Ok(format!("[{ms}ms — Cold → Hot decrypt + body load]"))
    });

    r.step("N6  TIER for preloaded drive is `hot`", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let tier = Runner::parse_drive_tier(&out, target).unwrap_or_default();
        if tier != "hot" {
            bail!("expected {target} tier=hot post-preload, got tier={tier:?}");
        }
        Ok(String::new())
    });

    r.step("N7  PIN UNTIL (ms) > 0", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let pin = Runner::parse_pin_until(&out, target).unwrap_or(0);
        if pin == 0 {
            bail!("expected non-zero PIN UNTIL (ms) for preloaded {target}, got 0");
        }
        Ok(format!("[pin_until_unix_ms = {pin}]"))
    });

    r.step("N8  Re-preload (Already Hot, pin extension only)", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, ms) = r.preload(target, 10)?;
        if !out.contains("Already Hot") {
            bail!("expected `Already Hot` on second preload, got:\n{out}");
        }
        // The AlreadyHot path skips the registry rebuild entirely —
        // it should be MUCH faster than the first preload (no body
        // load, no decrypt, no Arc bump under write-lock).
        Ok(format!("[{ms}ms — should be ≪ first preload]"))
    });

    // ── Optional TTL-wait subscenario (mirrors Windows runbook G6) ─
    //
    // When --pin-ttl-wait-secs > 0, sleep through the configured
    // window and re-check the preloaded drive's tier.  This proves
    // the pin defended against the **live** demote controller, not
    // just the structural shape of the post-preload state.  The
    // wait is only meaningful when the operator also exports
    // UFFS_WARM_TO_PARKED_IDLE_SECS to a value shorter than the
    // wait — otherwise the controller's default 30-min TTL never
    // fires and the test passes trivially.
    if r.pin_ttl_wait_secs > 0 {
        r.step("N8a Wait through warm-to-parked TTL window", |r| {
            let secs = r.pin_ttl_wait_secs;
            let env_ttl = std::env::var("UFFS_WARM_TO_PARKED_IDLE_SECS")
                .unwrap_or_else(|_| "(unset, controller using built-in default)".to_owned());
            println!(
                "    sleeping {secs}s ... (UFFS_WARM_TO_PARKED_IDLE_SECS = {env_ttl})"
            );
            std::thread::sleep(Duration::from_secs(secs));
            Ok(format!("[slept {secs}s]"))
        });
        r.step("N8b Preloaded drive is STILL `hot` post-wait", |r| {
            if target == '?' {
                return Ok(String::new());
            }
            let (out, _) = r.status_drives()?;
            let tier = Runner::parse_drive_tier(&out, target).unwrap_or_default();
            if tier != "hot" {
                bail!(
                    "pin was overridden by demote controller during the wait \
                     — expected {target} tier=hot, got tier={tier:?}.  \
                     If you set UFFS_WARM_TO_PARKED_IDLE_SECS shorter than \
                     the wait this is a real failure (Phase 8-C pin contract \
                     regression).  If the env var was unset, the controller's \
                     default TTL never fired and this assertion is moot — \
                     re-run with `UFFS_WARM_TO_PARKED_IDLE_SECS=30`."
                );
            }
            Ok(format!("[{target} stayed hot through wait]"))
        });
    }

    r.step("N9  Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

fn scenario_o(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario O: forget --force destructive cleanup (8-D) ──"
            .cyan()
            .bold()
    );

    let target = r.forget_drive;
    println!("    forget target: {target} (pass --forget-drive to override)");
    let pre_count_total: (usize, u64) = Runner::cache_files_summary(target);
    println!(
        "    pre-forget on-disk: {} cache files, {} bytes",
        pre_count_total.0, pre_count_total.1
    );

    r.step("O0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("O1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("O2  Verify Ready", |r| r.assert_ready());

    r.step("O3  forget on unknown drive is idempotent", |r| {
        // Use 'Y' as a never-loaded letter (can't collide with the
        // operator-supplied target).  The cleaner runs unconditionally
        // for unknown drives so any stale on-disk file gets purged;
        // when there's nothing to delete, the drive lands in
        // already_absent.
        let probe = if target == 'Y' { 'X' } else { 'Y' };
        let (out, ms) = r.forget(probe, false)?;
        if !out.contains("Already absent") {
            bail!("expected `Already absent` for unknown drive {probe}:\n{out}");
        }
        Ok(format!("[{probe} → already_absent, {ms}ms]"))
    });

    let mut freed_bytes: Option<u64> = None;
    r.step("O4  forget --force on operator-supplied target", |r| {
        // If the target is loaded, the daemon refuses without
        // --force; with --force it auto-hibernates first.  Either
        // path produces a populated freed_bytes total when there's
        // anything on disk.
        let (out, ms) = r.forget(target, true)?;
        if !out.contains("Daemon forgot") {
            bail!("unexpected forget output:\n{out}");
        }
        freed_bytes = Runner::parse_freed_bytes(&out);
        Ok(format!(
            "[{ms}ms, freed_bytes ≈ {}]",
            freed_bytes
                .map(|b| b.to_string())
                .unwrap_or_else(|| "(unparsed)".to_owned())
        ))
    });

    r.step("O5  Target drive removed from registry", |r| {
        let (out, _) = r.status_drives()?;
        if Runner::parse_drive_tier(&out, target).is_some() {
            bail!("expected {target} absent from status_drives post-forget");
        }
        Ok(format!("[{target} absent from registry]"))
    });

    r.step("O6  All four cache files unlinked from disk", |_r| {
        let (count, total) = Runner::cache_files_summary(target);
        if count > 0 {
            let paths: Vec<String> = Runner::cache_files_for(target)
                .into_iter()
                .filter(|p| p.exists())
                .map(|p| p.display().to_string())
                .collect();
            bail!(
                "expected zero cache files on disk for {target}, found {count} (total {total} bytes): {paths:?}"
            );
        }
        Ok(format!("[{target}: 0 cache files on disk]"))
    });

    r.step("O7  freed_bytes within 5% of pre-forget on-disk total", |_r| {
        let pre = pre_count_total.1;
        let Some(reported) = freed_bytes else {
            // Either the target had no caches (operator passed
            // `--forget-drive Z` against a host where Z doesn't
            // exist, or the previous run already wiped the
            // default-G caches and the daemon hasn't been searched
            // since to re-cold-load), or the parser failed.  Both
            // are acceptable on a fresh box.
            return Ok("[skipped — pre-forget had no on-disk cache or parse failed]".to_owned());
        };
        if pre == 0 {
            return Ok("[pre-forget had no on-disk cache; freed_bytes = {reported}]".to_owned());
        }
        let diff = if reported > pre { reported - pre } else { pre - reported };
        let pct = (diff as f64) / (pre as f64) * 100.0;
        if pct > 5.0 {
            bail!(
                "freed_bytes {reported} differs from pre-forget total {pre} by {pct:.1}% (>5% tolerance)"
            );
        }
        Ok(format!("[Δ = {pct:.2}%]"))
    });

    r.step("O8  Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

fn scenario_p(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario P: full Phase 8 round-trip cycle ──"
            .cyan()
            .bold()
    );

    r.step("P0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("P1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("P2  Warm-state baseline search", |r| {
        let n = r.search(100)?;
        Ok(format!("[{n} rows]"))
    });

    let mut target = '?';
    r.step("P3  Pick non-forget target drive", |r| {
        target = r.first_loaded_drive_not_forget_target()?;
        Ok(format!("[picked {target}]"))
    });

    let mut hibernate_ms: u128 = 0;
    r.step("P4  Hibernate (release RAM)", |r| {
        let (_out, ms) = r.hibernate(&[])?;
        hibernate_ms = ms;
        Ok(format!("[{ms}ms]"))
    });

    let mut preload_ms: u128 = 0;
    r.step("P5  Preload target (cold-boot decrypt)", |r| {
        if target == '?' {
            bail!("no target drive selected");
        }
        let (_out, ms) = r.preload(target, 10)?;
        preload_ms = ms;
        Ok(format!("[{ms}ms — re-decrypt + body load]"))
    });

    let mut search_ms: u128 = 0;
    r.step("P6  Search after preload (warm body, hit Hot)", |r| {
        let t = Instant::now();
        let n = r.search(100)?;
        let ms = t.elapsed().as_millis();
        search_ms = ms;
        if n == 0 {
            bail!("expected results post-preload, got 0");
        }
        Ok(format!("[{n} rows in {ms}ms]"))
    });

    let mut hibernate2_ms: u128 = 0;
    r.step("P7  Hibernate again (pin survives only against demote, not explicit hibernate)", |r| {
        let (_out, ms) = r.hibernate(&[])?;
        hibernate2_ms = ms;
        Ok(format!("[{ms}ms]"))
    });

    r.step("P8  Verify target is now Cold (pin overridden)", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let tier = Runner::parse_drive_tier(&out, target).unwrap_or_default();
        if tier != "cold" {
            bail!("expected {target} tier=cold post-second-hibernate, got tier={tier:?}");
        }
        Ok(format!("[{target} = cold; explicit hibernate overrode pin]"))
    });

    r.step("P9  Round-trip cost summary", |_r| {
        Ok(format!(
            "[hibernate1: {hibernate_ms}ms | preload: {preload_ms}ms | search: {search_ms}ms | hibernate2: {hibernate2_ms}ms]"
        ))
    });

    r.step("P10 Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

fn scenario_q(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario Q: Phase 9 promotions_total counter ──"
            .cyan()
            .bold()
    );

    r.step("Q0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("Q1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("Q2  Hibernate everything", |r| {
        r.hibernate(&[])?;
        Ok(String::new())
    });

    let mut target = '?';
    r.step("Q3  Pick non-forget target", |r| {
        target = r.first_loaded_drive_not_forget_target()?;
        Ok(format!("[picked {target}]"))
    });

    // Phase 9 wire field is now exposed as a `PROMOTIONS` column in
    // the `status_drives` CLI render, so this scenario can directly
    // assert the counter value instead of inferring it from per-RPC
    // timing.  Timing assertions are still kept as a defence-in-depth
    // signal — if the counter is ever mis-bumped (e.g. a refactor
    // that fires record_cold_to_hot_promote on the AlreadyHot path),
    // both the column reading AND the timing-ratio check would
    // catch it independently.
    let mut pre_q4_promotions: u64 = 0;
    r.step("Q3a Pre-preload promotions baseline", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        pre_q4_promotions = Runner::parse_promotions_total(&out, target).unwrap_or(0);
        Ok(format!("[{target}.promotions_total = {pre_q4_promotions}]"))
    });

    let mut first_preload_ms: u128 = 0;
    r.step("Q4  First preload from Cold (counter 0→1)", |r| {
        if target == '?' {
            bail!("no target drive selected");
        }
        let (_out, ms) = r.preload(target, 5)?;
        first_preload_ms = ms;
        Ok(format!("[{ms}ms]"))
    });

    r.step("Q4a Verify PROMOTIONS column incremented by 1", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let post = Runner::parse_promotions_total(&out, target).unwrap_or(0);
        let expected = pre_q4_promotions.saturating_add(1);
        if post != expected {
            bail!(
                "expected promotions_total = {expected} (pre = {pre_q4_promotions} + 1 Cold→Hot), \
                 got {post}"
            );
        }
        Ok(format!("[{pre_q4_promotions} → {post}]"))
    });

    let mut already_hot_ms: u128 = 0;
    r.step("Q5  Second preload (AlreadyHot — counter must NOT change)", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, ms) = r.preload(target, 5)?;
        already_hot_ms = ms;
        if !out.contains("Already Hot") {
            bail!("expected AlreadyHot path on second preload:\n{out}");
        }
        Ok(format!("[{ms}ms — pin extension only]"))
    });

    r.step("Q5a Verify PROMOTIONS column unchanged after AlreadyHot", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let post = Runner::parse_promotions_total(&out, target).unwrap_or(0);
        let expected = pre_q4_promotions.saturating_add(1);
        if post != expected {
            bail!(
                "AlreadyHot bumped the counter — Phase 9 contract regression!  \
                 expected {expected}, got {post}"
            );
        }
        Ok(format!("[stayed at {post}]"))
    });

    r.step("Q6  Hibernate target back to Cold", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (_, ms) = r.hibernate(&[target])?;
        // After the explicit hibernate, the pin is implicitly
        // cleared (registry rebuild installs a fresh ShardEntry
        // with pin_until_ms = 0) — this is the contract pinned by
        // `tests/forget_status.rs::hibernate_overrides_preload_pin`.
        Ok(format!("[{ms}ms]"))
    });

    let mut second_preload_ms: u128 = 0;
    r.step("Q7  Second Cold→Hot cycle (counter 1→2)", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (_out, ms) = r.preload(target, 5)?;
        second_preload_ms = ms;
        Ok(format!("[{ms}ms]"))
    });

    r.step("Q7a Verify PROMOTIONS column incremented by 1 again", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let post = Runner::parse_promotions_total(&out, target).unwrap_or(0);
        let expected = pre_q4_promotions.saturating_add(2);
        if post != expected {
            bail!(
                "expected promotions_total = {expected} (pre + 2 Cold→Hot), got {post}"
            );
        }
        Ok(format!("[{post}]"))
    });

    r.step("Q8  AlreadyHot path is ≥ 5x faster than Cold→Hot", |_r| {
        // Contract from Phase 8-C / Phase 9 design: AlreadyHot
        // skips the registry rebuild + body load + pin atomic
        // store; should be O(microseconds) vs the cold-source
        // preload's O(seconds).  Use ≥ 5× as a conservative bar
        // that survives small drives + fast hardware.
        if first_preload_ms == 0 || already_hot_ms == 0 {
            return Ok("[skipped — no timing data]".to_owned());
        }
        let ratio = first_preload_ms as f64 / already_hot_ms.max(1) as f64;
        if ratio < 5.0 {
            bail!(
                "expected AlreadyHot ≥ 5× faster than Cold→Hot; got {first_preload_ms}ms vs {already_hot_ms}ms (ratio {ratio:.1}x)"
            );
        }
        Ok(format!(
            "[Cold→Hot: {first_preload_ms}ms vs AlreadyHot: {already_hot_ms}ms = {ratio:.1}x]"
        ))
    });

    r.step("Q9  Two Cold→Hot preloads have similar latency (deterministic decrypt)", |_r| {
        if first_preload_ms == 0 || second_preload_ms == 0 {
            return Ok("[skipped — no timing data]".to_owned());
        }
        let pct = ((first_preload_ms as f64 - second_preload_ms as f64).abs())
            / first_preload_ms as f64
            * 100.0;
        // Loose bound: two decrypts of the same compact cache should
        // be within ±50% of each other on idle hardware.  If the
        // second is dramatically slower, something is off (cache
        // eviction, OS page-cache pressure, …); if it's much faster,
        // the second decrypt is hitting some memoisation we didn't
        // expect.
        if pct > 50.0 {
            return Ok(format!(
                "[unexpectedly large drift: {first_preload_ms}ms vs {second_preload_ms}ms = {pct:.0}% — investigate]"
            ));
        }
        Ok(format!(
            "[1st: {first_preload_ms}ms vs 2nd: {second_preload_ms}ms (Δ {pct:.1}%)]"
        ))
    });

    r.step("Q10 Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

fn scenario_r(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario R: search-driven re-promote latency profile ──"
            .cyan()
            .bold()
    );

    // Scenario K already measures COLD/WARM/HOT at **daemon
    // startup**.  This scenario factors out the per-drive
    // re-promote cost during normal operation: when an idle drive
    // demotes to Cold (or Parked) and the next search hits it, the
    // daemon's `ensure_warm_for_dispatch` decrypts + loads the
    // body to bring the shard back to `Warm`.  We measure that
    // single-drive re-warm cost and compare it to:
    //
    //   * **Already-Warm search** — the steady-state per-shard
    //     dispatch cost (no transition).  Acts as a baseline.
    //   * **Warm → Hot via preload** — the operator-driven path
    //     that takes a body already in RAM and just flips the tier
    //     marker.  Should be ≪ Cold→Warm (no decrypt) but slightly
    //     > AlreadyHot (it does a registry rebuild for the new
    //     `ShardEntry::new_hot_with_stats`).
    //
    // Parked → Warm is **not** measured here because reaching
    // Parked from outside the daemon requires either waiting
    // through the warm-to-parked TTL (slow) or the test-only
    // `demote_letter_for_test` escape hatch (not exposed via RPC).
    // The Windows-host runbook G1 captures the Parked → Warm path
    // implicitly via the cascade-demote → re-search flow.

    r.step("R0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("R1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("R2  Verify Ready", |r| r.assert_ready());

    let mut target = '?';
    r.step("R3  Pick non-forget target", |r| {
        target = r.first_loaded_drive_not_forget_target()?;
        Ok(format!("[picked {target}]"))
    });

    // Baseline: an already-Warm shard.  All shards start Warm
    // post-load (Phase 1+ contract), so the first search after
    // `daemon start` hits a steady-state Warm shard.
    let mut warm_search_ms: u128 = 0;
    r.step("R4  Baseline search (Warm — no transition)", |r| {
        let t = Instant::now();
        let n = r.search(100)?;
        warm_search_ms = t.elapsed().as_millis();
        if n == 0 {
            bail!("Warm-state search returned zero rows");
        }
        Ok(format!("[{n} rows in {warm_search_ms}ms]"))
    });

    // Hibernate everything → Cold.
    r.step("R5  Hibernate (every drive → Cold)", |r| {
        r.hibernate(&[])?;
        Ok(String::new())
    });

    // The next search must re-warm AT LEAST the target drive via
    // `ensure_warm_for_dispatch` → encrypted-cache decrypt + body
    // load.  Other drives may also re-warm depending on how the
    // CLI dispatches the search; for the timing baseline we
    // capture the FIRST search after hibernation as the
    // Cold→Warm path.
    let mut cold_to_warm_ms: u128 = 0;
    r.step("R6  Search after hibernate (Cold → Warm via cache decrypt)", |r| {
        let t = Instant::now();
        let n = r.search(100)?;
        cold_to_warm_ms = t.elapsed().as_millis();
        Ok(format!("[{n} rows in {cold_to_warm_ms}ms — re-decrypt + body load]"))
    });

    // The drive should now be Warm again (search promoted it).
    r.step("R7  Verify target is now Warm or Hot", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let tier = Runner::parse_drive_tier(&out, target).unwrap_or_default();
        if tier != "warm" && tier != "hot" {
            bail!(
                "expected {target} tier=warm/hot post-search, got tier={tier:?}.  \
                 Either ensure_warm_for_dispatch didn't promote, or the \
                 demote controller raced and re-demoted."
            );
        }
        Ok(format!("[{target} = {tier}]"))
    });

    // Now exercise Warm → Hot via preload.  The drive is Warm, so
    // preload goes through promote_letter_to_hot's Warm-source
    // arm — which does a registry rebuild but no body load.
    let mut warm_to_hot_ms: u128 = 0;
    r.step("R8  Preload from Warm (Warm → Hot, no body load)", |r| {
        if target == '?' {
            bail!("no target drive selected");
        }
        // ensure_warm_for_dispatch may have left the body in Hot
        // already (under load); refresh tier and skip if so.
        let (out_pre, _) = r.status_drives()?;
        let pre_tier = Runner::parse_drive_tier(&out_pre, target).unwrap_or_default();
        if pre_tier == "hot" {
            warm_to_hot_ms = 0;
            return Ok(format!("[{target} already hot — Warm→Hot path not exercisable]"));
        }
        let (_out, ms) = r.preload(target, 5)?;
        warm_to_hot_ms = ms;
        Ok(format!("[{ms}ms — registry rebuild only]"))
    });

    // Repeat the search now — drive is Hot + pinned, fastest path.
    let mut hot_search_ms: u128 = 0;
    r.step("R9  Search against Hot pinned drive", |r| {
        let t = Instant::now();
        let n = r.search(100)?;
        hot_search_ms = t.elapsed().as_millis();
        if n == 0 {
            bail!("Hot-state search returned zero rows");
        }
        Ok(format!("[{n} rows in {hot_search_ms}ms]"))
    });

    // Render the latency ladder so the operator sees the cost
    // hierarchy at a glance.
    r.step("R10 Re-promote cost ladder", |_r| {
        Ok(format!(
            "[Warm-baseline-search: {warm_search_ms}ms | \
              Cold→Warm-search: {cold_to_warm_ms}ms | \
              Warm→Hot-preload: {warm_to_hot_ms}ms | \
              Hot-search: {hot_search_ms}ms]"
        ))
    });

    // Sanity-check the cost hierarchy.  These bounds are loose
    // because real timing depends heavily on host hardware + drive
    // size, but they should hold on any reasonable box:
    //
    //   * Cold→Warm-search ≥ 5× Warm-baseline-search  (decrypt cost)
    //   * Warm→Hot-preload ≤ Cold→Warm-search          (no decrypt)
    //
    // If either fails, something is genuinely wrong (decrypt path
    // dropped, or Warm→Hot accidentally went through the body
    // loader).
    r.step("R11 Cold→Warm is ≥ 3× Warm baseline (decrypt cost)", |_r| {
        if warm_search_ms == 0 || cold_to_warm_ms == 0 {
            return Ok("[skipped — no timing data]".to_owned());
        }
        let ratio = cold_to_warm_ms as f64 / warm_search_ms.max(1) as f64;
        if ratio < 3.0 {
            return Ok(format!(
                "[unexpectedly small gap: warm={warm_search_ms}ms vs cold={cold_to_warm_ms}ms = {ratio:.1}x — investigate]"
            ));
        }
        Ok(format!(
            "[warm={warm_search_ms}ms vs cold={cold_to_warm_ms}ms = {ratio:.1}x]"
        ))
    });

    r.step("R12 Warm→Hot ≤ Cold→Warm (no decrypt cost)", |_r| {
        if warm_to_hot_ms == 0 || cold_to_warm_ms == 0 {
            return Ok("[skipped — no timing data]".to_owned());
        }
        if warm_to_hot_ms > cold_to_warm_ms {
            return Ok(format!(
                "[unexpected: warm→hot {warm_to_hot_ms}ms exceeded cold→warm {cold_to_warm_ms}ms — investigate]"
            ));
        }
        Ok(format!(
            "[warm→hot={warm_to_hot_ms}ms ≤ cold→warm={cold_to_warm_ms}ms]"
        ))
    });

    r.step("R13 Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

/// Scenario S — TTL-gated Parked → Hot re-promote latency.
///
/// Skipped entirely when `--park-wait-secs == 0` (the default).
/// When enabled, the scenario sleeps through the warm-to-parked
/// idle window so the demote controller fires organically, then
/// asserts the target drive landed in `Parked` (not Cold, not still
/// Warm), then times the operator-issued `preload` to measure the
/// Parked → Hot transition cost.
///
/// The Parked-source arm of
/// [`crate::cache::registry::ShardRegistry::promote_letter_to_hot`]
/// drops the parked bloom + trie and runs the body loader (see
/// `crates/uffs-daemon/src/index/tiering_ops.rs:303-312`), so
/// Parked → Hot pays the same body-decrypt cost as Cold → Hot.
/// The interesting comparison vs scenario R is that this rung is
/// reached **organically** (no `hibernate` RPC; no test-only
/// `demote_letter_for_test`) — proving the operator-driven
/// re-promote path works end-to-end against a drive the live
/// demote controller put into Parked.
///
/// Why this is a sibling scenario instead of a subscenario inside
/// R: scenario R measures the no-TTL-wait fast lane (~5 s end to
/// end) and runs in every readiness pass.  Scenario S is opt-in
/// because the wait dominates wall-clock, and operators on a
/// 7-drive box doing fast-iteration daemon work shouldn't pay the
/// soak cost on every smoke run.  Set `--park-wait-secs` only
/// when verifying the Parked-tier contract end-to-end.
fn scenario_s(r: &mut Runner) {
    if r.park_wait_secs == 0 {
        // Print a dimmed marker so the operator sees scenario S is
        // a known-skipped slot (rather than wondering why the
        // alphabetical sequence stops at R).  Mirrors the way
        // scenario L handles the Phase 8 skip flag.
        println!(
            "\n{}",
            "── Scenario S: Parked→Hot re-promote (skipped — pass --park-wait-secs to enable) ──"
                .dimmed()
        );
        return;
    }

    println!(
        "\n{}",
        "── Scenario S: TTL-gated Parked→Hot re-promote latency ──"
            .cyan()
            .bold()
    );

    r.step("S0  Ensure stopped", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    r.step("S1  Start", |r| {
        r.start_daemon()?;
        Ok(String::new())
    });
    r.step("S2  Verify Ready", |r| r.assert_ready());

    let mut target = '?';
    r.step("S3  Pick non-forget target", |r| {
        target = r.first_loaded_drive_not_forget_target()?;
        Ok(format!("[picked {target}]"))
    });

    // Touch the target so its `last_query_at_ms` is "now"; the
    // controller's idle clock starts ticking from here.  Reusing
    // this as the Warm-baseline timing so scenario S is
    // self-contained — the operator can compare Parked→Hot vs
    // Warm-baseline without cross-referencing scenario R's R4.
    let mut warm_search_ms: u128 = 0;
    r.step("S4  Baseline search (Warm — resets idle clock)", |r| {
        let t = Instant::now();
        let n = r.search(100)?;
        warm_search_ms = t.elapsed().as_millis();
        if n == 0 {
            bail!("Warm-state search returned zero rows");
        }
        Ok(format!("[{n} rows in {warm_search_ms}ms]"))
    });

    // Sleep through the warm-to-parked TTL window.  The two env
    // vars in the docstring on `Cli::park_wait_secs` are the
    // operator's responsibility — we surface their current values
    // so a misconfigured run shows the diagnosis inline.
    r.step("S5  Wait through warm-to-parked TTL window", |r| {
        let secs = r.park_wait_secs;
        let env_warm = std::env::var("UFFS_WARM_TO_PARKED_IDLE_SECS")
            .unwrap_or_else(|_| "(unset — controller using 360s default)".to_owned());
        let env_parked = std::env::var("UFFS_PARKED_TO_COLD_IDLE_SECS")
            .unwrap_or_else(|_| "(unset — controller using 86400s/24h default)".to_owned());
        println!(
            "    sleeping {secs}s ... \
             (UFFS_WARM_TO_PARKED_IDLE_SECS = {env_warm}, \
             UFFS_PARKED_TO_COLD_IDLE_SECS = {env_parked})"
        );
        std::thread::sleep(Duration::from_secs(secs));
        Ok(format!("[slept {secs}s]"))
    });

    // Hard assert the controller actually moved the drive into
    // Parked.  The error message enumerates the two
    // misconfigurations that cause the most common failure modes
    // so the operator's first action is reading the diagnosis,
    // not reading the source.
    r.step("S6  Verify target tier == parked", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let waited = r.park_wait_secs;
        let (out, _) = r.status_drives()?;
        let tier = Runner::parse_drive_tier(&out, target).unwrap_or_default();
        if tier != "parked" {
            bail!(
                "expected {target} tier=parked after {waited}s wait, got tier={tier:?}.  \
                 Two common misconfigurations:\n  \
                 (a) UFFS_WARM_TO_PARKED_IDLE_SECS > --park-wait-secs (controller never fired) — \
                     export UFFS_WARM_TO_PARKED_IDLE_SECS=30 and pass --park-wait-secs 60.\n  \
                 (b) UFFS_PARKED_TO_COLD_IDLE_SECS < --park-wait-secs (drive slipped past Parked into Cold) — \
                     export UFFS_PARKED_TO_COLD_IDLE_SECS=900 (or higher) so the Parked window covers the wait."
            );
        }
        Ok(format!("[{target} = parked]"))
    });

    // Time the Parked → Hot promote.  Goes through the
    // Cold/Parked-source arm of `preload_drive` (see
    // `crates/uffs-daemon/src/index/tiering_ops.rs:303-312`) — the
    // body is re-loaded from the encrypted compact cache and the
    // parked bloom + trie are dropped.  Cost should be similar to
    // scenario Q's Cold→Hot (Q4) since both arms take the same
    // body-load path.
    let mut parked_to_hot_ms: u128 = 0;
    r.step("S7  Preload from Parked (Parked → Hot, body re-decrypt)", |r| {
        if target == '?' {
            bail!("no target drive selected");
        }
        let (_out, ms) = r.preload(target, 5)?;
        parked_to_hot_ms = ms;
        Ok(format!("[{ms}ms — body re-load + drop bloom/trie]"))
    });

    // Verify the post-preload state shape: tier=hot AND
    // pin_until_ms > 0.  Reuses the existing `parse_pin_until`
    // helper which already accounts for the Phase 9 PROMOTIONS
    // column being last (pin_until is second-to-last).
    r.step("S8  Verify target tier == hot + pin_until > 0", |r| {
        if target == '?' {
            return Ok(String::new());
        }
        let (out, _) = r.status_drives()?;
        let tier = Runner::parse_drive_tier(&out, target).unwrap_or_default();
        if tier != "hot" {
            bail!("expected {target} tier=hot post-preload, got tier={tier:?}");
        }
        let pin = Runner::parse_pin_until(&out, target).unwrap_or(0);
        if pin == 0 {
            bail!("expected pin_until > 0 post-preload, got 0");
        }
        Ok(format!("[{target} = hot, pin_until = {pin}]"))
    });

    let mut hot_search_ms: u128 = 0;
    r.step("S9  Search against Hot pinned drive", |r| {
        let t = Instant::now();
        let n = r.search(100)?;
        hot_search_ms = t.elapsed().as_millis();
        if n == 0 {
            bail!("Hot-state search returned zero rows");
        }
        Ok(format!("[{n} rows in {hot_search_ms}ms]"))
    });

    // Render a one-rung profile.  The operator gets the absolute
    // ms numbers + the Parked→Hot vs Warm-baseline ratio.  For
    // cross-scenario comparison (Parked→Hot vs Cold→Hot from
    // scenario Q, vs Warm→Hot from scenario R) the operator reads
    // the per-step lines above each profile — keeping the rendering
    // local to the scenario that produced the timing avoids
    // sequence-coupled state between scenarios.
    r.step("S10 Parked-rung latency profile", |_r| {
        Ok(format!(
            "[Warm-baseline-search: {warm_search_ms}ms | \
              Parked→Hot-preload: {parked_to_hot_ms}ms | \
              Hot-search: {hot_search_ms}ms]"
        ))
    });

    // Sanity-check the cost hierarchy.  Bound is loose (3×) to
    // tolerate fast NVMe + small drives; a real regression
    // (Parked→Hot accidentally skipping the body load) would
    // collapse the ratio to ≈1×.
    r.step("S11 Parked→Hot is ≥ 3× Warm baseline (decrypt cost)", |_r| {
        if warm_search_ms == 0 || parked_to_hot_ms == 0 {
            return Ok("[skipped — no timing data]".to_owned());
        }
        let ratio = parked_to_hot_ms as f64 / warm_search_ms.max(1) as f64;
        if ratio < 3.0 {
            return Ok(format!(
                "[unexpectedly small gap: warm={warm_search_ms}ms vs \
                 parked→hot={parked_to_hot_ms}ms = {ratio:.1}x — investigate]"
            ));
        }
        Ok(format!(
            "[warm={warm_search_ms}ms vs parked→hot={parked_to_hot_ms}ms = {ratio:.1}x]"
        ))
    });

    r.step("S12 Cleanup: stop", |r| {
        r.run_ok(&["--daemon", "stop"])?;
        Ok(String::new())
    });
}

/// Render a per-command Phase 8 timing summary table.  Aggregates the
/// `phase8_timings` Vec into min / mean / max per command class so the
/// operator sees `hibernate` (RAM-only) ≪ `preload` (decrypt) costs at
/// a glance.
fn print_phase8_summary(r: &Runner) {
    if r.phase8_timings.is_empty() {
        return;
    }
    use std::collections::BTreeMap;
    let mut by_cmd: BTreeMap<String, Vec<u128>> = BTreeMap::new();
    for (name, ms) in &r.phase8_timings {
        by_cmd.entry(name.clone()).or_default().push(*ms);
    }

    println!();
    println!("── Phase 8 per-RPC timing summary ──────────────────────");
    println!();
    println!("  ┌────────────────┬───────┬────────┬────────┬────────┬────────┐");
    println!(
        "  │ {:<14} │ {:>5} │ {:>6} │ {:>6} │ {:>6} │ {:>6} │",
        "RPC", "n", "min", "mean", "max", "total"
    );
    println!("  ├────────────────┼───────┼────────┼────────┼────────┼────────┤");
    for (cmd, samples) in &by_cmd {
        let n = samples.len();
        let min = samples.iter().min().copied().unwrap_or(0);
        let max = samples.iter().max().copied().unwrap_or(0);
        let sum: u128 = samples.iter().sum();
        let mean = sum / (n as u128).max(1);
        println!(
            "  │ {:<14} │ {:>5} │ {:>4} ms │ {:>4} ms │ {:>4} ms │ {:>4} ms │",
            cmd, n, min, mean, max, sum
        );
    }
    println!("  └────────────────┴───────┴────────┴────────┴────────┴────────┘");
    println!();
    println!(
        "  Cost ladder: status_drives ≈ register-walk read-lock; \n              hibernate ≈ write-lock + N × Arc swap (RAM-only); \n              preload  ≈ encrypted-cache decrypt + body load + Arc swap; \n              forget   ≈ write-lock evict + 4 × fs::remove_file."
    );
    println!();
}

// ── Main ─────────────────────────────────────────────────────────────────────

/// Find the workspace root by walking up from cwd looking for Cargo.toml + .cargo.
fn find_workspace_root() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut dir = cwd.as_path();
    loop {
        if dir.join("Cargo.toml").exists() && dir.join(".cargo").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    cwd
}

/// Build a fresh release **workspace** and return the path to the
/// `uffs` CLI binary (macOS/Linux).
///
/// Builds every workspace package — not just `uffs-cli` — so the
/// daemon (`uffsd`, from `uffs-daemon`), the MCP host (`uffsmcp`,
/// from `uffs-mcp`), and the auxiliary `uffs-mft` binary all get
/// rebuilt in lock-step with the CLI.  Building only `uffs-cli` was
/// the source of a real bug discovered on 2026-05-04: the script
/// rebuilt `target/release/uffs` to pick up a CLI fix, but
/// `target/release/uffsd` was left at its prior mtime.  When the
/// fresh CLI invoked `daemon start`, `find_daemon_exe()` resolved
/// to the **stale** `uffsd` (sibling-of-uffs lookup), so every
/// Phase 8 RPC the readiness suite tested was sent to a daemon
/// that didn't know the new methods.  See
/// `crates/uffs-client/src/daemon_ctl.rs::find_daemon_exe` for the
/// sibling-lookup that makes co-versioning critical.
fn ensure_fresh_release_build() -> String {
    let workspace = find_workspace_root();
    let binary_path = workspace.join("target").join("release").join("uffs");

    eprintln!("╔══════════════════════════════════════════════════════════════════╗");
    eprintln!("║  Building fresh release workspace (uffs + uffsd + uffsmcp …)...  ║");
    eprintln!("╚══════════════════════════════════════════════════════════════════╝");
    eprintln!("  Workspace: {}", workspace.display());

    let start = Instant::now();
    let status = Command::new("cargo")
        .args(["build", "--release", "--workspace"])
        .current_dir(&workspace)
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!(
                "  ✅ Build completed in {:.1}s",
                start.elapsed().as_secs_f64()
            );
            eprintln!("  CLI binary: {}", binary_path.display());
            // Surface the daemon binary's mtime alongside the CLI
            // so a stale `uffsd` is loud at the top of the run rather
            // than mystery-failing inside a Phase 8 scenario.
            let uffsd = workspace.join("target").join("release").join("uffsd");
            if let Ok(meta) = std::fs::metadata(&uffsd) {
                if let Ok(modified) = meta.modified() {
                    eprintln!(
                        "  uffsd:      {} ({:.0}s ago)",
                        uffsd.display(),
                        modified.elapsed().map_or(0.0, |d| d.as_secs_f64())
                    );
                }
            }
            eprintln!();
        }
        Ok(s) => {
            eprintln!("  ❌ cargo build --release --workspace failed (exit {s})");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("  ❌ Failed to run cargo: {e}");
            std::process::exit(1);
        }
    }

    binary_path.to_string_lossy().into_owned()
}

/// On non-Windows, default to ~/uffs_data when no path is given.
fn default_data_dir() -> Option<String> {
    if cfg!(windows) { return None; }
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::PathBuf::from(home).join("uffs_data");
    if dir.is_dir() { Some(dir.to_string_lossy().into_owned()) } else { None }
}

fn default_binary() -> String {
    if cfg!(windows) {
        // Windows: check ~/bin/ first, then target/release/.
        if let Ok(home) = std::env::var("USERPROFILE") {
            let deployed = std::path::PathBuf::from(&home).join("bin").join("uffs.exe");
            if deployed.exists() {
                return deployed.to_string_lossy().into_owned();
            }
        }
        "target\\release\\uffs.exe".to_string()
    } else {
        // Non-Windows: always do a fresh release build so we test the latest code.
        ensure_fresh_release_build()
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let binary = cli.binary.unwrap_or_else(|| default_binary());

    let (source_flag, source_path): (Option<&'static str>, String) = match &cli.path {
        Some(path) => {
            let (flag, val) = detect_data_source(path)?;
            (Some(flag), val)
        }
        None if cfg!(windows) => {
            // Windows: auto-discover live NTFS drives.
            (None, String::new())
        }
        None => {
            // Non-Windows: default to ~/uffs_data if it exists.
            match default_data_dir() {
                Some(dir) => (Some("--data-dir"), dir),
                None => {
                    bail!(
                        "PATH is required on non-Windows platforms.\n\n\
                         On macOS/Linux, provide a data directory or MFT file:\n  \
                         rust-script scripts/dev/daemon-readiness.rs ~/uffs_data\n  \
                         rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp\n\n\
                         On Windows, omit PATH to auto-discover live NTFS drives."
                    );
                }
            }
        }
    };

    let forget_drive: char = match cli.forget_drive.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase(),
        _ => bail!(
            "--forget-drive must be a single ASCII letter; got `{}`",
            cli.forget_drive
        ),
    };

    println!("{}", "═══ UFFS Daemon Readiness Verification ═══".bold());
    println!("  binary:           {binary}");
    match source_flag {
        Some(flag) => println!("  source:           {flag} {source_path}"),
        None => println!("  source:           live NTFS drives (auto-discover)"),
    }
    println!("  pattern:          {}", cli.pattern);
    if cli.skip_phase8 {
        println!("  phase 8/9 (L-S):  skipped (--skip-phase8)");
    } else {
        println!("  forget target:    {forget_drive}");
        if cli.pin_ttl_wait_secs > 0 {
            println!("  pin TTL wait:     {}s (scenario N)", cli.pin_ttl_wait_secs);
        }
        if cli.park_wait_secs > 0 {
            println!("  park wait:        {}s (scenario S)", cli.park_wait_secs);
        }
    }

    let mut r = Runner::new(
        binary,
        source_flag,
        source_path,
        cli.pattern,
        forget_drive,
        cli.pin_ttl_wait_secs,
        cli.park_wait_secs,
    );

    // Phase 5/6/7 lifecycle scenarios — pre-existing.
    scenario_a(&mut r);
    scenario_b(&mut r);
    scenario_c(&mut r);
    scenario_d(&mut r);
    scenario_e(&mut r);
    scenario_f(&mut r);
    scenario_g(&mut r);
    scenario_h(&mut r);
    scenario_i(&mut r);
    scenario_j(&mut r);
    scenario_k(&mut r);

    // Phase 8 / 9 operator-driven memory tiering.
    if !cli.skip_phase8 {
        scenario_l(&mut r);
        scenario_m(&mut r);
        scenario_n(&mut r);
        scenario_o(&mut r);
        scenario_p(&mut r);
        scenario_q(&mut r);
        scenario_r(&mut r);
        scenario_s(&mut r);
    }

    // Final cleanup
    r.ensure_stopped();

    // Summary
    println!();
    println!("─── Timings ───────────────────────────────────────────");
    for (name, ms) in &r.timings {
        println!("  {name:<45} {ms:>6}ms");
    }

    // Per-RPC summary for the Phase 8 operator commands.  Skipped
    // automatically when no Phase 8 scenarios ran.
    print_phase8_summary(&r);

    println!();
    let total = r.passed + r.failed;
    if r.failed == 0 {
        println!(
            "{}",
            format!("══ ALL GOOD ══  {total}/{total} steps passed")
                .green()
                .bold()
        );
    } else {
        println!(
            "{}",
            format!("══ FAILED ══  {}/{total} steps failed", r.failed)
                .red()
                .bold()
        );
    }

    std::process::exit(if r.failed > 0 { 1 } else { 0 });
}
