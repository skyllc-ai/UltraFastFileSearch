<!-- SPDX-License-Identifier: MPL-2.0 -->
<!-- Copyright (c) 2025-2026 SKY, LLC. -->

# Memory-tiering Windows-host validation runbook

**Audience:** operator with shell access to a Windows box that has the
target dataset loaded under the daemon (the 7-drive reference box, or
any equivalent machine with at least 4 indexed NTFS volumes).
**Source-of-truth tracker:** the live progress doc lives in
`docs/refactor/memory-tiering-implementation-plan.md` (gitignored —
local working copy on the implementer's machine).  Use this runbook
when you want the **what to run on Windows** without the
implementation-side context.

**Companion docs (added 2026-05-05 alongside the v0.6.0 readiness pass):**

- [`memory-tiering-readiness-validation-2026-05-05.md`](memory-tiering-readiness-validation-2026-05-05.md) —
  dual-platform capture of the Phase 8 + 9 operator-surface readiness
  pass (150 / 150 scenarios on Mac and Windows).  This is the canonical
  validation evidence for the v0.6.0 cut.
- [`memory-tiering-bake-criteria.md`](memory-tiering-bake-criteria.md) —
  operationalised exit criteria for the one-week `main` bake that
  precedes the v0.6.0 cut.

Section map:

- **§1** — Phase 5 operator gates (G1-G4), ~75 min wall-clock total.
- **§2** — Phase 6 24-h `min_tier = "Warm"` operator soak.
- **§3** — Phase 7 24-h USN-journal continuous-churn operator soak.
- **§4** — Phase 8 operator-command gates (G5-G8), ~20 min wall-clock total.
- **§5** — PR-attachment checklist for the acceptance PR.
- **§6** — Reference captures from the 2026-05-02 v0.5.86 baseline run.

For the long-running §2 + §3 soaks, the operator-friendly entry point is
the `just soak` recipe (see
[`scripts/dev/long-soak.rs`](../../scripts/dev/long-soak.rs) and
[`just/dev.just`](../../just/dev.just)), which automates the daemon
lifecycle, hourly snapshots, and the end-of-soak grep validators that
close each gate.

> **Cross-reference convention.**  External code / docs that point INTO
> this runbook should reference **gate IDs** (`G1`, `G6`, `Phase 6 soak`,
> etc.) — *never* raw section numbers (`§3`, `§5`).  Section numbers
> renumber when new sections land (Phase 7 added 2026-05-05 forced one
> such renumbering).  Gate IDs are stable for the life of the gate.
> When inserting a new section here, search the workspace for `§`
> references that point at this file before bumping any number — and
> prefer rewriting them to anchor / gate-ID style on the way through.

---

## 0. Prerequisites

* Windows 10 1709+ or Windows 11 (the `MEMORY_RESOURCE_NOTIFICATION`
  API has been stable since 1709; older Windows editions degrade to
  the never-fires path documented in `crate::cache::pressure`).
* The daemon binary built from the branch under test, copied to the
  host (or built locally with `cargo build --release -p uffs-daemon`).
* The seven NTFS volumes loaded against the daemon — confirm with the
  Phase-8-E per-drive tier table:
  ```powershell
  uffs --daemon status     # expect: Status: Ready
  uffs --daemon status_drives
  ```
  `daemon status_drives` is the canonical post-Phase-8 view: a fixed-
  width table with `DRIVE / TIER / RESIDENT / QPM / LAST QUERY (ms) /
  PIN UNTIL (ms)` columns, sorted ASCII ascending by drive letter.
  Expect every drive's `TIER` column to be `warm` (default after
  load) — any `parked` / `cold` from the start means the gate setup
  is wrong; bounce the daemon (`uffs --daemon stop` → `uffs --daemon
  start --drives C,D,E,F,G,M,S`).
* Task Manager → **Details** tab → enable the **I/O priority** column
  via column-header → *Select columns…* → check `I/O priority`.  This
  is required for gate **G3** (USN catch-up I/O priority capture).
* PowerShell 5.1 or PowerShell 7 — the snippets below use
  `Start-Process` / `Get-Counter` / `Wait-Event` which exist in both.

---

## 1. Phase 5 operator gates — 4 captures, ~75 minutes wall-clock

> **Retired 2026-10-03 — pressure cascade removed.**  The kernel-`Low`
> → cascade-demote behaviour that G1 and the G4 cascade assertions
> validate no longer exists.  By owner ruling the daemon never forces a
> tier change on memory pressure: `spawn_pressure_subscriber` logs the
> `Pressure transition observed level=…` line and stops there; a shard
> goes Parked → Hot when a search needs it and only the idle TTL ladder
> retires it.  `reason="pressure-cascade"`, `cascade_demote_one_step`
> and `PressureLevel::requires_cascade_demote` are gone.  The captures
> below are kept as the historical record of v0.5.86–v0.6.42; on a
> current build G1 produces the `Pressure transition observed` line and
> **no** `shard.transition` demote lines, and that is the pass
> criterion.  Trigger for the removal: the 2026-10-03 benchmark run on
> a memory-starved host, where the cascade parked all six drives
> 2.5 min after start and turned the next `*.*` into a four-minute MFT
> re-read.

### G1 — Low-pressure stress: kernel notification → cache.pressure → cascade demote

**Duration:** ~5 min wall-clock.
**Plan reference:** `docs/refactor/memory-tiering-implementation-plan.md` §3 Phase 5 line "Stress test: spawn allocator that drains free RAM."

#### Setup

Open two terminals on the same Windows host.  In terminal A start the
daemon with structured logging:

```powershell
$env:RUST_LOG = "uffs_daemon=info,cache.pressure=info,shard.transition=info"
uffs --daemon start --drives C,D,E,F,G,M,S 2>&1 | Tee-Object -FilePath C:\Temp\uffsd-G1.log
```

Wait until `uffs --status` reports `Ready` for every drive.

#### Drive

In terminal B, allocate memory until the kernel publishes
`LowMemoryResourceNotification`.  The simplest portable driver is a
`testlimit64`-style PowerShell loop (no separate binary required).

> **Critical: pages must be touched, not just reserved.**  `New-Object
> byte[] 1073741824` on its own only commits virtual address space
> (Private Bytes climbs) but leaves the pages non-resident (Working Set
> flat), because Windows backs zero-filled pages on first write via the
> demand-zero handler.  The kernel's `LowMemoryResourceNotification`
> fires on **physical-RAM pressure**, not commit-charge pressure, so a
> reserve-only allocator never trips the cascade.  Force commit by
> writing a non-zero byte to every page — `[Array]::Fill($arr, [byte]1)`
> is the fastest portable way (CLR-native; fills 1 GiB in milliseconds).
> If you have Sysinternals installed, `testlimit.exe -d 1024 -c N`
> bypasses .NET entirely and is the canonical tool for this.

```powershell
# Allocate 1 GiB chunks AND commit by touching every page; auto-break on Low.
$alloc = New-Object System.Collections.Generic.List[byte[]]
try {
  while ($true) {
    $arr = New-Object byte[] 1073741824
    [Array]::Fill($arr, [byte]1)            # ← commits every page; the line above only reserves
    $alloc.Add($arr)
    $free = (Get-Counter '\Memory\Available MBytes').CounterSamples[0].CookedValue
    Write-Host ("allocated {0} GiB; free = {1:N0} MiB" -f $alloc.Count, $free)
    if ($free -lt 512) {
      Write-Host "Low-memory zone reached — holding 10 s for the cascade to drain, then releasing."
      Start-Sleep -Seconds 10
      break
    }
    Start-Sleep -Milliseconds 500
  }
} finally {
  $alloc.Clear()
  [GC]::Collect()
  Write-Host "released"
}
```

The kernel typically fires `LowMemoryResourceNotification` once
**Available MBytes** drops below ~256 MB on a 16 GB box (Windows
auto-tunes the threshold to ~32 MB on the lower end and
`PhysicalMemory / 64` on the upper end — see Microsoft Learn's
[CreateMemoryResourceNotification][win32-mem]).

#### Capture (this is what you paste into the PR)

The daemon log (`C:\Temp\uffsd-G1.log`) must show (note: tracing
target names like `cache.pressure` and `shard.transition` set the
filter routing but are **not** rendered in the log message text under
the default formatter — grep on the message text and field names
shown below):

```text
INFO Pressure transition observed level=Low
INFO letter=C from=warm to=parked freed_mb=… last_query_at_ms=… reason="pressure-cascade"
INFO letter=G from=warm to=parked freed_mb=… last_query_at_ms=… reason="pressure-cascade"
…
```

Grep cheat-sheet:

```powershell
Select-String -Path C:\Temp\uffsd-G1.log -Pattern 'Pressure transition|reason="pressure-cascade"' |
    Select-Object -ExpandProperty Line
```

— one cascade line per Warm shard.  The `letter` field's order must
follow oldest-`last_query_at_ms`-first (LRU contract pinned by
`crate::index::tests::lifecycle_hooks::cascade_demote_one_step_picks_lru_warm_and_drains_in_order`
+ `pressure_subscriber_drains_warm_cascade_on_low_and_no_ops_on_high`).

