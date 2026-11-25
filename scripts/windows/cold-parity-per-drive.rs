#!/usr/bin/env rust-script
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//! Per-drive parity benchmark: UFFS (Rust daemon HOT) vs UFFS (C++ MFT re-read)
//!
//! # Methodology
//!
//!   Both tools answer the same `*` (all-files, path-only) query per drive:
//!
//!   - **UFFS Rust** (`uffs.exe`): daemon model.  The daemon loads all drives
//!     once and serves each per-drive `*` query from the in-memory index.
//!     Each round is HOT (daemon already warm for every drive before the loop).
//!
//!   - **UFFS C++** (`uffs.com`): no daemon.  Re-reads the MFT on every
//!     invocation regardless of `--drives=X`.  The `--drives` flag is an
//!     *output filter*, not a load-time filter — each round is effectively COLD.
//!
//!   The per-drive table therefore measures the real-world workflow difference:
//!   how fast can each tool answer an interactive `*` query on a single drive?
//!
//! # Sequence
//!
//!   1. Kill the running daemon and restart it with the requested `--drives`
//!      (or with no filter so it auto-discovers all system drives when none
//!      are given).  This ensures every run starts from a known COLD state.
//!      If `--purge-cache` is also set the on-disk cache files are deleted
//!      before the restart, forcing a true MFT re-read on first load.
//!   2. Warm-up: issue `uffs * --limit 1 --profile` **three times** so the
//!      daemon moves through its load path (pass 1) then fully primes its
//!      JIT query state (passes 2-3).  Pass-1 wall + `await_ready` reported.
//!   3. Per-drive loop (`--rounds` iterations each):
//!        UFFS Rust HOT:  `uffs.exe '*' --drive X --out <tmp> --columns Path
//!                         --profile`
//!        C++ MFT-reread: `uffs.com '*' --drives=X --columns=path --out=<tmp>`
//!      Report p50 wall-clock + daemon-ms (from `--profile`) per drive.
//!      `--hide-system` / `--hide-ads` are intentionally **omitted**: those
//!      flags exist only to align row counts with Everything (which skips
//!      system files and ADS).  This benchmark is uffs.exe vs uffs.com only
//!      — the full unfiltered MFT corpus is the correct baseline.
//!      `--profile` is kept: it prints `daemon: N ms` on stderr so Table 2
//!      can break the wall-clock into daemon-search vs CLI overhead, giving
//!      insight into the daemon IPC cost.  It is a non-default code path but
//!      the extra `SearchProfile` payload is small and does not materially
//!      change wall-clock at the >100 ms scale of these queries.
//!   4. Two markdown summary tables.
//!
//! # Binary resolution (same cascade as the bench suite)
//!
//!   1. Explicit `--uffs-bin` / `--cpp-bin`
//!   2. `%USERPROFILE%\bin\uffs.exe` / `%USERPROFILE%\bin\uffs.com`
//!   3. `target\release\uffs.exe`  (Rust only)
//!   4. bare name on PATH (OS errors clearly if absent)
//!
//!   If a required binary is not found an actionable error with the download
//!   URL is printed and the script exits non-zero.
//!
//! # Usage
//!
//! ```powershell
//! rust-script scripts\windows\cold-parity-per-drive.rs --drives C,D,G
//! rust-script scripts\windows\cold-parity-per-drive.rs --drives C,D --rounds 3
//! rust-script scripts\windows\cold-parity-per-drive.rs --purge-cache
//! rust-script scripts\windows\cold-parity-per-drive.rs --skip-cpp
//! rust-script scripts\windows\cold-parity-per-drive.rs --dump-raw
//! rust-script scripts\windows\cold-parity-per-drive.rs --uffs-bin C:\tools\uffs.exe
//! ```
//!
//! ```cargo
//! [dependencies]
//! ```

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

// ── Constants ─────────────────────────────────────────────────────────────────

const UFFS_DOWNLOAD_URL: &str =
    "https://github.com/skyllc-ai/UltraFastFileSearch/releases/latest";
const CPP_DOWNLOAD_URL: &str =
    "https://github.com/githubrobbi/Ultra-Fast-File-Search/releases/latest";

// ── CLI ───────────────────────────────────────────────────────────────────────

