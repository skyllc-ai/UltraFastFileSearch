// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS VSS Requestor — per-run native VSS snapshot helper.
//!
//! Spawned once per volume scan by `uffs-broker`'s Snapshot Manager, per
//! `docs/dev/architecture/uffs-vss-rust-cpp-shim-implementation-guide.md`.
//! Creates one `VSS_CTX_FILE_SHARE_BACKUP` snapshot (ephemeral,
//! auto-release, no writer participation), holds the VSS requestor
//! session alive for the whole scan, and exits only after an explicit
//! `Release`/`Cancel` over its private control pipe, the pipe closing,
//! or its parent Broker process dying (this process's own watchdog is a
//! second, independent safety net alongside the Job Object the Broker
//! assigns it to).
//!
//! # Usage
//!
//! ```bash
//! uffs-vss-requestor --version
//! uffs-vss-requestor --pipe-name <name> --volume-path <path> --parent-pid <pid>
//! ```
//!
//! Not meant to be run manually outside of debugging — the pipe name
//! and parent PID are private, Broker-assigned values.

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod protocol;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod snapshot;

#[expect(
    clippy::print_stderr,
    reason = "no tracing subscriber in this tiny helper; stderr is the only \
              diagnostic channel and is captured by the spawning Broker"
)]
fn main() {
    uffs_version::handle_version!("uffs-vss-requestor");

    #[cfg(windows)]
    {
        if let Err(run_err) = run::run() {
            eprintln!("uffs-vss-requestor: {run_err:#}");
            std::process::exit(1);
        }
    }

    #[cfg(not(windows))]
    {
        eprintln!("uffs-vss-requestor is a Windows-only component.");
        std::process::exit(1);
    }
}