> **Format note** (G4 follow-up, 2026-05-02): the cascade demote
> event was previously emitted twice per shard — once from the
> registry primitive (`reason="demote"`, `letter=` field) and a
> second time from `cascade_demote_one_step` itself (`reason=
> "pressure-cascade"`, `drive=` field, with the "Pressure cascade
> demoted one LRU Warm shard" message text).  The second event was
> redundant and the gap between the two confused log analysis, so
> the discriminator is now in the registry primitive's `reason`
> field directly and the second event is gone.  Old runbooks that
> grepped for `Pressure cascade demoted` will see zero matches —
> use `reason="pressure-cascade"` instead.

When you Ctrl-C the allocator the kernel fires
`HighMemoryResourceNotification`.  The log must show:

```text
INFO Pressure transition observed level=High
```

— and **no further** cascade lines after that timestamp.

[win32-mem]: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-creatememoryresourcenotification

---

### G2 — Working set drops in Task Manager within 5 s of demote

**Duration:** ~3 min wall-clock (re-uses the G1 terminal A log).
**Plan reference:** §3 Phase 5 line "Working set drops in Task Manager within 5 s of demote."

#### Drive

Re-run the G1 allocator loop with Task Manager → *Details* visible.

* Note `uffsd.exe` **Working Set (Memory)** before the allocator starts (`baseline_ws`).
* Press the kernel into firing `Low` (allocator runs).
* Within 5 s of the first `shard.transition` cascade line in
  `uffsd-G1.log`, `uffsd.exe` **Working Set** must drop below
  `baseline_ws / 2` (typical observation: 5341 MB → ~1 200 MB on the
  7-drive reference box).

#### Capture

Two screenshots in the PR description, both with
`Get-Date | Out-Host` visible at the top:

1. Task Manager → *Details* showing `uffsd.exe` `WorkingSet` at the
   peak (just before `Low` fires).
2. Task Manager → *Details* showing `uffsd.exe` `WorkingSet` ≤ 5 s
   after the first cascade line.

The drop is caused by the per-batch `EmptyWorkingSet` call wired in
Phase 5 task 5.4 (`crate::index::transitions::IndexManager::demote_idle_shards`
→ `WorkingSetTrim::trim`).  Mac/Linux ship a no-op stub.

---

### G3 — Background-IO priority during USN catch-up

**Duration:** ~6 min wall-clock (the USN refresh runs every 5 min by default).
**Plan reference:** §3 Phase 5 line "During USN catch-up, Task Manager I/O priority on `uffsd.exe` shows 'Low'."

#### Setup

Force a USN catch-up by reducing the cadence — easier than waiting
the default 5 min between ticks.  In terminal A:

```powershell
$env:UFFS_USN_REFRESH_INTERVAL_SECS = "30"
$env:RUST_LOG = "uffs_daemon=info,shard.refresh=info"
uffs --daemon stop
uffs --daemon start --drives C,D,E,F,G,M,S 2>&1 | Tee-Object -FilePath C:\Temp\uffsd-G3.log
```

The 30-second cadence guarantees a refresh tick within ~1 minute of
each Warm shard becoming visible to the controller.

#### Capture

> **Per-thread, not per-process.**  `BackgroundIoScope` calls
> `SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN)`
> on the **calling thread only** — specifically, on the
> `tokio::task::spawn_blocking` worker that runs the refresh closure
> for one drive.  Windows lowers that thread's **I/O priority** to
> `IoPriorityVeryLow` and **memory priority** to lowest; scheduling
> priority is left unchanged at Normal (Base Priority = 8) by
> design.  The **process-level** I/O priority (the column shown in
> PE's main window or Task Manager Details) stays **Normal**,
> because the daemon's main thread + IPC accept loop are not in
> background mode.  **Do not look at the process-row I/O Priority
> column — it will (correctly) always show Normal.**

Three capture paths, in increasing operator effort:

> **What changes when `THREAD_MODE_BACKGROUND_BEGIN` fires.**  The
> Win32 documentation says "the system lowers the resource scheduling
> priorities of the thread", but the **only** properties that actually
> change in observable Windows APIs are:
>
> * **I/O priority** — drops from `IoPriorityNormal` to
>   `IoPriorityVeryLow`.  Read via
>   `NtQueryInformationThread(ThreadIoPriority)` or PE's I/O Priority
>   column.
> * **Memory priority** — drops to `MEMORY_PRIORITY_LOWEST`.  Read
>   via `GetThreadInformation(ThreadMemoryPriority)`.
>
> The thread's **scheduling priority** (`GetThreadPriority`, the
> `THREAD_PRIORITY_*` enum) **stays at `THREAD_PRIORITY_NORMAL` /
> Base Priority = 8**, by design — background mode is meant to
> deprioritise I/O and memory without slowing CPU work when the
> system is idle.  This means **`.NET ProcessThread.PriorityLevel`
> and `Get-CimInstance Win32_Thread` will NOT show any change
> during a tick** (verified empirically: 60 s of 10 Hz polling
> across two ticks captured zero threads at lowered scheduling
> priority).  Only the I/O Priority column or the
> `NtQueryInformationThread` syscall surface the actual change.

**(a) Debug-log grep — text-only, definitive (preferred for PR
evidence):**

Clear the `RUST_LOG` env var (it overrides `--log-level`), restart
with `--log-level debug`, wait 2-3 ticks, then grep for
`BackgroundIoPriority`.  Empty grep = `SetThreadPriority` succeeded
on every per-drive worker; the wiring + the unit test
(`refresh_usn_for_warm_shards_wraps_each_closure_in_background_io_scope`)
complete the proof:

```powershell
uffs --daemon stop
Remove-Item Env:\RUST_LOG -ErrorAction SilentlyContinue
$env:UFFS_USN_REFRESH_INTERVAL_SECS = "30"
uffs --daemon start --log-level debug --log-file C:\Temp\uffsd-G3-debug.log

# Re-warm so refresh has work to do.
uffs "*" --ext rs --drive C --limit 5 > $null

Start-Sleep -Seconds 90

# Empty grep = success (begin() returned Ok on every closure).
"=== BackgroundIoPriority diagnostic (empty = good) ==="
Select-String -Path C:\Temp\uffsd-G3-debug.log -Pattern 'BackgroundIoPriority' |
    Select-Object -ExpandProperty Line

# Tick lines confirm the controller fired during the same window.
"=== Tick lines ==="
Select-String -Path C:\Temp\uffsd-G3-debug.log -Pattern 'USN refresh tick' |
    Select-Object -ExpandProperty Line
```

If the `BackgroundIoPriority` grep prints any `begin failed` lines,
that's a real bug — file a follow-up issue with the error string and
stop.  G3 stays 🟡 in that case.

**(b) Process Explorer Threads tab (GUI screenshot, redundant
verification):**

1. Right-click `uffsd.exe` → **Properties** → **Threads** tab.
2. Right-click any column header → **Select Columns** → **Threads**
   sub-tab → enable **I/O Priority**.  (Base Priority is *not*
   useful here — it stays at 8 in background mode.)
3. During a tick window (between the
   `USN refresh tick starting count=N` and
   `USN refresh tick complete refreshed=N` log lines), screenshot.
   Expect ≥ 1 thread with **I/O Priority = Very Low**.  Outside the
   tick, all threads show `I/O Priority = Normal`.

**(c) Sysinternals `accesschk.exe -p -t <pid>`** (if installed):
Dumps per-thread info including I/O priority — grep the output for
`I/O Priority: VeryLow` during a tick window.

> **What does NOT work for this gate:**
>
> * `Get-CimInstance Win32_Thread` — the WMI provider returns NULL
>   for `PriorityCurrent` / `BasePriority` on modern Windows.
> * `(Get-Process uffsd).Threads | Where PriorityLevel -in 'Idle',
>   'Lowest', 'BelowNormal'` — reads scheduling priority via
>   `GetThreadPriority`, which doesn't change in background mode.
> * Task Manager "Details" tab I/O Priority column — process-level
>   only; the daemon's main thread is at Normal so this column
>   stays Normal.

Grep the log for the matching tick window:

```powershell
Select-String -Path C:\Temp\uffsd-G3.log -Pattern 'USN refresh tick' |
    Select-Object -ExpandProperty Line
```

The transition is driven by `crate::cache::background_io::BackgroundIoScope`
RAII guards wrapped around each per-letter `tokio::task::spawn_blocking`
closure in `crate::index::transitions::IndexManager::refresh_usn_for_warm_shards`
(Phase 5 task 5.7).  Mac/Linux: no-op (the trait stub returns
`Ok(())`).  Windows: `SetThreadPriority(GetCurrentThread(),
THREAD_MODE_BACKGROUND_BEGIN)` on enter,
`THREAD_MODE_BACKGROUND_END` on drop.

##### Diagnostic if priority is not dropping

`BackgroundIoScope::begin()` failures are logged at **debug** level
under target `shard.refresh` and otherwise swallowed.  If `(b)` or
`(c)` show the threads stuck at Normal priority during a tick,
restart the daemon with `RUST_LOG=...,shard.refresh=debug` and
grep:

```powershell
Select-String -Path C:\Temp\uffsd-G3.log -Pattern 'BackgroundIoPriority' |
    Select-Object -ExpandProperty Line
```

A non-empty match on `begin failed` indicates a real bug (e.g., the
process token is missing `SeIncreaseBasePriorityPrivilege`, or an AV
product is intercepting `SetThreadPriority`); empty grep + threads
at Normal would mean the scope guard is not actually running.

#### Capture for the PR

**Preferred (path (a) above):** the empty `BackgroundIoPriority`
grep + populated `USN refresh tick` grep from a `--log-level debug`
run.  Two short text blocks, no GUI required.  This is sufficient
proof because:

1. The unit test
   `refresh_usn_for_warm_shards_wraps_each_closure_in_background_io_scope`
   in `crates/uffs-daemon/src/index/tests/usn_refresh.rs` already pins
   that `begin()` and `end()` are called exactly once per Warm shard
   per refresh tick.
2. The empty debug grep proves `SetThreadPriority` returned `Ok` for
   every per-drive worker (the only failure path is `tracing::debug!`
   target `shard.refresh` line `BackgroundIoPriority::begin failed`).
3. The populated tick grep proves the controller fired during the
   same observation window.

**Optional (path (b) above):** screenshot of PE Threads tab with the
**I/O Priority** column showing `Very Low` on a worker thread
during a tick.  Useful if a reviewer wants visual confirmation but
not required for sign-off.

Reset before moving on:

```powershell
Remove-Item Env:\UFFS_USN_REFRESH_INTERVAL_SECS
uffs --daemon stop
```

---

### G4 — 1-hour sustained-pressure soak: no OOM

**Duration:** 60 min wall-clock (one operator-attended setup; runs unattended).
**Plan reference:** §3 Phase 5 line "1-hour sustained-pressure test: no OOM."

#### Drive

Run an **adaptive** allocator continuously for 60 min.  The kernel's
`LowMemoryResourceNotification` threshold scales with physical RAM
(roughly 1.5% of total — ~1 GB on 64 GB hosts, ~512 MB on 32 GB), so
the allocator must drive sysAvailable down to that threshold to fire
Low; the only correct stop condition is "sysAvailable about to hit a
safety floor", **not** a hardcoded GiB cap based on guessed reserves
(an empirical lesson: a `TotalRAM − 26 GiB` cap stopped the allocator
at 38 GiB on a 64 GB host with sysAvailable still at 7 GB, never
firing Low).  The version below has only one stop condition — the
256 MB safety floor — so it grows as much as the host has headroom
for, then holds at the target while the daemon cascades:

```powershell
# Set up daemon log routing FIRST (before allocator), in a fresh
# shell so RUST_LOG doesn't carry over from a previous gate.
Remove-Item Env:\RUST_LOG -ErrorAction SilentlyContinue
$env:RUST_LOG = "uffs_daemon=info,cache.pressure=info,shard.transition=info"
uffs --daemon stop
uffs --daemon start --log-file C:\Temp\uffsd-G4.log

# In a second shell, run the adaptive allocator.
$targetAvailMB = 1024  # squeeze sysAvailable down to ~1 GB to fire Low
$safetyAvailMB = 256   # NEVER drop below this — allocator pauses growth
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()

$start = Get-Date
$end   = $start.AddMinutes(60)
$alloc = New-Object System.Collections.Generic.List[byte[]]

Write-Host "Adaptive allocator (incompressible random fill; safety floor only)."
Write-Host "  target=$targetAvailMB MB  safety floor=$safetyAvailMB MB"
Write-Host "  start=$($start.ToString('HH:mm:ss'))  end=$($end.ToString('HH:mm:ss'))"

while ((Get-Date) -lt $end) {
    $avail = [math]::Round((Get-Counter '\Memory\Available MBytes').CounterSamples[0].CookedValue)
    $action = if ($avail -lt $safetyAvailMB) {
        "hold(safety)"
    } elseif ($avail -gt $targetAvailMB) {
        $arr = New-Object byte[] 1073741824
        $rng.GetBytes($arr)                  # incompressible — defeats Memory Compression
        $alloc.Add($arr)
        "+1 GiB"
    } else {
        "hold(target)"
    }
    Write-Host "$(Get-Date -Format 'HH:mm:ss')  $($action.PadRight(13))  alloc=$($alloc.Count) GiB  sysAvailable=$avail MB"
    Start-Sleep -Seconds 5
}

$alloc.Clear()
[GC]::Collect()
[GC]::WaitForPendingFinalizers()
```

> **Why cryptographic random bytes, not `[Array]::Fill($arr, [byte]1)`.**
> Windows 10/11 enables **Memory Compression** by default: pages
> with low entropy (uniform fill, mostly-zeros, repetitive patterns)
> are compressed in-place to a small fraction of their size and
> held in the **Compression Store** under the System process.
> Verified empirically: a `byte 1` fill on a 64 GB host let the
> allocator reach 70 GiB held while `sysAvailable` stayed at 4-7 GB
> the whole time — the OS had compressed ~50 GiB into ~10 GiB of
> physical memory, so kernel-Low never fired.  `RandomNumberGenerator.GetBytes`
> produces ~8 bits/byte entropy which the compressor cannot shrink,
> so each +1 GiB allocation drops `sysAvailable` by ~1 GiB linearly
> as expected.  The crypto RNG path is slower (~150 MB/s vs ~3 GB/s
> for the uniform fill) so each chunk takes ~7 s instead of <1 s,
> but the kernel-Low signal arrives reliably.

State machine (only three states, no GiB cap):

* **Climb:** `sysAvailable > target` → add 1 GiB of random bytes.
* **Hold(target):** `safety <= sysAvailable <= target` → hold steady.
  The daemon's cascade runs here; as it frees memory, sysAvailable
  rises above target and the allocator climbs again, driving the
  next Low event.
* **Hold(safety):** `sysAvailable < safety` → stop adding.  Prevents
  OOM-killing other apps when the daemon hasn't cascaded fast enough.

The allocator will grow to whatever GiB count is required to push
sysAvailable down to the target on this specific host.  With
incompressible fill that's typically (TotalRAM − DaemonRSS − ~6 GB
OS overhead): on a 64 GB box with the daemon at ~16 GiB, expect
the allocator to reach ~42-46 GiB held when `sysAvailable` first
crosses the target.  Each `hold(target)` window is the daemon's
chance to cascade; each return to `+1 GiB` is the next Low trigger.
Over 60 min you should see the daemon log emit a
`level=Low ... level=High` pair every 30-90 s.

