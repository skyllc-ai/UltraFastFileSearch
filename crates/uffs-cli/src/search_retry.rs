// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Client-side resilience for a search that races a parked-drive re-warm.
//!
//! When the daemon has idled long enough to **park** drives (bodies evicted,
//! only the bloom + path trie resident), the first search re-warms them. On
//! Windows that re-warm reads MFT extents over the broker's overlapped handle,
//! and a pending read can be cancelled with `ERROR_OPERATION_ABORTED` (os error
//! 995) when the worker thread that issued it is reaped. The daemon's own read
//! primitive ([`uffs-mft`'s `read_handle_at`]) already retries that transient;
//! this is the client-side belt-and-suspenders so a user never sees a raw I/O
//! error for a recoverable warm-up hiccup. The retry is silent: the caller
//! waits for a result and gets nothing else; the daemon logs the re-warm.
//!
//! Only an error the **daemon** reported is a candidate for the retry.  The
//! client's own per-RPC deadline (`UFFS_CLIENT_TIMEOUT_SECS`, enforced on
//! Windows through `CancelSynchronousIo`, which also yields os error 995 on
//! the cancelled read) surfaces as [`ClientError::Timeout`] and is never
//! retried: the daemon is still executing that search, and re-sending it
//! would only queue another full scan behind the first — the 2026-10-03
//! benchmark run did exactly that, five `*.*` scans for three invocations.

// Each helper is used exactly once on the search path; this is cohesion, not a
// smell — the `single_call_fn` restriction lint is relaxed for the module.
#![expect(
    clippy::single_call_fn,
    reason = "search-retry helpers, each called once on the search hot path"
)]

use uffs_client::connect_sync::UffsClientSync;
use uffs_client::error::ClientError;

/// Max client-side retries when a search aborts mid-index-warm.
const WARM_RETRY_MAX: u32 = 5;
/// Backoff between warm-retry attempts.
const WARM_RETRY_BACKOFF: core::time::Duration = core::time::Duration::from_millis(400);

/// Run `search_cli_raw`, transparently retrying the one transient a search can
/// hit while the daemon re-warms parked drives: a daemon-reported Windows
/// `ERROR_OPERATION_ABORTED` (os error 995).
///
/// Bounded + back-off so a genuine, persistent failure still fails fast.  A
/// [`ClientError::Timeout`] is returned as-is on the first occurrence — see the
/// module docs for why the client's own deadline must never trigger a retry.
pub(crate) fn search_cli_with_warm_retry(
    client: &mut UffsClientSync,
    args: &[String],
) -> Result<serde_json::Value, ClientError> {
    let mut attempt = 0_u32;
    loop {
        match client.search_cli_raw(args) {
            Ok(response) => return Ok(response),
            Err(err) if attempt < WARM_RETRY_MAX && is_index_warming_abort(&err) => {
                // Silent on purpose: the caller waits for a result and gets
                // nothing else; the daemon logs the re-warm it raced.
                attempt += 1;
                std::thread::sleep(WARM_RETRY_BACKOFF);
            }
            Err(err) => return Err(err),
        }
    }
}

/// `true` if `err` is a **daemon-reported** transient `ERROR_OPERATION_ABORTED`
/// (os error 995) — what a search hits when it races a parked-drive re-warm.
/// Matched on the stable OS error code in the rendered message — the daemon's
/// typed I/O error is flattened to a string across the JSON-RPC boundary, so
/// the code is the only portable signal left.
///
/// A client-side transport error never qualifies, whatever its text: the only
/// client-side source of os error 995 is the deadline watchdog cancelling our
/// own read, and the sync client already reports that as
/// [`ClientError::Timeout`].
fn is_index_warming_abort(err: &ClientError) -> bool {
    let ClientError::DaemonError { message, .. } = err else {
        return false;
    };
    message.contains("os error 995") || message.contains("operation has been aborted")
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::default_numeric_fallback,
        reason = "test module — relaxed linting"
    )]

    use uffs_client::error::ClientError;

    use super::is_index_warming_abort;

    /// Only a daemon-reported transient `ERROR_OPERATION_ABORTED` (os error
    /// 995) — what a search hits while racing a parked-drive re-warm — should
    /// be retried; real errors must surface immediately.
    #[test]
    fn warming_abort_matches_only_daemon_reported_995() {
        assert!(is_index_warming_abort(&ClientError::DaemonError {
            code: -32000,
            message: "I/O error: ... (os error 995)".to_owned(),
        }));
        assert!(is_index_warming_abort(&ClientError::DaemonError {
            code: -32000,
            message: "The I/O operation has been aborted because of either a thread exit \
                      or an application request."
                .to_owned(),
        }));
        // Real failures are NOT retried.
        assert!(!is_index_warming_abort(&ClientError::DaemonError {
            code: -32000,
            message: "permission denied (os error 5)".to_owned(),
        }));
        assert!(!is_index_warming_abort(&ClientError::Io(
            "permission denied (os error 5)".to_owned()
        )));
        assert!(!is_index_warming_abort(&ClientError::ConnectionClosed));
    }

    /// The client's own deadline is not a warm-up transient.  On Windows the
    /// watchdog's `CancelSynchronousIo` makes our blocked read fail with the
    /// same os error 995 the daemon transient carries; the sync client maps
    /// that to `Timeout`, and a raw client-side `Io` 995 (a thread-exit
    /// cancellation outside the guard) must not be retried either — the
    /// daemon is still running the search we just abandoned.
    #[test]
    fn client_side_cancellation_is_never_retried() {
        assert!(!is_index_warming_abort(&ClientError::Timeout));
        assert!(!is_index_warming_abort(&ClientError::Io(
            "The I/O operation has been aborted because of either a thread exit \
             or an application request. (os error 995)"
                .to_owned()
        )));
    }
}