struct Args {
    drives: Vec<String>,
    uffs_bin: Option<String>,
    cpp_bin: Option<String>,
    rounds: usize,
    sleep_ms: u64,
    purge_cache: bool,
    skip_cpp: bool,
    dump_raw: bool,
    output_file: Option<String>,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = env::args().skip(1).collect();
        let mut a = Args {
            drives: vec!["C".into(), "D".into()],
            uffs_bin: None,
            cpp_bin: None,
            rounds: 1,
            sleep_ms: 1_000,
            purge_cache: false,
            skip_cpp: false,
            dump_raw: false,
            output_file: None,
        };
        let mut i = 0usize;
        while i < raw.len() {
            match raw[i].as_str() {
                "--drives" => {
                    i += 1;
                    if i < raw.len() {
                        a.drives = raw[i]
                            .split(',')
                            .map(|d| d.trim().to_uppercase())
                            .filter(|d| !d.is_empty())
                            .collect();
                    }
                }
                "--uffs-bin" => {
                    i += 1;
                    if i < raw.len() {
                        a.uffs_bin = Some(raw[i].clone());
                    }
                }
                "--cpp-bin" => {
                    i += 1;
                    if i < raw.len() {
                        a.cpp_bin = Some(raw[i].clone());
                    }
                }
                "--rounds" => {
                    i += 1;
                    if i < raw.len() {
                        a.rounds = raw[i].parse().unwrap_or(1).max(1);
                    }
                }
                "--sleep-ms" => {
                    i += 1;
                    if i < raw.len() {
                        a.sleep_ms = raw[i].parse().unwrap_or(1_000);
                    }
                }
                "--output-file" => {
                    i += 1;
                    if i < raw.len() {
                        a.output_file = Some(raw[i].clone());
                    }
                }
                "--purge-cache" => a.purge_cache = true,
                "--skip-cpp" => a.skip_cpp = true,
                "--dump-raw" => a.dump_raw = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("Unknown argument: {other}  (use --help)");
                    std::process::exit(1);
                }
            }
            i += 1;
        }
        a
    }
}

fn print_help() {
    println!(
        "cold-parity-per-drive — UFFS Rust (daemon HOT) vs UFFS C++ (MFT re-read)\n\
         \n\
         Usage:  rust-script cold-parity-per-drive.rs [OPTIONS]\n\
         \n\
         Options:\n\
           --drives C,D,G       Comma-separated drives to test (default: C,D)\n\
           --rounds N           Rounds per drive per tool (default: 1)\n\
           --uffs-bin <path>    Explicit path to uffs.exe\n\
           --cpp-bin  <path>    Explicit path to uffs.com\n\
           --purge-cache        Stop daemon + purge all cache files before warm-up\n\
           --skip-cpp           Skip the C++ reference column entirely\n\
           --dump-raw           Print raw stderr for each invocation\n\
           --sleep-ms N         Sleep between rounds in ms (default: 1000)\n\
           --output-file <path> Tee all output to a file\n\
           --help               Show this help\n\
         \n\
         Requires admin elevation for MFT reads (C++ column)."
    );
}

// ── Binary resolution ─────────────────────────────────────────────────────────

/// Returns `(resolved_path, source_description)`.
fn resolve_uffs(explicit: Option<&str>) -> (String, String) {
    if let Some(e) = explicit {
        return (e.to_owned(), format!("explicit --uffs-bin ({e})"));
    }
    let bin = "uffs.exe";
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Ok(home) = env::var(home_var) {
        let p = PathBuf::from(&home).join("bin").join(bin);
        if p.exists() {
            return (p.to_string_lossy().into_owned(), format!("%USERPROFILE%\\bin ({})", p.display()));
        }
    }
    let tgt = PathBuf::from("target").join("release").join(bin);
    if tgt.exists() {
        return (
            tgt.to_string_lossy().into_owned(),
            format!("target\\release ({})", tgt.display()),
        );
    }
    (
        bin.to_owned(),
        format!("unresolved (tried: explicit, %USERPROFILE%\\bin\\{bin}, target\\release, PATH)"),
    )
}

/// Returns `(resolved_path, source_description)`.
fn resolve_cpp(explicit: Option<&str>) -> (String, String) {
    if let Some(e) = explicit {
        return (e.to_owned(), format!("explicit --cpp-bin ({e})"));
    }
    let bin = "uffs.com";
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Ok(home) = env::var(home_var) {
        let p = PathBuf::from(&home).join("bin").join(bin);
        if p.exists() {
            return (p.to_string_lossy().into_owned(), format!("%USERPROFILE%\\bin ({})", p.display()));
        }
    }
    (
        bin.to_owned(),
        format!("unresolved (tried: explicit, %USERPROFILE%\\bin\\{bin}, PATH)"),
    )
}

/// Extract the version string from uffs.com `--version` output.
/// The C++ binary prints several lines; the one starting with
/// `\tUFFS version:` (after trimming) holds the actual version number.
fn extract_cpp_version(raw: &str) -> String {
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("UFFS version:") {
            return format!("uffs.com {}", rest.trim());
        }
    }
    // Fallback: first non-empty line.
    raw.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("uffs.com (version unknown)")
        .to_owned()
}

