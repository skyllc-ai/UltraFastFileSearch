// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The typed non-completion of a daemon search.
//!
//! Split from [`super::search`] for the 800-LOC file-size policy; the
//! only consumer-facing surface is [`SearchFailure`], re-exported there.

use core::fmt;

/// Why [`crate::index::IndexManager::run_search_over`] produced no response.
///
/// Every variant used to come back as a success-shaped
/// [`uffs_client::protocol::response::SearchResponse`]
/// with zero rows and `truncated: false`, indistinguishable from "nothing
/// matched": the 2026-10-03 benchmark run read two scan timeouts as
/// "0 results".  Each is now a JSON-RPC error with its own code (see
/// [`Self::rpc_code`]) and an operator-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchFailure {
    /// The per-search scan budget expired.  The scan was cancelled
    /// cooperatively (`SearchFilters::cancel`) and its partial result
    /// discarded.
    TimedOut {
        /// The budget that expired, in seconds
        /// (`UFFS_SEARCH_TIMEOUT_SECS`).
        budget_secs: u64,
        /// Milliseconds spent paging parked / cold drives back in
        /// before the scan started — separate from the budget, and the
        /// first thing to look at when a timeout follows an idle
        /// stretch.
        promotion_ms: u64,
    },
    /// The blocking scan task panicked; details are in the daemon log.
    Panicked {
        /// Milliseconds spent on index warm-up before the scan.
        promotion_ms: u64,
    },
    /// No search permit became available within the wait: the
    /// concurrency cap (`UFFS_SEARCH_MAX_CONCURRENCY`) is saturated.
    /// Nothing was scanned.
    Saturated,
    /// The pattern cannot be searched for (an invalid `>regex`); nothing
    /// was scanned.  The caller's mistake, reported as invalid params.
    InvalidPattern(uffs_core::search::SearchError),
}

impl SearchFailure {
    /// The JSON-RPC error code the client receives for this failure.
    pub(crate) const fn rpc_code(&self) -> i32 {
        match self {
            Self::TimedOut { .. } => uffs_client::protocol::ERR_SEARCH_TIMEOUT,
            Self::Panicked { .. } => uffs_client::protocol::ERR_INTERNAL,
            Self::Saturated => uffs_client::protocol::ERR_SEARCH_BUSY,
            Self::InvalidPattern(_) => uffs_client::protocol::ERR_INVALID_PARAMS,
        }
    }
}

impl fmt::Display for SearchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut {
                budget_secs,
                promotion_ms,
            } => write!(
                f,
                "search exceeded the daemon scan budget of {budget_secs}s and was cancelled \
                 (index warm-up before the scan: {promotion_ms} ms); narrow the query, or raise \
                 UFFS_SEARCH_TIMEOUT_SECS on the daemon"
            ),
            Self::Panicked { promotion_ms } => write!(
                f,
                "search task panicked on the daemon (index warm-up before the scan: \
                 {promotion_ms} ms); see the daemon log"
            ),
            Self::Saturated => write!(
                f,
                "daemon search slots are saturated (UFFS_SEARCH_MAX_CONCURRENCY); nothing was \
                 scanned, retry shortly"
            ),
            Self::InvalidPattern(err) => write!(f, "{err}"),
        }
    }
}
