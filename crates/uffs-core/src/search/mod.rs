// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search engine: backend, sort, filters, query routing, tree walk.
//!
//! This module contains the compact-index search infrastructure shared
//! between the TUI, daemon, CLI, and any future surface.

pub mod backend;
pub mod bloom_skip;
pub mod columns;
mod dataframe_convert;
pub mod derived;
mod dispatch;
mod display_row;
pub mod field;
pub mod filters;
pub mod query;
mod search_error;
mod sorting;
pub mod tree;

pub use search_error::SearchError;