fn binary_available(bin: &str) -> bool {
    if PathBuf::from(bin).exists() {
        return true;
    }
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn print_missing_uffs_error(path: &str, source: &str) {
    eprintln!();
    eprintln!("ERROR: uffs.exe not found.");
    eprintln!("       Resolved  : {path}");
    eprintln!("       Source    : {source}");
    eprintln!();
    eprintln!("       Looked in order:");
    eprintln!("         1. --uffs-bin <path> (not provided)");
    eprintln!("         2. %USERPROFILE%\\bin\\uffs.exe");
    eprintln!("         3. target\\release\\uffs.exe  (cargo build --release)");
    eprintln!("         4. PATH");
    eprintln!();
    eprintln!("       Download: {UFFS_DOWNLOAD_URL}");
    eprintln!();
}

fn print_missing_cpp_warning(path: &str, source: &str) {
    eprintln!();
    eprintln!("WARNING: uffs.com (C++ reference) not found — C++ column will be skipped.");
    eprintln!("         Resolved  : {path}");
    eprintln!("         Source    : {source}");
    eprintln!("         Looked in order:");
    eprintln!("           1. --cpp-bin <path> (not provided)");
    eprintln!("           2. %USERPROFILE%\\bin\\uffs.com");
    eprintln!("           3. PATH");
    eprintln!("         Download: {CPP_DOWNLOAD_URL}");
    eprintln!();
}

// ── Output sink (console + optional file tee) ─────────────────────────────────

struct Out {
    file: Option<fs::File>,
}

impl Out {
    fn open(path: Option<&str>) -> Self {
        let file = path.and_then(|p| {
            if let Some(parent) = PathBuf::from(p).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            fs::File::create(p)
                .map_err(|e| eprintln!("WARNING: cannot open output file {p}: {e}"))
                .ok()
        });
        Self { file }
    }

    fn line(&mut self, s: &str) {
        println!("{s}");
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{s}");
        }
    }

    fn divider(&mut self, title: &str) {
        let bar = "=".repeat(110);
        self.line("");
        self.line(&bar);
        if !title.is_empty() {
            self.line(&format!("  {title}"));
        }
        self.line(&bar);
        self.line("");
    }
}

// ── Round result ──────────────────────────────────────────────────────────────

struct Round {
    wall_ms: u64,
    daemon_ms: Option<u64>,
    rows: Option<u64>,
}

// ── Cache helpers ─────────────────────────────────────────────────────────────

fn uffs_cache_dir() -> PathBuf {
    env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| {
            env::var("USERPROFILE").map(|h| {
                PathBuf::from(h)
                    .join("AppData")
                    .join("Local")
            })
        })
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("uffs")
        .join("cache")
}

fn purge_drive_cache(cache_dir: &PathBuf, drive: &str, dump_raw: bool) {
    for suffix in &[
        "_compact.uffs",
        "_index.uffs",
        "_index.uffs.tmp",
        "_index.lock",
        "_compact.uffs.tmp",
    ] {
        let p = cache_dir.join(format!("{drive}{suffix}"));
        if p.exists() {
            match fs::remove_file(&p) {
                Ok(()) => {
                    if dump_raw {
                        eprintln!("    - removed {}", p.display());
                    }
                }
                Err(e) => eprintln!("    WARNING: could not remove {}: {e}", p.display()),
            }
        }
    }
}

// ── Daemon helpers ────────────────────────────────────────────────────────────