#### Capture

Acceptance criteria:

* `uffsd.exe` is still running (`Get-Process uffsd | Select Id, WS`).
* No `OutOfMemoryError` lines in `uffsd-G4.log`.
* No `panic` lines.
* `uffs --status --drives` returns within 1 s after the soak completes;
  the per-drive tier markers reflect a coherent state (some `[Parked]`,
  some `[Hot]`/`[Warm]` where the operator drove queries).

#### Capture for the PR

* `Get-Process uffsd | Select Id, WS, PM, NPM, VM, CPU, StartTime`
  output captured at 0 min, 30 min, and 60 min.
* The tail of `uffsd-G4.log` (last 200 lines) — should show
  alternating `level=Low` / `level=High` transitions and no fatal
  errors.

> **Distinguishing pressure-cascade from TTL idle-demote.**
> Every demote — whether TTL-driven (`demote_idle_shards`) or
> pressure-driven (`cascade_demote_one_step`) — flows through the
> low-level `Registry::demote_letter_with_reason` primitive in
> `cache/registry.rs`, which emits **exactly one** canonical
> `INFO`-level `shard.transition` event per shard.  The
> discriminator is in the `reason` field:
>
> * `reason="demote"` — TTL-driven idle demote (the
>   `DemoteReason::IdleTtl` default).
> * `reason="pressure-cascade"` — kernel-Low pressure cascade
>   demote (`DemoteReason::PressureCascade`, used only by
>   `cascade_demote_one_step`).
>
> Both events also carry `letter`, `from`, `to`, `freed_mb`, and
> `last_query_at_ms` fields (the latter was promoted from a
> cascade-only field to a uniform schema during the G4 follow-up
> refactor).  Operator runbooks can therefore count each path
> independently with a single grep:
>
> ```powershell
> # Cascade-demote events (the goal during a memory-pressure soak):
> $cascade = (Select-String -Path C:\Temp\uffsd-G4.log -Pattern 'reason="pressure-cascade"').Count
> "Cascade-demote events:  $cascade"
>
> # TRUE TTL-driven idle demotes (should be 0 on a clean G4 run
> # with `UFFS_WARM_TO_PARKED_IDLE_SECS=3600`):
> $ttl = (Select-String -Path C:\Temp\uffsd-G4.log -Pattern 'reason="demote"').Count
> "TTL idle-demote events: $ttl"
> ```
>
> No pair-match is required — the `reason` field is authoritative.
> Pinned in `crates/uffs-daemon/src/index/tests/idle_demote.rs::cascade_demote_emits_single_event_with_pressure_cascade_reason`
> so a future refactor can't reintroduce the prior dual-event
> pattern.

---

## 2. Phase 6 operator gate — 24-hour `min_tier = "Warm"` soak

**Duration:** 24 h wall-clock (set up and walk away).
**Plan reference:** `docs/refactor/memory-tiering-implementation-plan.md` §3 Phase 6 Windows gate.

#### Setup

Write a `daemon.toml` to the platform-default path
(`%LOCALAPPDATA%\uffs\daemon.toml`) with `min_tier = "Warm"` for `C:`:

```powershell
$cfgPath = "$env:LOCALAPPDATA\uffs\daemon.toml"
@'
[shards.per_drive."C:"]
min_tier = "WARM"
'@ | Set-Content -Encoding UTF8 -Path $cfgPath
uffs --daemon stop
# shard.ttl at TRACE (not DEBUG) is REQUIRED — see the 2026-05-11
# soak findings in §6 ("Reference captures") sub-section §4.5b
# below.  The DEBUG `idle-demote` /
# `min-tier-clamp` events only fire when the controller is
# proposing a demote; during the synthetic-load window drive C
# sits in Warm/Hot with `idle_secs ≈ 0`, so only the catch-all
# TRACE-level `below-ttl` event carries the bonused
# `warm_ttl_sec` field that criterion 3 below scrapes for.
$env:RUST_LOG = "uffs_daemon=info,shard.ttl=trace,shard.transition=info"
uffs --daemon start --drives C,D,E,F,G,M,S 2>&1 | Tee-Object -FilePath C:\Temp\uffsd-phase6-soak.log
```

#### Drive

**Do nothing for 24 h.**  No queries against any drive; let the
adaptive-TTL ladder drive demotions naturally.

#### Capture

Acceptance criteria (the Phase 6 contract under `[shards.per_drive]`):

**Note on grep patterns**: tracing target names (`shard.transition`,
`shard.ttl`) are NOT rendered in the log line text under the default
formatter.  In addition, two different conventions coexist in the
source:

* `cache/registry.rs` demote / promote / usn-refresh events
  (the canonical `shard.transition` info events, including the
  cascade path's `reason="pressure-cascade"` after the G4
  follow-up refactor) use field name `letter=` and lowercase
  state names (`to=parked`).
