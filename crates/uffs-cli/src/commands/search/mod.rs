// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search command — the thin-client entry point and its output helpers.
//!
//! All searches route through the UFFS daemon via `search_cli` RPC.
//! `run_search` owns the round trip; the submodules own the argument
//! transforms on the way in and the output formatting on the way out.

/// Argument transforms (spawn-arg extraction, `--out` resolution,
/// NUL-stdout `--no-output` injection).
pub(crate) mod args;
/// Output dispatch and formatting.
pub mod dispatch;
/// The `search_cli` round trip and payload-to-stdout writer.
mod run;
/// The error for a search the client stopped waiting for.
mod unfinished;

pub(crate) use run::run_search;