/// Kill any running daemon and start a fresh one with the given drives.
/// If `drives` is empty the daemon auto-discovers all system drives.
/// Sleeps briefly after kill to let the OS release socket / named-pipe handles.
///
/// The daemon is started with extended idle-demote TTLs so it stays HOT
/// for the entire bench run without spurious Hot→Warm or Warm→Parked demotes:
///   `UFFS_HOT_TO_WARM_IDLE_SECS=3600`   (1 hr — default 10 min)
///   `UFFS_WARM_TO_PARKED_IDLE_SECS=7200` (2 hr — default 30 min)
fn kill_and_restart_daemon(uffs_bin: &str, drives: &[String]) {
    let _ = Command::new(uffs_bin)
        .args(["--daemon", "kill"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    let mut start_args: Vec<&str> = vec!["--daemon", "start"];
    let drive_flags: Vec<String> = drives
        .iter()
        .flat_map(|d| ["--drive".to_owned(), d.clone()])
        .collect();
    for s in &drive_flags {
        start_args.push(s.as_str());
    }
    let _ = Command::new(uffs_bin)
        .args(&start_args)
        .env("UFFS_HOT_TO_WARM_IDLE_SECS", "3600")
        .env("UFFS_WARM_TO_PARKED_IDLE_SECS", "7200")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn daemon_total_records(uffs_bin: &str) -> Option<u64> {
    let out = Command::new(uffs_bin)
        .args(["--daemon", "status", "-v"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.contains("Total records:") {
            let digits: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
            return digits.parse().ok();
        }
    }
    None
}

/// Result of the three-pass daemon warm-up sequence.
struct WarmupResult {
    /// Wall-clock for each of the three passes (seconds).
    pass_wall_sec: [f64; 3],
    /// `Await ready` from pass-1 stderr (ms): daemon spawn + MFT load + index build.
    await_ready_ms: Option<u64>,
    /// `daemon:` search time from each pass's stderr (ms).
    pass_daemon_ms: [Option<u64>; 3],
    /// Total records indexed, read after all passes settle.
    total_records: Option<u64>,
}

/// Prime the daemon with three `uffs * --limit 1 --profile` queries.
///
/// Pass 1 — COLD load path: daemon reads MFT (or compact cache) and builds
///          the in-memory index; `await_ready` measures this.
/// Pass 2 — first HOT query: JIT query structures are built for the first time.
/// Pass 3 — fully primed HOT query: steady-state latency.
///
/// All three wall times are returned so the caller can display the full
/// COLD → WARM → HOT trajectory.
fn warmup_daemon_primed(uffs_bin: &str, dump_raw: bool) -> WarmupResult {
    let mut pass_wall_sec = [0.0f64; 3];
    let mut pass_daemon_ms = [None::<u64>; 3];
    let mut await_ready_ms = None;
    for pass in 0usize..3 {
        let t = Instant::now();
        let result = Command::new(uffs_bin)
            .args(["*", "--limit", "1", "--profile"])
            .output();
        pass_wall_sec[pass] = t.elapsed().as_secs_f64();
        let stderr = match result {
            Ok(ref o) => String::from_utf8_lossy(&o.stderr).into_owned(),
            Err(ref e) => {
                eprintln!("    WARNING: warm-up pass {} failed: {e}", pass + 1);
                continue;
            }
        };
        if dump_raw {
            eprintln!("    [warm-up pass {}]\n{stderr}", pass + 1);
        }
        pass_daemon_ms[pass] = parse_tagged_ms(&stderr, "daemon:");
        if pass == 0 {
            await_ready_ms = parse_tagged_ms(&stderr, "Await ready:");
        }
    }
    let total_records = daemon_total_records(uffs_bin);
    WarmupResult { pass_wall_sec, await_ready_ms, pass_daemon_ms, total_records }
}

fn parse_tagged_ms(text: &str, marker: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(after) = line.split_once(marker).map(|(_, r)| r) {
            let digits: String = after
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                return digits.parse().ok();
            }
        }
    }
    None
}

// ── File line counter ─────────────────────────────────────────────────────────

fn count_lines(path: &PathBuf) -> Option<u64> {
    let bytes = fs::read(path).ok()?;
    Some(bytes.iter().filter(|&&b| b == b'\n').count() as u64)
}

// ── Unique temp path ──────────────────────────────────────────────────────────

fn tmp_path(prefix: &str, drive: &str) -> PathBuf {
    // Transient comparison CSVs (written → line-counted → deleted) live under
    // the shared bench tree's scratch dir, not scattered across $TMPDIR.
    let scratch = shared_bench_root().join("scratch");
    let _ = fs::create_dir_all(&scratch);
    scratch.join(format!(
        "uffs_{prefix}_{drive}_{}.csv",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ))
}

/// Resolve the consolidated bench-artifact root, mirroring the `_bench-dir`
/// helper in `just/bench_uffs.just` and the other bench scripts so every tool
/// writes under ONE tree.  Precedence: `$UFFS_BENCH_DIR` >
/// `%LOCALAPPDATA%\uffs-bench` > `$XDG_CACHE_HOME|~/.cache` + `/uffs-bench`.
fn shared_bench_root() -> PathBuf {
    if let Ok(v) = env::var("UFFS_BENCH_DIR") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(v) = env::var("LOCALAPPDATA") {
        if !v.is_empty() {
            return PathBuf::from(v).join("uffs-bench");
        }
    }
    let base = env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cache")
        });
    base.join("uffs-bench")
}

// ── Round runners ─────────────────────────────────────────────────────────────