* `index/transitions.rs` `shard.ttl` debug / trace events
  (idle-demote evaluation diagnostics) and `shard.refresh`
  events (USN refresh tick) use field name `drive=` and the
  `TierLevel` `Debug` formatter (`to=Parked`, capitalized,
  unquoted).

The patterns below handle both.  Future operators: if you change the
daemon's tracing fields, update these patterns in the same commit.

1. **C never demotes below `Warm`.**  Grep the soak log:
   ```powershell
   Select-String -Path C:\Temp\uffsd-phase6-soak.log -Pattern '(letter|drive)=C\b.*to="?[Pp]arked"?' -List
   ```
   Expect **zero matches**.  Every `min-tier-clamp` event for C is
   logged at debug-level via `shard.ttl` with the descriptive
   message `"Demote target clamped by per-drive min_tier"`:
   ```powershell
   Select-String -Path C:\Temp\uffsd-phase6-soak.log -Pattern 'Demote target clamped.*drive=C' -Context 0,0 |
       Select-Object -First 5
   ```

2. **Other drives demote normally** (Warm → Parked at the configured
   `warm_ttl_base_secs`):
   ```powershell
   Select-String -Path C:\Temp\uffsd-phase6-soak.log -Pattern '(letter|drive)=[DEFGMS]\b.*from="?[Ww]arm"?.*to="?[Pp]arked"?' -List
   ```
   Expect at least one match per drive (D, E, F, G, M, S).

3. **Different TTLs for high-rate vs low-rate drives.**  After the
   soak, drive a synthetic 5-min load against C, leave the others
   idle, and grep for the per-drive `warm_ttl_sec` field on the
   `shard.ttl` events.  `warm_ttl_sec` (not the older
   `chosen_ttl_sec`) is the right field for the cross-drive
   comparison: it is the rate-sensitive Warm→Parked edge that
   exists on **every** drive's events regardless of current tier,
   so the target-vs-peers compare is apples-to-apples (the
   pre-2026-05-07 `chosen_ttl_sec` compare was structurally
   impossible to pass when drives were in different tiers — see
   `crate::index::tests::shard_ttl_events` for the contract pin):
   ```powershell
   Select-String -Path C:\Temp\uffsd-phase6-soak.log -Pattern 'warm_ttl_sec' |
       Select-Object -Last 30 -ExpandProperty Line
   ```
   The C row's `warm_ttl_sec` during the synthetic-load window
   should exceed the peers' by the `600·log2(rate)` bonus from
   the §5.2 formula (`crate::cache::policy::warm_ttl`).  Per-tick
   evidence requires `shard.ttl=trace` in `RUST_LOG` (see the
   Setup block above, the 2026-05-11 deferral analysis in §6
   sub-section §4.5b, and the 2026-05-14 end-to-end closing
   capture in §6 sub-section §4.5e — C.max_warm_ttl=3 786 s vs
   peer max=300 s).

#### Reset

```powershell
Remove-Item $cfgPath
uffs --daemon stop
```

#### Automation

`just soak phase6` (or `rust-script scripts/dev/long-soak.rs phase6`)
automates the entire setup → soak → end-of-soak load → validation
flow.  See §3 just below for the matching Phase 7 recipe — the two
recipes share their snapshot / output-dir / validation-report shape.

---

## 3. Phase 7 operator gate — 24-hour USN-journal churn soak

**Duration:** 24 h wall-clock (set up and walk away).
**Plan reference:**
`docs/refactor/memory-tiering-implementation-plan.md`
§3 Phase 7 Windows gate.

