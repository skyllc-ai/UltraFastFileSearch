// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Restore (design §10): relaunch services from the captured snapshot
//! recipe, **providers before consumers** (broker → daemon → MCP).
//!
//! This is also the INV-1 backstop: the apply rollback path and Phase H
//! call it to make sure nothing the run stopped is left down. So it is
//! **best-effort per component** — a failure to restart one is reported
//! but never blocks restarting the others.

use std::process::Command;

use uffs_broker_protocol::{PIPE_NAME, SERVICE_NAME};

use crate::plan::{SnapRunning, Snapshot};
use crate::quiesce::{daemon_pid_file, wait_until};

/// How long to wait for a service to come back up.
const START_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(20);

/// How long to wait for the broker's named pipe to begin serving (ms).
const PIPE_READY_TIMEOUT_MS: u32 = 10_000;

/// Restart every component that was running, provider→consumer. Returns
/// the components that failed to restart (empty = all good).
pub(crate) fn restore(snapshot: &Snapshot) -> Vec<String> {
    let mut failed = Vec::new();
    for component in ["broker", "daemon", "mcp"] {
        for running in snapshot
            .running
            .iter()
            .filter(|run| run.component == component)
        {
            if !start_component(component, running) {
                failed.push(component.to_owned());
            }
        }
    }
    failed
}

/// Start one component; `true` on (best-effort) success.
fn start_component(component: &str, running: &SnapRunning) -> bool {
    match component {
        "broker" => start_broker(),
        "daemon" => start_from_command_line(running, || daemon_pid_file().exists()),
        // The stdio MCP server (`uffsmcp`) is spawned and OWNED by its LLM host
        // (Claude Desktop / Cursor / Windsurf) over stdio — relaunching it
        // detached has no client to serve. Quiesce stopped it only to free the
        // `uffsmcp.exe` file lock for the swap; the host respawns it on the
        // next tool call. So restoring mcp is a deliberate no-op (and a
        // success, not a "failed to restart"). The HTTP gateway is a separate
        // binary (`uffs-mcp-http`), never captured here — detection scans only
        // for `uffsmcp`.
        "mcp" => true,
        _ => false,
    }
}

/// Start the broker via native SCM (waits for RUNNING internally), **then**
/// wait until the pipe is actually serving (R10, §19.13). Service-RUNNING
/// is necessary but not sufficient: the daemon's warm-up hits
/// `ERROR_PIPE_BUSY` if it connects before the broker's pipe is listening.
///
/// Idempotent: if the broker is **already serving**, it's up — and a
/// non-elevated caller can neither need nor (via SCM) perform a start of a
/// `LocalSystem` service. Treat that as success so a redundant, elevation-gated
/// `uffs_winsvc::start` failure can't surface as a fault on a healthy broker.
fn start_broker() -> bool {
    if broker_pipe_ready(PIPE_READY_TIMEOUT_MS) {
        return true;
    }
    uffs_winsvc::start(SERVICE_NAME).is_ok() && broker_pipe_ready(PIPE_READY_TIMEOUT_MS)
}

/// Wait up to `timeout_ms` for the broker's named pipe to begin serving,
/// via `uffs-winsvc`'s non-connecting `WaitNamedPipe` probe (R10, §19.13).
/// `true` off Windows (no pipe exists).
pub(crate) fn broker_pipe_ready(timeout_ms: u32) -> bool {
    uffs_winsvc::pipe_serving(PIPE_NAME, timeout_ms)
}

/// Relaunch a process from its captured `command_line`, detached, then
/// wait for `ready` (e.g. the daemon's PID file to reappear).
fn start_from_command_line(running: &SnapRunning, ready: impl Fn() -> bool) -> bool {
    let Some(cmd) = running.command_line.as_deref() else {
        return false;
    };
    let Some((program, args)) = parse_command_line(cmd) else {
        return false;
    };
    // Spawn detached: don't wait, so the relaunched service outlives us.
    let mut command = Command::new(&program);
    command.args(&args);
    detach_stdio(&mut command);
    let spawned = command.spawn().is_ok();
    spawned && wait_until(START_TIMEOUT, &ready)
}

/// Sever the relaunched service from this process's console.
///
/// The captured command line replays the daemon's argv but NOT the stdio
/// redirection the normal `--daemon start` path applies via the client's
/// detached spawn. Without this, a service relaunched here inherits the
/// updater's console: the daemon's `info` tracing (and any startup prints)
/// pour into the user's terminal long after `uffs --update` returns. Null
/// all three handles, and on Windows also set `DETACHED_PROCESS |
/// CREATE_NO_WINDOW` so no console is attached at all.
fn detach_stdio(command: &mut Command) {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `DETACHED_PROCESS` — the child gets no console.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        /// `CREATE_NO_WINDOW` — belt-and-suspenders against a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
}

/// Split a captured command line into `(program, args)`.
///
/// Naive whitespace split — sufficient for UFFS's switch-style argv
/// (`uffsd --no-retire --drive C,D`). Pure → testable.
pub(crate) fn parse_command_line(cmd: &str) -> Option<(String, Vec<String>)> {
    let mut parts = cmd.split_whitespace().map(ToOwned::to_owned);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

#[cfg(test)]
mod tests {
    use super::{parse_command_line, start_component};
    use crate::plan::SnapRunning;

    #[test]
    fn mcp_restore_is_noop_success() {
        // A stdio `uffsmcp` is client-owned: restore must not relaunch it,
        // and must report success (not a "failed to restart") so the commit
        // path doesn't warn about a component it deliberately leaves alone.
        let running = SnapRunning {
            component: "mcp".to_owned(),
            pid: 1,
            image_path: None,
            command_line: Some("uffsmcp".to_owned()),
        };
        assert!(start_component("mcp", &running));
    }

    #[test]
    fn parses_switch_style_command_line() {
        let (program, args) = parse_command_line("uffsd --no-retire --drive C,D").expect("parse");
        assert_eq!(program, "uffsd");
        assert_eq!(args, vec!["--no-retire", "--drive", "C,D"]);
    }

    #[test]
    fn parses_program_only() {
        let (program, args) = parse_command_line("uffsmcp").expect("parse");
        assert_eq!(program, "uffsmcp");
        assert_eq!(args, Vec::<String>::new(), "program-only line has no args");
    }

    #[test]
    fn empty_command_line_is_none() {
        assert!(parse_command_line("   ").is_none());
    }
}