fn run_uffs_hot(uffs_bin: &str, drive: &str, dump_raw: bool) -> Round {
    let out_path = tmp_path("hot", drive);
    let t = Instant::now();
    let result = Command::new(uffs_bin)
        .args([
            "*",
            "--drive",
            drive,
            "--out",
            out_path.to_str().unwrap_or("out.csv"),
            "--columns",
            "Path",
            "--profile",
        ])
        .output();
    let wall_ms = t.elapsed().as_millis() as u64;
    let (daemon_ms, stderr) = match result {
        Ok(ref o) => {
            let s = String::from_utf8_lossy(&o.stderr).into_owned();
            (parse_tagged_ms(&s, "daemon:"), s)
        }
        Err(ref e) => {
            eprintln!("    WARNING: uffs.exe failed: {e}");
            (None, String::new())
        }
    };
    if dump_raw && !stderr.is_empty() {
        eprintln!("{stderr}");
    }
    let rows = count_lines(&out_path);
    let _ = fs::remove_file(&out_path);
    Round { wall_ms, daemon_ms, rows }
}

fn run_cpp(cpp_bin: &str, drive: &str, dump_raw: bool) -> Round {
    let out_path = tmp_path("cpp", drive);
    // C++ binary uses freopen() internally — must NOT capture stdout/stderr via
    // pipe; inherit both so the internal redirect onto --out= works correctly.
    let t = Instant::now();
    let status = Command::new(cpp_bin)
        .args([
            "*",
            &format!("--drives={drive}"),
            "--columns=path",
            &format!("--out={}", out_path.display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    let wall_ms = t.elapsed().as_millis() as u64;
    if dump_raw {
        let code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        eprintln!("    [cpp exit: {code}]");
    }
    let rows = count_lines(&out_path);
    let _ = fs::remove_file(&out_path);
    Round { wall_ms, daemon_ms: None, rows }
}

// ── Statistics ────────────────────────────────────────────────────────────────

fn p50(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut s = values.to_vec();
    s.sort_unstable();
    let mid = s.len() / 2;
    if s.len() % 2 == 1 {
        s[mid]
    } else {
        (s[mid - 1] + s[mid]) / 2
    }
}

// ── Formatting ────────────────────────────────────────────────────────────────

fn fmt_count(n: u64) -> String {
    // Insert thousands separators.
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn opt_count(n: Option<u64>) -> String {
    n.map(fmt_count).unwrap_or_else(|| "n/a".to_owned())
}

fn opt_ms(n: Option<u64>) -> String {
    n.map(|v| format!("{v} ms")).unwrap_or_else(|| "n/a".to_owned())
}

/// Format a millisecond value as `"N ms"` with thousands-separated N.
fn ms_str(ms: u64) -> String {
    format!("{} ms", fmt_count(ms))
}

/// Format a speedup ratio as `"N.Nx"`, or `"n/a"` / `"(skipped)"`.
fn speedup_str(cpp_ms: Option<u64>, rust_ms: u64) -> String {
    match cpp_ms {
        Some(c) if rust_ms > 0 => format!("{:.1}x", c as f64 / rust_ms as f64),
        Some(_) => "n/a".to_owned(),
        None => "(skipped)".to_owned(),
    }
}

fn now_str() -> String {
    // No chrono dep: use the OS time via a simple epoch calculation.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (s, m, h, d, mo, y) = epoch_to_ymd_hms(secs);
    format!("{y}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

fn epoch_to_ymd_hms(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86_400;
    // Tomohiko Sakamoto's algorithm (Jan 1 1970 = day 0).
    let mut y = 1970u64;
    let mut remaining = days;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let days_in_year = if leap { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days: [u64; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1u64;
    for days_in_month in month_days {
        if remaining < days_in_month {
            break;
        }
        remaining -= days_in_month;
        mo += 1;
    }
    (s, m, h, remaining + 1, mo, y)
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args = Args::parse();

    // Binary resolution.
    let (uffs_bin, uffs_src) = resolve_uffs(args.uffs_bin.as_deref());
    let (cpp_bin, cpp_src) = resolve_cpp(args.cpp_bin.as_deref());

    let mut out = Out::open(args.output_file.as_deref());

    // ── Preflight header ───────────────────────────────────────────────────
    out.divider(&format!("UFFS per-drive parity benchmark  —  {}", now_str()));
    out.line(&format!("  uffs.exe     : {uffs_bin}"));
    out.line(&format!("  uffs.exe src : {uffs_src}"));
    if args.skip_cpp {
        out.line("  uffs.com     : (--skip-cpp)");
    } else {
        out.line(&format!("  uffs.com     : {cpp_bin}"));
        out.line(&format!("  uffs.com src : {cpp_src}"));
    }
    out.line(&format!("  drives       : {}", args.drives.join(", ")));
    out.line(&format!("  rounds/drive : {}", args.rounds));
    out.line(&format!("  purge-cache  : {}", args.purge_cache));
    out.line(&format!("  cache dir    : {}", uffs_cache_dir().display()));
    out.line("");

    // Verify uffs.exe (required).
    if !binary_available(&uffs_bin) {
        print_missing_uffs_error(&uffs_bin, &uffs_src);
        std::process::exit(1);
    }
    if let Ok(o) = Command::new(&uffs_bin).arg("--version").output() {
        let ver = String::from_utf8_lossy(&o.stdout);
        let ver = ver.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        out.line(&format!("  {ver}"));
    }

    // Verify uffs.com (optional).
    let cpp_available = !args.skip_cpp && {
        let avail = binary_available(&cpp_bin);
        if !avail {
            print_missing_cpp_warning(&cpp_bin, &cpp_src);
        } else if let Ok(o) = Command::new(&cpp_bin).arg("--version").output() {
            let raw = String::from_utf8_lossy(&o.stdout);
            out.line(&format!("  {}", extract_cpp_version(&raw)));
        }
        avail
    };

    // ── Phase 0a: kill + (optional cache purge) + restart daemon ──────────
    {
        let drives_label = if args.drives.is_empty() {
            "(auto-discover all)".to_owned()
        } else {
            args.drives.join(", ")
        };
        let phase_title = if args.purge_cache {
            format!("Phase 0a: kill + purge caches + restart  [{drives_label}]  (true COLD)")
        } else {
            format!("Phase 0a: kill + restart daemon  [{drives_label}]")
        };
        out.divider(&phase_title);
        if args.purge_cache {
            out.line("  Killing daemon …");
            let cdir = uffs_cache_dir();
            for drive in &args.drives {
                out.line(&format!("  Purging cache for {drive}: …"));
                purge_drive_cache(&cdir, drive, args.dump_raw);
            }
        } else {
            out.line("  Killing daemon …");
        }
        kill_and_restart_daemon(&uffs_bin, &args.drives);
        let drives_hint = if args.drives.is_empty() {
            "all drives (auto-discovered)"
        } else {
            "requested drives"
        };
        out.line(&format!("  Daemon restarted with {drives_hint}."));
        std::thread::sleep(std::time::Duration::from_millis(args.sleep_ms));
    }

    // ── Phase 0b: daemon warm-up (3 priming passes) ────────────────────────
    out.divider("Phase 0b: daemon warm-up — 3 priming passes");
    out.line("  Pass 1 = COLD load (MFT read / cache hit + await_ready)");
    out.line("  Pass 2 = WARM (first HOT query, JIT structures built)");
    out.line("  Pass 3 = HOT  (fully primed, steady-state latency)");
    out.line("  Running `uffs '*' --limit 1 --profile`  ×3 …");
    let wu = warmup_daemon_primed(&uffs_bin, args.dump_raw);
    let pass_labels = ["COLD", "WARM", "HOT "];
    for i in 0..3 {
        out.line(&format!(
            "    [pass {} — {}]  wall = {:.2} s   daemon = {}",
            i + 1,
            pass_labels[i],
            wu.pass_wall_sec[i],
            opt_ms(wu.pass_daemon_ms[i]),
        ));
    }
    out.line(&format!(
        "  await_ready (pass 1) = {}   total_records = {}",
        opt_ms(wu.await_ready_ms),
        opt_count(wu.total_records),
    ));
    out.line(&format!(
        "  Mode: {}",
        if args.purge_cache {
            "COLD — daemon restarted from scratch, cache files purged"
        } else {
            "COLD — daemon restarted (on-disk cache retained for faster load)"
        }
    ));
    std::thread::sleep(std::time::Duration::from_millis(args.sleep_ms));

    // ── Phase 1: per-drive loop ────────────────────────────────────────────

    struct DriveRow {
        drive: String,
        rust_p50: u64,
        rust_daemon_p50: Option<u64>,
        rust_rows: Option<u64>,
        cpp_p50: Option<u64>,
        cpp_rows: Option<u64>,
    }

    let mut table: Vec<DriveRow> = Vec::new();

    for drive in &args.drives {
        out.divider(&format!(
            "Drive {drive}:  — {} round(s) per tool",
            args.rounds
        ));

        // Rust HOT rounds.
        let mut rust_walls: Vec<u64> = Vec::new();
        let mut rust_daemons: Vec<u64> = Vec::new();
        let mut rust_rows_r1: Option<u64> = None;

        for r in 1..=args.rounds {
            out.line(&format!(
                "  [Rust HOT {r}/{}]  uffs.exe '*' --drive {drive} \
                 --out <tmp> --columns Path --profile",
                args.rounds
            ));
            let res = run_uffs_hot(&uffs_bin, drive, args.dump_raw);
            out.line(&format!(
                "    -> wall = {} ms   daemon = {}   rows = {}",
                res.wall_ms,
                opt_ms(res.daemon_ms),
                opt_count(res.rows),
            ));
            if r == 1 {
                rust_rows_r1 = res.rows;
            }
            rust_walls.push(res.wall_ms);
            if let Some(dm) = res.daemon_ms {
                rust_daemons.push(dm);
            }
            std::thread::sleep(std::time::Duration::from_millis(args.sleep_ms));
        }

        let rust_p50 = p50(&rust_walls);
        let rust_daemon_p50 = if rust_daemons.is_empty() {
            None
        } else {
            Some(p50(&rust_daemons))
        };

        // C++ MFT-reread rounds.
        let mut cpp_walls: Vec<u64> = Vec::new();
        let mut cpp_rows_r1: Option<u64> = None;

        if cpp_available {
            for r in 1..=args.rounds {
                out.line(&format!(
                    "  [C++ MFT-reread {r}/{}]  uffs.com '*' --drives={drive} \
                     --columns=path --out=<tmp>",
                    args.rounds
                ));
                let res = run_cpp(&cpp_bin, drive, args.dump_raw);
                out.line(&format!(
                    "    -> wall = {} ms   rows = {}",
                    res.wall_ms,
                    opt_count(res.rows),
                ));
                if r == 1 {
                    cpp_rows_r1 = res.rows;
                }
                cpp_walls.push(res.wall_ms);
                std::thread::sleep(std::time::Duration::from_millis(args.sleep_ms));
            }
        }

        let cpp_p50 = if cpp_walls.is_empty() {
            None
        } else {
            Some(p50(&cpp_walls))
        };

        table.push(DriveRow {
            drive: drive.clone(),
            rust_p50,
            rust_daemon_p50,
            rust_rows: rust_rows_r1,
            cpp_p50,
            cpp_rows: cpp_rows_r1,
        });
    }

    // ── Pre-compute column widths from data for aligned tables ────────────
    let w_drive   = table.iter().map(|r| r.drive.len() + 1).max().unwrap_or(5).max(5); // "Drive"
    let w_cpp     = table.iter().map(|r| r.cpp_p50.map(|v| ms_str(v).len()).unwrap_or(9)).max().unwrap_or(9).max(17); // "C++ (MFT re-read)"
    let w_rust    = table.iter().map(|r| ms_str(r.rust_p50).len()).max().unwrap_or(9).max(17); // "Rust (daemon HOT)"
    let w_speedup = table.iter().map(|r| speedup_str(r.cpp_p50, r.rust_p50).len()).max().unwrap_or(7).max(7); // "Speedup"
    let w_rrows   = table.iter().map(|r| opt_count(r.rust_rows).len()).max().unwrap_or(9).max(9); // "Rust rows"
    let w_crows   = table.iter().map(|r| opt_count(r.cpp_rows).len()).max().unwrap_or(8).max(8);  // "C++ rows"

    // ── Summary table 1: parity ────────────────────────────────────────────
    out.divider(&format!(
        "Summary — table 1: per-drive parity (wall-clock p50 over {} round(s))",
        args.rounds
    ));

    let h1 = format!("| {:<w_drive$} | {:>w_cpp$} | {:>w_rust$} | {:>w_speedup$} | {:>w_rrows$} | {:>w_crows$} |",
        "Drive", "C++ (MFT re-read)", "Rust (daemon HOT)", "Speedup", "Rust rows", "C++ rows");
    let sep1 = format!("| {:-<w_drive$} | {:->w_cpp$}: | {:->w_rust$}: | {:->w_speedup$}: | {:->w_rrows$}: | {:->w_crows$}: |",
        "", "", "", "", "", "");
    out.line(&h1);
    out.line(&sep1);

    let mut total_rust_ms: u64 = 0;
    let mut total_cpp_ms: u64 = 0;
    let mut any_cpp = false;

    for row in &table {
        total_rust_ms += row.rust_p50;
        let speedup = speedup_str(row.cpp_p50, row.rust_p50);
        let cpp_cell = row
            .cpp_p50
            .map(|c| {
                total_cpp_ms += c;
                any_cpp = true;
                ms_str(c)
            })
            .unwrap_or_else(|| "(skipped)".to_owned());
        out.line(&format!(
            "| {:<w_drive$} | {:>w_cpp$} | {:>w_rust$} | {:>w_speedup$} | {:>w_rrows$} | {:>w_crows$} |",
            format!("{}:", row.drive),
            cpp_cell,
            ms_str(row.rust_p50),
            speedup,
            opt_count(row.rust_rows),
            opt_count(row.cpp_rows),
        ));
    }

    if any_cpp && total_rust_ms > 0 {
        let total_speedup = total_cpp_ms as f64 / total_rust_ms as f64;
        let speedup_label = format!("{total_speedup:.1}x");
        out.line(&format!(
            "| {:<w_drive$} | {:>w_cpp$} | {:>w_rust$} | {:>w_speedup$} | {:>w_rrows$} | {:>w_crows$} |",
            "TOTAL",
            ms_str(total_cpp_ms),
            ms_str(total_rust_ms),
            speedup_label,
            "—",
            "—",
        ));
    } else {
        out.line(&format!(
            "| {:<w_drive$} | {:>w_cpp$} | {:>w_rust$} | {:>w_speedup$} | {:>w_rrows$} | {:>w_crows$} |",
            "TOTAL",
            "—",
            ms_str(total_rust_ms),
            "—",
            "—",
            "—",
        ));
    }

    // ── Summary table 2: Rust daemon breakdown ─────────────────────────────
    let w2_rows  = table.iter().map(|r| opt_count(r.rust_rows).len()).max().unwrap_or(9).max(9);
    let w2_wall  = table.iter().map(|r| ms_str(r.rust_p50).len()).max().unwrap_or(13).max(13);
    let w2_dmn   = table.iter().map(|r| r.rust_daemon_p50.map(|v| ms_str(v).len()).unwrap_or(3)).max().unwrap_or(15).max(15);
    let w2_over  = table.iter().map(|r| r.rust_daemon_p50.map(|dm| ms_str(r.rust_p50.saturating_sub(dm)).len()).unwrap_or(3)).max().unwrap_or(12).max(12);

    out.divider("Summary — table 2: Rust wall vs daemon breakdown");
    let h2 = format!("| {:<w_drive$} | {:>w2_rows$} | {:>w2_wall$} | {:>w2_dmn$} | {:>w2_over$} |",
        "Drive", "Rust rows", "Rust wall p50", "Rust daemon p50", "CLI overhead");
    let sep2 = format!("| {:-<w_drive$} | {:->w2_rows$}: | {:->w2_wall$}: | {:->w2_dmn$}: | {:->w2_over$}: |",
        "", "", "", "", "");
    out.line(&h2);
    out.line(&sep2);
    for row in &table {
        let daemon_cell = row.rust_daemon_p50.map(ms_str).unwrap_or_else(|| "n/a".to_owned());
        let overhead_cell = row.rust_daemon_p50
            .map(|dm| ms_str(row.rust_p50.saturating_sub(dm)))
            .unwrap_or_else(|| "n/a".to_owned());
        out.line(&format!(
            "| {:<w_drive$} | {:>w2_rows$} | {:>w2_wall$} | {:>w2_dmn$} | {:>w2_over$} |",
            format!("{}:", row.drive),
            opt_count(row.rust_rows),
            ms_str(row.rust_p50),
            daemon_cell,
            overhead_cell,
        ));
    }
    out.line("");
    out.line("  Legend:");
    out.line("    Rust wall p50    = process wall-clock from spawn to exit (Rust Instant).");
    out.line("    Rust daemon p50  = daemon-reported search duration (--profile stderr: 'daemon: N ms').");
    out.line("    CLI overhead     = wall − daemon  (process spawn + IPC round-trip + file write).");
    out.line("    C++ has no daemon — its wall-clock IS its total cost (full MFT re-read every run).");
    out.line("    Row counts differ: Rust returns all MFT records; C++ may apply implicit filters.");

    // ── Warm-up recap ──────────────────────────────────────────────────────
    out.divider("Summary — daemon warm-up (Phase 0b)");
    let pass_labels = ["COLD", "WARM", "HOT "];
    let pass_notes  = [
        "daemon load + MFT read / cache hit",
        "first HOT query — JIT structures built",
        "fully primed — steady-state latency",
    ];
    for i in 0..3 {
        out.line(&format!(
            "  Pass {} [{} ]  wall = {:.2} s   daemon = {}   ({})",
            i + 1,
            pass_labels[i],
            wu.pass_wall_sec[i],
            opt_ms(wu.pass_daemon_ms[i]),
            pass_notes[i],
        ));
    }
    out.line(&format!(
        "  Await ready   : {}   total_records = {}",
        opt_ms(wu.await_ready_ms),
        opt_count(wu.total_records),
    ));
    out.line(&format!(
        "  Mode          : {}",
        if args.purge_cache {
            "COLD (daemon restarted, cache purged — true MFT re-read)"
        } else {
            "COLD (daemon restarted, on-disk cache retained for faster load)"
        }
    ));

    out.divider("Done");
    if let Some(path) = &args.output_file {
        println!("Full log written to: {path}");
    }
}