Validates that the per-shard USN-journal loop
(`crates/uffs-daemon/src/cache/journal_loop.rs`, PR #117 / activation
PR #118) keeps a Warm shard's body, bloom, and trie in sync with the
live NTFS journal across 24 h of continuous churn — the long-running
Windows-host equivalent of the Mac-side 1048 / 1048 unit test suite.

#### Setup

The `just soak phase7` recipe takes care of everything below; the
manual steps are documented for the operator who wants to understand
what the harness is doing or run a slice of it standalone.

```powershell
# 1. Pick a churn directory under the user profile.  Default in
#    long-soak.rs.  Avoid system-owned paths so the soak can clean
#    up after itself without elevation.
$churnDir = "$env:USERPROFILE\uffs-soak\churn"
New-Item -ItemType Directory -Path $churnDir -Force | Out-Null

# 2. Bounce the daemon with the right log filter.
Remove-Item Env:\RUST_LOG -ErrorAction SilentlyContinue
$env:RUST_LOG = "uffs_daemon=info,shard.refresh=info,shard.transition=info,journal_loop=debug"
uffs --daemon stop
uffs --daemon start --drives C,D,E,F,G,M,S \
    --log-file "$env:USERPROFILE\uffs_soak\phase7-$(Get-Date -Format yyyyMMdd-HHmmss)\daemon.log"
```

#### Drive

A continuous create / modify / delete loop in `$churnDir`.  The
`just soak phase7` recipe spawns a background thread for this; the
portable PowerShell-only equivalent is:

```powershell
# Run for 24 h, ~5 files / sec.
$end = (Get-Date).AddHours(24)
$counter = 0
while ((Get-Date) -lt $end) {
    $path = Join-Path $churnDir "churn-$($counter % 1024).tmp"
    "phase7 churn payload $counter"  | Set-Content -Path $path
    "appended at $(Get-Date -Format o)" | Add-Content -Path $path
    if ($counter % 4 -eq 0) { Remove-Item -Path $path -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 200
    $counter++
}
```

#### Capture

Four acceptance criteria (the Phase 7 contract under
`memory-tiering-implementation-plan.md` §3 Phase 7 Windows gate):

1. **New-item latency ≤ 2 s.**  Drop a unique probe file in
   `$churnDir`; confirm `uffs --daemon status_drives` returns a
   non-error response within 2 s of the file's `LastWriteTime`.
   The harness probes once at T+0 and once at T+24h; both must hit
   the budget.

2. **Encrypted-cache refresh fired during the soak** (≤ 1× / 5 min,
   ≥ 1× total over 24 h).  Grep the daemon log for the literal
   substring of the `journal_loop::process_tick`-emitted save
   event:
   ```powershell
   Select-String -Path C:\Temp\uffsd-phase7-soak.log `
       -Pattern 'compact-cache save' |
       Select-Object -ExpandProperty Line | Measure-Object
   ```
   `count` must be `≥ 1` (≥ 1× total).  A 24 h soak typically
   produces 10-30 events depending on the threshold mix.

   The matching INFO line shape is:

   ```text
   INFO Journal poll: triggered background compact-cache save \
       drive=F reason=AgeElapsed cursor=151008
   ```

   The literal `"compact-cache save"` substring is pinned by
   `crate::cache::journal_loop::tests::save_log_message::
   compact_cache_save_log_message_pins_string_target_and_level`
   so a future rename fails CI before reaching a 24-h soak.
   See the 2026-05-11 finding in §6 sub-section §4.5c for the
   validator-regex fix history.

3. **`uffsd.exe` Working Set ≤ 1.5× over 24 h.**  Hourly
   `Get-Process uffsd | Select Id, WS, ...` snapshots.  Compare
   the first to the last:
   ```powershell
   $first = Get-Content (Resolve-Path "$soakDir\snapshots\00h-process.json") | ConvertFrom-Json
   $last  = Get-Content (Resolve-Path "$soakDir\snapshots\24h-process.json") | ConvertFrom-Json
   $ratio = $last.WS / $first.WS
   "WS ratio: $ratio (must be <= 1.5)"
   ```
   The harness wraps this assertion as a `validation/*.{pass,fail}`
   breadcrumb.

   > **Note (2026-05-13 finding):** on Windows the `WS` field
   > drops sharply on the first `EmptyWorkingSet` trim (Phase 5
   > G2 wiring) and stays low for the rest of the soak — the
   > daemon's actual memory footprint is `PM` (Private Memory
   > Size / commit-charge), which is also in every snapshot.
   > The `≤ 1.5×` bound still catches a real leak (which would
   > grow both WS and PM monotonically), but reviewers should
   > cross-check `PM` from `00h-process.json` vs
   > `23h-process.json` for the leak-relevant reading.  See §6
   > sub-section §4.5d for the full breakdown.

4. **No `panic` / `OutOfMemoryError` / `FATAL`.**  Same grep
   pattern as G4:
   ```powershell
   Select-String -Path C:\Temp\uffsd-phase7-soak.log `
       -Pattern '\bpanic\b|\bOutOfMemoryError\b|\bFATAL\b'
   ```
   Must return zero matches.

#### Reset

```powershell
uffs --daemon stop
Remove-Item -Recurse -Force "$env:USERPROFILE\uffs-soak\churn"
```

#### Automation

The one-line invocation that captures all four assertions in a
single 24 h run is:

```powershell
just soak phase7
```

…which writes its full output (daemon log + per-hour snapshots +
validation breadcrumbs + summary) into
`$env:USERPROFILE\uffs_soak\phase7-<timestamp>\`.  Attach
`summary.txt` + the snapshot dir to the acceptance PR.

For a Mac-side shake-out before burning the real Windows 24 h:

```bash
just soak phase7 --dev   # 5-min run, 1-min snapshots, relaxed bounds
```

The `--dev` flag is documented in the harness CLI; production
invocations omit it.

---

## 4. Phase 8 operator-command gates — G5-G8, 4 captures, ~20 min wall-clock

These gates validate the four operator-driven memory-tiering
commands shipped in Phase 8 (PRs #122 + #123).  They run against
the same daemon used for the Phase 5 / 6 gates — pick any moment
when interactive search is paused.  Captures go in the same PR
description as the Phase 5 / 6 captures.

The four gates correspond 1:1 to plan tasks 8.1 / 8.2 / 8.3 / 8.4.
Run them in order — G7 is destructive (deletes a drive's caches)
and the order leaves the daemon in a known-good shape for the
final G8 render.

### G5 — `uffs --daemon hibernate` demotes every drive to Cold

**Duration:** ~3 min wall-clock.
**Plan reference:** plan task 8.1; PR #122.

#### Setup

A daemon running with the seven drives in mixed tiers (any steady
state — typically the post-G4 shape after the Phase-5 soak).

#### Drive

```powershell
# Capture pre-hibernate state.
"=== Pre-hibernate ==="
Get-Process uffsd | Select Id, WS, PM, NPM, VM
uffs --daemon status_drives
```

```powershell
# Hibernate every loaded drive.
uffs --daemon hibernate
```

Expected stdout (drive letters depend on the registry):

```text
Daemon hibernated 7 drive(s):
  Hot     -> Cold:  C
  Warm    -> Cold:  D, E, F
  Parked  -> Cold:  G, M, S
  Already Cold:     (none)
```

#### Capture

```powershell
"=== Post-hibernate ==="
Get-Process uffsd | Select Id, WS, PM, NPM, VM
uffs --daemon status_drives

# Hibernate keeps the encrypted compact caches on disk — verify
# they're all still there (the *_compact.uffs files are the
# Cold-tier source-of-truth for re-promote).
"=== Cache files preserved ==="
Get-ChildItem "$env:LOCALAPPDATA\uffs\cache\*_compact.uffs" |
    Select-Object Name, Length, LastWriteTime
```

Acceptance criteria:

* Every drive's `TIER` column in `status_drives` is `cold`.
* `uffsd.exe` Working Set drops by ≥ 50 % vs the pre-hibernate
  sample (no body Arc, no parked-body bloom + trie resident).
* Every `<letter>_compact.uffs` file from the pre-hibernate sample
  is still on disk with the same `Length` (hibernate releases RAM
  only — disk untouched).

### G6 — `uffs --daemon preload` pin contract survives idle TTL

**Duration:** ~10 min wall-clock.
**Plan reference:** plan task 8.2; PR #122.

The pin-contract test that the Mac unit-test suite cannot exercise
is "pinned shard survives the live demote-controller's idle-TTL
evaluation" — Mac tests inject mock clocks; this gate uses the
real wall clock with shortened TTLs so the operator can observe
the pin actually defending against demote in the wild.

#### Setup

Continuing from G5 — every drive Cold.  Lower the warm-to-parked
TTL via env so the demote-controller can prove the pin works
without a default-30-min wait:

```powershell
$env:UFFS_WARM_TO_PARKED_IDLE_SECS = "30"
$env:UFFS_PARKED_TO_COLD_IDLE_SECS = "60"
$env:RUST_LOG = "uffs_daemon=info,shard.transition=info,shard.ttl=debug"
uffs --daemon stop
uffs --daemon start --drives C,D,E,F,G,M,S 2>&1 |
    Tee-Object -FilePath C:\Temp\uffsd-G6.log
```

#### Drive

```powershell
# Pin C in Hot for 5 minutes.
uffs --daemon preload C --pin-minutes 5
```

Expected stdout:

```text
Daemon preloaded (5-min pin):
  Promoted to Hot:  C
  Already Hot:      (none)
  Pin expires at:   <unix-millis> (Unix-millis)
```

Verify C is Hot + pinned and the others are still Cold:

```powershell
"=== Pre-wait ==="
uffs --daemon status_drives
```

Wait through 1.5× the warm-to-parked TTL — total ~90 s — long
enough for `demote_idle_shards` to evaluate every shard at least
twice:

```powershell
"=== Wait 90 s for the idle-demote controller to fire ==="
Start-Sleep -Seconds 90
"=== Post-wait ==="
uffs --daemon status_drives
```

Confirm the demote-controller log shows zero `to=parked` /
`to=cold` lines for `letter=C`:

```powershell
Select-String -Path C:\Temp\uffsd-G6.log -Pattern '(letter|drive)=C\b.*to="?[Pp]arked|cold"?' |
    Select-Object -ExpandProperty Line
```

Empty output = the pin gate in `IndexManager::demote_idle_shards`
+ `cascade_demote_one_step` correctly skipped C.

#### Capture

Paste into the PR:

* Pre-wait `status_drives` row for C — `tier=hot`,
  `pin_until_ms > 0`.
* Post-wait `status_drives` row for C — still `tier=hot` despite
  90 s past the warm-to-parked TTL.
* Empty grep result above (verbatim).

Acceptance criteria:

* C's `TIER` column stays `hot` for the full 90-s window.
* C's `PIN UNTIL (ms)` column is non-zero and at least 5 min in
  the future.
* No `letter=C` `to=parked` / `to=cold` events in the daemon log
  for the wait window.

#### Reset

```powershell
Remove-Item Env:\UFFS_WARM_TO_PARKED_IDLE_SECS
Remove-Item Env:\UFFS_PARKED_TO_COLD_IDLE_SECS
uffs --daemon stop
uffs --daemon start --drives C,D,E,F,G,M,S
```

### G7 — `uffs --daemon forget --force` evicts + deletes caches

**Duration:** ~3 min wall-clock.
**Plan reference:** plan task 8.3; PR #123.

> **WARNING — destructive.**  This gate **deletes** a drive's
> on-disk caches.  The next search of that drive must re-read the
> entire MFT (cold boot, ~30-60 s for a 4 M-record drive).  Pick
> a drive you can afford to re-build — the documented choice on
> the 7-drive reference box is **`M:`** (smaller volume, less
> painful re-warm).  **DO NOT** run this gate against `C:`.

#### Setup

The daemon from G6's reset.  Capture the chosen drive's on-disk
cache footprint pre-forget so we can verify the freed-bytes
accounting:

```powershell
$drive = 'M'
$cacheRoot = "$env:LOCALAPPDATA\uffs\cache"

# Phase 8-D unlinks four canonical paths.  Filenames are
# case-mixed in production (uffs-mft uses uppercase for
# `_index.{uffs,lock}`; uffs-core uses lowercase for
# `_compact.uffs` / `_usn.cursor`) — the cleaner is case-tolerant
# but the Test-Path verification has to be too.
$cachePaths = @(
    "$cacheRoot\$($drive.ToLower())_compact.uffs"
    "$cacheRoot\$($drive.ToLower())_usn.cursor"
    "$cacheRoot\$($drive.ToUpper())_index.uffs"
    "$cacheRoot\$($drive.ToUpper())_index.lock"
)

"=== Pre-forget cache footprint ==="
$total = 0
foreach ($p in $cachePaths) {
    if (Test-Path $p) {
        $size = (Get-Item $p).Length
        Write-Host ("  {0,12:N0} bytes  {1}" -f $size, (Split-Path -Leaf $p))
        $total += $size
    }
}
"  --------"
("  {0,12:N0} bytes  TOTAL" -f $total)
```

#### Drive

```powershell
uffs --daemon forget M --force
```

Expected stdout:

```text
Daemon forgot 1 drive(s); freed XX.XX MiB:
  Forgotten:        M
  Already absent:   (none)
```

#### Capture

```powershell
# Verify M is gone from the registry.
"=== Post-forget status_drives — M must NOT be listed ==="
uffs --daemon status_drives

# Verify every per-drive cache file is gone.
"=== Post-forget cache files — every Test-Path must be False ==="
foreach ($p in $cachePaths) {
    $exists = Test-Path $p
    Write-Host ("  exists={0}  {1}" -f $exists, $p)
}
```

Acceptance criteria:

* `forget` stdout's freed-bytes value (the `Daemon forgot 1
  drive(s); freed XX.XX MiB` line) matches the pre-forget total
  within rounding.
* `status_drives` no longer lists `M`.
* Every per-drive cache file (`*_compact.uffs`, `*_usn.cursor`,
  `*_index.uffs`, `*_index.lock`) is absent on disk.

#### Reset

To restore `M:` to the daemon, hot-load it (re-reads the MFT cold,
~30-60 s for a typical drive):

```powershell
uffs --daemon load --drive M
```

### G8 — `uffs --daemon status_drives` table render contract

**Duration:** ~1 min wall-clock.
**Plan reference:** plan task 8.4; PR #123.

#### Drive

```powershell
uffs --daemon status_drives
```

#### Capture

Paste the full table output verbatim into the PR description.

Acceptance criteria:

* Header row exactly matches:
  ```text
  DRIVE  TIER    RESIDENT     QPM   LAST QUERY (ms)   PIN UNTIL (ms)
  ```
* One row per drive currently loaded (post-G7 this is 6 if you
  forgot `M`; 7 if you re-loaded it via the G7 reset).
* Rows are sorted by drive letter ASCII ascending.
* `TIER` column values are lowercase (`hot` / `warm` / `parked` /
  `cold`).
* `RESIDENT` column has the right unit suffix per tier:
  * `hot` / `warm` ⇒ `MiB` or `GiB` (full body heap)
  * `parked` ⇒ `KiB` or `MiB` (bloom + trie only)
  * `cold` ⇒ `0 B`
* `PIN UNTIL (ms)` column is `-` for unpinned drives, a Unix-millis
  integer for pinned ones (only the drive last `preload`-ed within
  the pin window).
* `LAST QUERY (ms)` column is `-` for never-queried drives, a
  Unix-millis integer otherwise.

---

## 5. PR-attachment checklist

Before opening the Phase 5 / Phase 6 / Phase 7 / Phase 8 acceptance
PR, paste the following into the description so the reviewer can
sign off without re-running the soak:

* **G1** capture — log excerpt showing `Low` → cascade chain →
  `High` with the LRU-ordered `drive=` field.
* **G2** capture — pair of Task Manager screenshots (peak / +5 s).
* **G3** capture — Task Manager screenshot showing
  `I/O priority = Low` during a `shard.refresh` tick window;
  matching log line.
* **G4** capture — `Get-Process uffsd` table at 0 / 30 / 60 min plus
  the soak-log tail.
* **Phase 6 24-h soak** capture — `summary.txt` from
  `$env:USERPROFILE\uffs_soak\phase6-<timestamp>\` plus the three
  grep-result files under `validation/` (`Drive_C_never_demotes_below_Warm.pass`,
  the per-peer-drive `Warm-Parked.pass` files, and
  `Drive_C_warm_ttl_sec_exceeds_peers__adaptive_bonus_engaged_.pass`).
  Note: the file name pivoted from
  `Drive_C_chosen_ttl_sec_exceeds_peers.pass` to the `warm_ttl_sec`
  shape in the 2026-05-07 validator update; runs against the
  pre-update harness produce the older breadcrumb shape.
* **Phase 7 24-h soak** capture — `summary.txt` from
  `$env:USERPROFILE\uffs_soak\phase7-<timestamp>\` plus the four
  grep-result files under `validation/`
  (`No_panic_OOM_FATAL.pass`, both `*latency*.pass`,
  `Encrypted-cache_refresh_fired.pass`,
  `Working-Set_growth_ratio.pass`).  Also attach
  `snapshots/00h-process.json` and `snapshots/24h-process.json` so
  the reviewer can independently spot-check the WS bound.
* **G5 hibernate** capture — pre/post `status_drives` showing every
  drive demoted to `cold`, plus the `Get-ChildItem
  *_compact.uffs` listing proving the on-disk caches were
  preserved.
* **G6 preload pin** capture — pre-/post-wait `status_drives` rows
  for `C` showing `tier=hot` survives a 90-s wait past the
  warm-to-parked TTL, plus the empty `letter=C ... to=parked|cold`
  log grep.
* **G7 forget** capture — pre-forget cache-file size table +
  post-forget `Test-Path` listing showing all four files absent +
  the `freed_bytes` total from the `forget` command's stdout.
* **G8 status_drives** capture — full table output as it appears
  in the operator's terminal (header row + one row per loaded
  drive, sorted ASCII ascending).

After all ten captures land, update the implementation-plan §5.1
row for the corresponding phase to 🟢 with the date the PR landed.

---

## 6. Reference captures (2026-05-02 v0.5.86 — Phase 5 G1-G4 baseline)

This section documents the first end-to-end Phase 5 Windows-host
capture pass against the 7-drive reference box, run on 2026-05-02
with the v0.5.86 dev binary (the **pre-canonical-event-refactor**
build — the dual-logging pattern visible in the G4 excerpts below
is the very gap that the same-day refactor commit `4a627246d`
closes; future capture passes against v0.5.87+ binaries will
produce one INFO `shard.transition` event per cascade step
instead of two).  Operators running a future capture pass have a
known-good baseline to compare against.

Cross-reference: implementation-plan
`docs/refactor/memory-tiering-implementation-plan.md`
§3 Phase 5 "Phase 5 Windows-host validation findings (2026-05-02
capture pass, v0.5.86)" carries the per-gate analysis and the
end-to-end contract claims (LRU cascade ordering, watcher
`Cascade preempted by transition out of Low` mid-cascade abort,
sustained 2 h 11 min soak with no OOM).

### 4.1 Artefact index

| Gate | Artefact path (under repo root) | Wall duration | What it pins |
|---|---|---|---|
| **G1** kernel notification → cascade | `LOG/WINDOWS uffsd-G1G2.log` (706 lines) | ~32 min (20:55 → 21:30) | Win32 watcher thread translates `MEMORY_RESOURCE_NOTIFICATION` Low/High into `PressureSignal::Low/High` events |
| **G2** working-set drop ≤ 5 s | TaskMgr screenshots in PR + `LOG/WINDOWS uffsd-G1G2.log` lines 374-380 | TTL idle-demote at 21:00:41 freed 4592 MB across 7 drives in 27 ms wall | Demote → `EmptyWorkingSet` syscall returns memory to OS within the 5 s acceptance window |
| **G3** background-IO priority during USN catch-up | `LOG/G3/g3-acceptance-greps.txt` + `LOG/G3/uffsd-G3-debug.log` (1527 lines) | ~3 min 30 s (22:17:38 → 22:21:08) | `BackgroundIoScope::begin()` returned `Err` zero times across 7 successive `USN refresh tick` cadences (preferred capture path (a) — empty diagnostic + populated tick) |
| **G4** sustained-pressure soak, no OOM | `LOG/uffsd-G4.log` (33 min, 116 lines) + `LOG/uffsd-G4-bonus.log` (2 h 11 min, 110 lines) | combined 2 h 44 min (**2.7× the 60-min G4 acceptance bar**) | Daemon survives ~30 `level=Low ↔ level=High` cycle pairs at 30-90 s intervals; cascade preempts on transition-out-of-Low (4× during 7-drive drain); no panic, no `JoinError`, no shard transition fault |

### 4.2 G1 — Low-pressure stress (kernel → cascade)

Source: `LOG/WINDOWS uffsd-G1G2.log` lines 383-411 (12 paired
kernel/watcher events in a 24 s window) + lines 524, 529 (first
two cascade demotes).

```text
2026-05-02T21:28:21.134825Z  INFO Pressure transition observed level=Low
2026-05-02T21:28:21.134793Z  INFO Memory resource notification fired level=Low
2026-05-02T21:28:21.134912Z  INFO Memory resource notification fired level=High
2026-05-02T21:28:21.134936Z  INFO Pressure transition observed level=High
…
2026-05-02T21:29:47.655430Z  INFO Pressure cascade demoted one LRU Warm shard \
    drive=C from="Warm" to="Parked" reason="pressure-cascade" \
    last_query_at_ms=1777757366606
2026-05-02T21:29:49.277114Z  INFO Pressure cascade demoted one LRU Warm shard \
    drive=G from="Warm" to="Parked" reason="pressure-cascade" \
    last_query_at_ms=1777757385120
```

The kernel-side `Memory resource notification fired` event (target
`cache.pressure`) and the watcher-side `Pressure transition observed`
event arrive within 1 ms of each other — `PlatformPressureSignal::watcher`
emits the second event right after `WaitForMultipleObjects` returns
and the `watch::Sender::send_replace` publishes the new level.  This
end-to-end matches the deterministic Mac unit-test contract pinned
by `pressure_subscriber_drains_warm_cascade_on_low_and_no_ops_on_high`.

> **v0.5.86-pre-refactor shape.**  The two cascade-demote lines
> shown above are the **OLD** dual-logging format — note the
> `drive="Warm"` (quoted-string field) in the cascade event vs the
> `letter=C` (bare-char) field on the registry primitive.  Future
> v0.5.87+ captures will show **one** event per cascade step with
> the uniform `letter=` field name and `reason="pressure-cascade"`.

### 4.3 G2 — Working set drop (TTL-driven baseline)

Source: `LOG/WINDOWS uffsd-G1G2.log` lines 374-380 (TTL
idle-demote at 21:00:41, ~5 min after daemon start).

```text
2026-05-02T21:00:41.838723Z  INFO letter=G from=warm to=parked freed_mb=1     reason="demote"
2026-05-02T21:00:41.842359Z  INFO letter=M from=warm to=parked freed_mb=301   reason="demote"
2026-05-02T21:00:41.845280Z  INFO letter=F from=warm to=parked freed_mb=448   reason="demote"
2026-05-02T21:00:41.850675Z  INFO letter=E from=warm to=parked freed_mb=474   reason="demote"
2026-05-02T21:00:41.853916Z  INFO letter=C from=warm to=parked freed_mb=687   reason="demote"
2026-05-02T21:00:41.858904Z  INFO letter=D from=warm to=parked freed_mb=1318  reason="demote"
2026-05-02T21:00:41.866166Z  INFO letter=S from=warm to=parked freed_mb=1363  reason="demote"
```

All 7 drives demoted from Warm to Parked in 27 ms wall, releasing
**4592 MB** of body Arc state cumulatively.  `EmptyWorkingSet`
fires once per batch (Phase 5 task 5.4 `applied > 0` gate).
TaskMgr screenshots in the PR description show the corresponding
`uffsd.exe` Working Set drop within the 5 s acceptance window.

### 4.4 G3 — Background-IO priority during USN catch-up

Source: `LOG/G3/g3-acceptance-greps.txt` (the runbook's preferred
capture path (a) — empty `BackgroundIoPriority` debug-log grep +
populated `USN refresh tick` cadence).

```text
=== BackgroundIoPriority diagnostic lines (empty = success) ===

=== USN refresh tick lines ===
2026-05-02T22:26:35.723436Z  INFO USN refresh tick starting count=7 interval_secs=30
2026-05-02T22:26:49.360505Z  INFO USN refresh tick complete refreshed=7 failed=0 total=7 total_ms=13637
2026-05-02T22:27:05.714331Z  INFO USN refresh tick starting count=7 interval_secs=30
2026-05-02T22:27:17.389393Z  INFO USN refresh tick complete refreshed=7 failed=0 total=7 total_ms=11675
…
```

Empty diagnostic = `BackgroundIoScope::begin()` returned `Ok`
on every per-letter `spawn_blocking` worker across 7 successive
30 s refresh ticks (each ~11-13 s wall for 7 drives totalling
25.7 M records).  No leaked thread-priority elevations
(`Drop::drop` paired `THREAD_MODE_BACKGROUND_END` correctly via
the RAII guard).  Full debug log: `LOG/G3/uffsd-G3-debug.log`.

### 4.5 G4 — 1-hour sustained-pressure soak (dual log)

Source: `LOG/uffsd-G4.log` (33 min, daemon-restart phase) +
`LOG/uffsd-G4-bonus.log` (2 h 11 min, sustained pressure).
Combined wall **2 h 44 min, 2.7× the runbook's 60-min G4 bar**.

`uffsd-G4-bonus.log` lines 38-69 capture a 7-step LRU cascade
under sustained kernel-Low pressure (excerpted):

```text
2026-05-02T23:08:11.134135Z  INFO Memory resource notification fired level=Low
2026-05-02T23:08:11.134161Z  INFO Pressure transition observed level=Low
2026-05-02T23:08:11.134631Z  INFO letter=G from=warm to=parked freed_mb=1 reason="demote"
2026-05-02T23:08:11.145475Z  INFO Memory resource notification fired level=High
2026-05-02T23:08:11.970112Z  INFO Pressure cascade demoted one LRU Warm shard \
    drive=G from="Warm" to="Parked" reason="pressure-cascade" \
    last_query_at_ms=1777763174381
2026-05-02T23:08:11.970271Z  INFO Cascade preempted by transition out of Low new_level=High
…
```

Three end-to-end contracts demonstrated in this excerpt:

1. **LRU cascade ordering under fully-tied input.**  All 7
   cascade-demote events carry the **same** `last_query_at_ms=1777763174381`
   because the operator drove zero queries before the soak — the
   in-memory clock skew between `DriveStats::last_query_at_ms` of
   different shards collapsed to a single sample.  The
   `cascade_demote_one_step` per-call iteration through the
   `oldest-last_query_at_ms` heuristic still drained one shard per
   cascade tick (vs dumping all 7 at once) per the Phase 5 task
   5.10 contract.

2. **Cascade-preempt-on-High via `rx.has_changed()`.**  Lines 43,
   49, 55, 65 (4 of 7 cascade steps) show
   `Cascade preempted by transition out of Low new_level=High`
   — the kernel briefly recovered headroom mid-soak, the watcher
   thread translated the recovery to a `PressureSignal::High` send,
   and the cascade subscriber's `rx.has_changed()` poll between
   iterations caught it before over-shedding remaining shards.

3. **Dual-logging in the wild (now fixed at the source).**  Each
   cascade step in this excerpt emits **two** INFO events: the
   registry primitive's `letter=G from=warm to=parked
   freed_mb=1 reason="demote"` (line 40) **followed by** the
   cascade's redundant `Pressure cascade demoted one LRU Warm
   shard drive=G ... reason="pressure-cascade"` (line 42),
   separated by 1339 ms (the `EmptyWorkingSet` syscall on the
   first cascade demote, dropping to ~6 ms on subsequent ones).
   Operator counting cascade demotes by grepping
   `reason="pressure-cascade"` would see 7 events; counting by
   `letter=` field would see 14 (each cascade step
   double-counted).  Commit `4a627246d` (same-day refactor)
   collapses these into one event with
   `reason="pressure-cascade"`; future capture passes will not
   exhibit this pattern.

### 4.5b 2026-05-11 Phase 6 24-h capture — adaptive-bonus visibility deferred

Source: `LOG/uffs_soak/phase6-20260509-213122/` (24-h run on the
7-drive reference box, May 9-10 2026).

Run summary: **8 of 9 harness assertions PASS; 1 assertion deferred**
to a re-run with the post-2026-05-13 harness fix.

| Contract (from §2 above) | 24-h evidence | Status |
|---|---|---|
| 1. `C` never demotes below `Warm` | 0 `to=Parked` events for letter=C; 2 871 `min-tier-clamp` debug events | ✅ end-to-end verified |
| 2. Peer drives demote `Warm → Parked` normally | D / E / F / G / M / S each fired ≥ 2 Warm→Parked transitions | ✅ end-to-end verified |
| 3. Adaptive TTL bonus (`+600·log2(rate)`) engages under load | Daemon computed `warm_ttl_sec ≈ 3 687 s` every tick during the synthetic-load window but emitted it only at TRACE; harness's `RUST_LOG=shard.ttl=debug` filter dropped every `below-ttl` event | ⚠️ **deferred** — see below |

**Root cause of the deferred criterion.**  `crate::index::transitions::
evaluate_idle_demote` emits its `shard.ttl` event at one of three
levels depending on the demote-eval ladder's outcome:

| Arm | Level | Fires when |
|---|---|---|
| `idle-demote` | DEBUG | drive idle past TTL → demote target accepted |
| `min-tier-clamp` | DEBUG | drive idle past TTL → demote suppressed by `min_tier` floor |
| `below-ttl` | **TRACE** | drive not yet idle past TTL (the catch-all) |

During the synthetic-load window drive C sits in Warm/Hot with
`idle_secs ≈ 0`, so the demote-eval ladder never reaches either
DEBUG arm — only the TRACE-level `below-ttl` event fires, carrying
the bonused `warm_ttl_sec` field that criterion 3 scrapes for.
The pre-2026-05-13 runbook `RUST_LOG` was
`shard.ttl=debug`, filtering the trace events out.

**Fix landed in PR #218 (2026-05-13):**

* `scripts/dev/long-soak.rs:746` — `shard.ttl=debug` → `shard.ttl=trace`.
  Cost: ~23 k extra trace events over 24 h (~3.5 MB), marginal
  against the existing 75 MB log volume the WARN
  journal-not-active spam already produces.
* `crates/uffs-daemon/src/index/tests/shard_ttl_events.rs::
  below_ttl_event_pins_target_level_message_and_reason` —
  daemon-side regression test pinning target = `shard.ttl`,
  level = TRACE, message = `"Adaptive idle-demote evaluation: not
  yet idle past TTL"`, `reason="below-ttl"`, and the four TTL
  fields the harness's `parse_max_ttl_field` reads.

**The Phase 6 contract is satisfied at code + unit-test level**
(the EMA-integration formula is pinned by
`crate::cache::shard::tests::decay_ema_integrates_new_queries_into_rate_estimate`
from PR #146; the field shape is pinned by the new regression
test above).  **Direct end-to-end evidence of the adaptive bonus
engaging during a synthetic-load window** was captured in
the post-PR-218 re-run on 2026-05-14 — see §4.5e below for the
closing capture.

### 4.5c 2026-05-11 Phase 7 24-h capture — retroactively ALL GREEN

Source: `LOG/uffs_soak/phase7-20260510-214412/` (24-h run on the
7-drive reference box, May 10-11 2026).

Run summary: **6 of 7 harness assertions PASS at run time**; **7
of 7 with the post-2026-05-13 validator regex fix** (no new soak
required — the fix is a pure regex change against the existing
24-h `daemon.log`).

| Contract (from §3 above) | 24-h evidence | Status |
|---|---|---|
| 1. New-item latency ≤ 2 s | initial probe = 14 ms, final probe = 18 ms | ✅ end-to-end verified |
| 2. Encrypted-cache refresh fired ≥ 1× over 24 h | 11 `Journal poll: triggered background compact-cache save` events captured | ✅ end-to-end verified (after regex fix) |
| 3. `uffsd.exe` Working Set ≤ 1.5× over 24 h | first=7 259 754 496 B, last=12 767 232 B, ratio=0.00× | ✅ within bound (see footnote) |
| 4. No `panic` / `OutOfMemoryError` / `FATAL` | 0 fatal-class log lines | ✅ end-to-end verified |
| **harness bonus** Demote-to-Parked count ≤ 12 | 6 `to=Parked` events (ceiling 12) | ✅ within bound |

**Root cause of the single PASS-after-fix.**  The pre-fix
validator's regex was a speculative
`USN refresh tick|trigger_save|threshold.*save|encrypted cache refresh`
— **none** of those alternatives match the daemon's actual INFO
line `Journal poll: triggered background compact-cache save`.
The save pipeline was healthy all along; the validator was just
hunting for strings the daemon never emits.

**Fix landed in PR #218 (2026-05-13):**

* `scripts/dev/long-soak.rs:1244` — regex re-anchored on
  `compact-cache save`.  Retroactively passes the existing 24-h
  log:
  ```sh
  grep -c 'compact-cache save' \
    LOG/uffs_soak/phase7-20260510-214412/daemon.log
  # → 11
  ```
* `crates/uffs-daemon/src/cache/journal_loop/tests/
  save_log_message.rs` — daemon-side regression test pinning
  target = `uffs_daemon::cache::journal_loop`, level = INFO, and
  the literal `compact-cache save` substring so a future log-
  message rename fails CI before reaching a 24-h soak.

> **Footnote on the Working-Set ratio (criterion 3).**  The ratio
> passed the ≤ 1.5× bound with a 500× drop over 24 h
> (`7 259 754 496 B` → `12 767 232 B`).  Initially flagged as a
> potential vacuous pass (suspected idle-decay), but **resolved
> by the 2026-05-13 ws-trace capture (§4.5d below)**: the same
> 30× drop was observed in ws-trace while the keep-warm worker
> held all 7 drives in Warm across 24 h, and `pm_bytes`
> (commit-charge) stayed essentially flat throughout.  Both
> soaks' WS drops are the benign Phase 5 G2 `EmptyWorkingSet`
> page-trim, not silent idle-decay or leak.  See §4.5d for the
> full `ws_bytes`-vs-`pm_bytes` breakdown.

**Phase 7 closes retroactively** — no new 24-h soak required.
The validator-only re-run can be done by replaying the existing
`daemon.log` through the fixed `scripts/dev/long-soak.rs`
`validate_phase7` (manual `grep` shown above suffices for the
acceptance bar).

### 4.5d 2026-05-13 ws-trace 24-h capture — ALL GREEN with a measurement caveat

Source: `LOG/uffs_soak/wstrace-20260513-113344/` (24-h
observe-only Working-Set trajectory on the 7-drive reference
box, May 13-14 2026).

Run summary: **4 of 4 harness assertions PASS** with a
measurement nuance worth documenting before the v0.6.0 cut.

| Assertion (from `memory-tiering-bake-criteria.md` §1.7) | 24-h evidence | Status |
|---|---|---|
| ≥ 20 hourly snapshots captured | 24 snapshots in `snapshots/*-process.json` + `snapshots/*-status-drives.txt` | ✅ |
| Keep-warm worker fired ≥ 216 probes | 289 / 289 fired, zero errors in `keep-warm.log` | ✅ |
| Daemon PID at hour 24 == PID at hour 0 | PID 50492 across all 24 samples | ✅ |
| Working Set at hour 24 ≤ 1.5× Working Set at hour 0 | first=5 367 414 784 B, last=184 193 024 B, ratio=**0.03×** | ✅ |

**The catch: `ws_bytes` is NOT the right proxy for "no leak" on
Windows.**  `wstrace.csv` shows a 30× drop in `ws_bytes` at the
03h → 04h boundary (5.37 GB → 160 MB), but at the same sample:

* `pm_bytes` (Private Memory Size, the OS's commit-charge for
  the process) actually **increased** by ~870 MB (6.53 GB →
  7.40 GB), then settled to 6.36 GB by 06h and held there for
  the remaining 18 h.
* The daemon's own per-drive RESIDENT accounting in
  `04h-status-drives.txt` is **identical** to
  `03h-status-drives.txt`: all 7 drives still Warm with the
  same ~5.0 GiB cumulative body-Arc footprint (C=715 MiB,
  D=1.30 GiB, E=485 MiB, F=466 MiB, G=1 MiB, M=311 MiB,
  S=1.36 GiB).
* The keep-warm worker fired without errors across the
  03h → 04h boundary (probes #37-#48 all `OK` in
  `keep-warm.log`).

**Conclusion:** the WS drop is the **`EmptyWorkingSet`
page-trim mechanism** (the Phase 5 G2 wiring via
`crate::cache::working_set_trim::WorkingSetTrim`), not a tier
transition or memory release.  Pages moved from the daemon's
resident WS into the OS standby list; underlying private bytes
stayed allocated and the data is still arc-held on heap.  On
next access the pages re-fault from standby with no disk I/O.

Three orthogonal readings of the same 24 h:

```
Working Set     :  5 367 414 784 B  →    184 193 024 B   (0.03×)  ← OS view, not a leak signal
Private Memory  :  6 534 524 928 B  →  6 355 034 112 B   (0.97×)  ← commit-charge, real leak signal
Daemon RESIDENT :          5.0 GiB  →         5.0 GiB    (1.00×)  ← daemon's body-Arc accounting
```

The daemon **decreased** committed memory by 3 % over 24 h
while holding 7 drives in WARM and serving 289 probes.  No leak.

**Implication for the §4.5c Phase 7 footnote.**  The "vacuous
pass" concern raised there (Phase 7's WS ratio = 0.00× being
suspicious because the daemon might have demoted everything to
Parked) is **resolved**.  ws-trace's keep-warm worker held the
daemon in Warm steady-state across 24 h and the daemon still
showed the same 30× WS drop — so the Phase 7 WS drop is the
same benign `EmptyWorkingSet` page-trim, not silent idle-decay.
Both soaks were healthy.

**Recommended future refinement (NOT a v0.6.0 blocker).**
`scripts/dev/long-soak.rs` should additionally surface
`pm_bytes` and re-anchor the assertion on it, since on Windows
it's the leak-relevant metric.  Tracked informally for
post-v0.6.0; not a blocker because:

1. `pm_bytes` is already captured in every `Get-Process`
   snapshot (`snapshots/*-process.json` has it under the `PM`
   field) — the evidence is on disk regardless of which field
   the assertion uses.
2. The current `ws_bytes` assertion still catches a real leak
   reliably: any process that's leaking would grow both WS and
   private bytes monotonically.  The "0.03× ratio" pass is
   directionally correct (no growth = no leak), just
   numerically misleading.

**ws-trace closes** — all 4 assertions PASS end-to-end on the
existing capture; no re-run needed.

### 4.5e 2026-05-14 Phase 6 24-h re-run — ALL GREEN end-to-end

Source: `LOG/uffs_soak/phase6-20260514-122946/` (24-h run on the
7-drive reference box, 2026-05-14 12:29:46Z → 2026-05-15
12:29:46Z, against the post-PR-218 harness fix).

Run summary: **9 of 9 harness assertions PASS** — the §4.5b
deferred adaptive-bonus criterion now has direct end-to-end
evidence.

| Contract (from §2 above) | 24-h evidence | Status |
|---|---|---|
| 1. `C` never demotes below `Warm` | 0 `to=Parked` events for letter=C; **2 870** `Demote target clamped by per-drive min_tier` debug events | ✅ end-to-end verified |
| 2. Peer drives demote `Warm → Parked` normally | D / E / F / G / M / S each fired **2** Warm→Parked transitions | ✅ end-to-end verified |
| 3. Adaptive TTL bonus (`+600·log2(rate)`) engages under load | C.max_warm_ttl=**3 786 s** vs max(peers.max_warm_ttl)=**300 s** — bonus = **12.6× peer baseline** | ✅ **end-to-end verified** (was §4.5b ⚠️) |

**Why this re-run was needed.**  The §4.5b deferral was a
harness-side log-level filter: the `RUST_LOG=shard.ttl=debug`
filter dropped every `below-ttl` TRACE event, which is the only
event class that carries the bonused `warm_ttl_sec` field
during the synthetic-load window (drive C sits in Warm/Hot
with `idle_secs ≈ 0`, so the demote-eval ladder never reaches
either DEBUG arm).  PR #218 raised the harness filter to
`shard.ttl=trace`; this re-run is the first 24-h soak captured
against that fix.

**Memory trajectory.**  In addition to the 9 contract
assertions, the hourly process snapshots show the tiering
machinery doing real work:

```
                          00h           23h         post-load
Working Set (WS) :  6 746 800 128  →  22 888 448  →  69 238 784  (308× WS trim, 3× post-load)
Private Memory   :  8 293 457 920  → 1 791 188 992 → 1 669 558 272 (78 % drop, real release)
Virtual Memory   : 28 172 120 064  → 28 168 974 336 → 28 168 974 336 (flat — no address-space leak)
NPM (non-paged)  :         26 736  →        26 328 →         26 328 (flat)
```

This is materially different from §4.5d's ws-trace
trajectory, where `pm_bytes` stayed within 3 % over 24 h
because the keep-warm worker held all 7 drives in Warm.  Here
the controller actively demoted the 6 peer drives Warm → Parked
twice each, which unloaded cold shards from memory and produced
the **78 % private-memory release** — exactly the intended
tiering-machinery behavior under sustained idle.

The synthetic-load window at end-of-soak then re-paged
recently-needed shards back into WS (22 MiB → 66 MiB, 3×)
without growing private memory (which actually continued to
fall slightly), confirming the page-cache vs. private-bytes
split is healthy: the daemon brings pages back from standby
on access rather than re-allocating.

**Operational signals (informational, not gate criteria).**

* `daemon.log` size: 530 454 lines / 78 MiB.  Bulk of the
  volume is the expected `journal_loop=info` per-tick TRACE on
  drives whose USN journal isn't active (M and G — non-system
  drives with no journal enabled, a benign Win32 error 1179
  surfaced as `WARN Journal poll failed; retrying next tick`).
  The PR-#218 `shard.ttl=trace` filter added an additional
  ~23 k events as predicted in §4.5b.
* `0` `panic` / `OutOfMemoryError` / `FATAL` log lines across
  24 h — same clean-run shape as §4.5c Phase 7.
* All 24 hourly snapshots captured (`snapshots/00h-process.json`
  through `snapshots/23h-process.json` plus
  `snapshots/post-load-process.json`).
* All 9 `validation/*.pass` breadcrumb files written.

**Phase 6 closes** — 9 of 9 assertions PASS end-to-end against
the live Windows host; no further re-runs needed.  The
`bake-criteria.md` §3 Phase-6 checkbox can now be ticked.
Combined with §4.5c (Phase 7 closes retroactively), §4.5d
(ws-trace closes), and the existing Phase 8 closure from
2026-05-05, this exhausts the v0.6.0 24-h-soak gate slate.
Only the one-week `main` bake remains per
[`memory-tiering-bake-criteria.md`](memory-tiering-bake-criteria.md).

### 4.6 Lifecycle load-stall force-retire at 2 h 11 min (Phase 7 scope)

Source: `LOG/uffsd-G4-bonus.log` line 104 — at 01:17:15 (2 h 11 min
after daemon start), the lifecycle controller logged:

```text
2026-05-03T01:17:15.019200Z ERROR Load stalled — no drive progress, \
    force-retiring stall_secs=300 heartbeat_age_secs=7864
2026-05-03T01:17:15.019985Z  INFO Daemon shutting down
```

Root cause: `LifecycleManager::run_idle_timer` interprets "no
drive-loading progress" as a stall, but a daemon serving zero
queries against 7 fully-Parked drives **has** no drive-loading
progress to make — the heartbeat hasn't ticked because there's
nothing to do.  This is a **Phase 7 / lifecycle-controller scope
item**, not a Phase 5 regression: the load-stall semantics need
to distinguish "load incomplete, no progress" (legitimate stall)
from "load complete, no demand" (legitimately idle).  Tracked in
[`crates/uffs-daemon/src/lifecycle.rs`][lifecycle-rs] (file-size
permanent-exception, see `scripts/ci/file_size_exceptions.txt`).
G4 acceptance bar (no OOM through 60 min) was met independently
of this — the 01:17 force-retire happened at the 2 h 11 min mark,
well past the gate window.

[lifecycle-rs]: ../../crates/uffs-daemon/src/lifecycle.rs
