// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Data model for self-update **Phase A — detect & capture** (see
//! `docs/dev/architecture/UFFS-Self-Update-Feasibility-and-Design.md` §5).
//!
//! Phase A discovers every install *root* (a directory holding ≥1 UFFS
//! binary) from three independent anchors — the invoking CLI, the
//! running daemon, and the running broker — then records, per root, the
//! channel that put it there and the on-disk version of each binary.
//! These types are the in-memory shape of that result; nothing here
//! mutates the system.

use std::path::PathBuf;

/// Install channel that placed the binaries at a discovered root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    /// Windows Package Manager (portable nested package under
    /// `…\WinGet\Packages\…`).
    WinGet,
    /// Hand-placed: a manual GitHub-release extract, a copied build, etc.
    Unmanaged,
    /// A cargo `target/{debug,release}` build tree — never auto-updated.
    DevBuild,
    /// Could not be classified from the path.
    Unknown,
}

impl Channel {
    /// Short human label for report output.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::WinGet => "winget",
            Self::Unmanaged => "unmanaged",
            Self::DevBuild => "dev-build",
            Self::Unknown => "unknown",
        }
    }
}

/// Install scope for a `WinGet` root (user- vs machine-wide).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Per-user install (`%LOCALAPPDATA%`).
    User,
    /// Machine-wide install (`%PROGRAMFILES%`).
    Machine,
    /// Scope not determined.
    Unknown,
}

impl Scope {
    /// Short human label for report output.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Machine => "machine",
            Self::Unknown => "-",
        }
    }
}

/// Which live anchor surfaced an install root (a root may be surfaced by
/// more than one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// The `uffs.exe` that invoked `--update` (`current_exe()`).
    Cli,
    /// The running daemon's image directory.
    Daemon,
    /// The running MCP gateway's image directory.
    Mcp,
    /// The registered broker-service `binPath` directory.
    Broker,
    /// Found by scanning the well-known `WinGet\Packages` location — a
    /// winget-managed install with nothing currently running from it.
    Winget,
}

impl Anchor {
    /// Short human label for report output.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Daemon => "daemon",
            Self::Mcp => "mcp",
            Self::Broker => "broker",
            Self::Winget => "winget",
        }
    }
}

/// One UFFS binary found inside an install root. Its on-disk path is
/// `root.dir.join(exe_file_name(name))` — derived on demand by later
/// phases rather than stored here.
#[derive(Debug, Clone)]
pub(crate) struct BinaryInfo {
    /// Logical stem (e.g. `uffsd`), without the platform `.exe` suffix.
    pub(crate) name: String,
    /// Parsed `--version` (e.g. `0.6.2`), or `None` if it could not be read.
    pub(crate) version: Option<String>,
}

/// A discovered install location (directory) holding ≥1 UFFS binary.
#[derive(Debug, Clone)]
pub(crate) struct InstallRoot {
    /// The directory itself (canonicalised where possible).
    pub(crate) dir: PathBuf,
    /// Channel that placed these binaries.
    pub(crate) channel: Channel,
    /// Install scope (meaningful mainly for `WinGet`).
    pub(crate) scope: Scope,
    /// Anchors that surfaced this root (deduplicated, insertion order).
    pub(crate) anchored_by: Vec<Anchor>,
    /// The UFFS binaries present in this root.
    pub(crate) binaries: Vec<BinaryInfo>,
}

impl InstallRoot {
    /// Record that `anchor` also surfaced this root, without duplicating.
    pub(crate) fn note_anchor(&mut self, anchor: Anchor) {
        if !self.anchored_by.contains(&anchor) {
            self.anchored_by.push(anchor);
        }
    }
}

/// Kind of running UFFS component discovered during anchor scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Component {
    /// The resident index server (`uffsd`).
    Daemon,
    /// The elevated handle broker service (`uffs-broker` / `UffsAccessBroker`).
    Broker,
    /// The MCP server (`uffsmcp`), when run as the HTTP gateway.
    Mcp,
    /// The resident-services supervisor (`uffs-watchdog`, Windows).  Stopped
    /// before the services it supervises, or it would revive them mid-update
    /// or mid-uninstall.
    Watchdog,
}

impl Component {
    /// Short human label for report output.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Broker => "broker",
            Self::Mcp => "mcp",
            Self::Watchdog => "watchdog",
        }
    }
}

/// A live UFFS process captured during Phase A — the basis for the
/// later stop/restart recipe.
#[derive(Debug, Clone)]
pub(crate) struct RunningProcess {
    /// Which component this process is.
    pub(crate) component: Component,
    /// Operating-system process id.
    pub(crate) pid: u32,
    /// Full path to the running image, if it could be resolved.
    pub(crate) image_path: Option<PathBuf>,
    /// The exact launch command line (image + all switches), if available.
    pub(crate) command_line: Option<String>,
    /// Parsed running version, if it could be read.
    pub(crate) version: Option<String>,
}

/// Full Phase-A detection result: the update *targets* (roots) plus the
/// live-process map (what to stop / how to restart).
#[derive(Debug, Clone, Default)]
pub(crate) struct DetectionReport {
    /// Deduplicated install roots = the update targets.
    pub(crate) roots: Vec<InstallRoot>,
    /// Live UFFS processes found via the anchors.
    pub(crate) running: Vec<RunningProcess>,
}
