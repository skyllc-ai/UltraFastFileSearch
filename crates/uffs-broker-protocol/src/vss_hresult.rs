// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The VSS `HRESULT`s the snapshot pipeline treats specially, each
//! pinned to its `vsserror.h` name — the one table every UFFS component
//! classifies a VSS failure by.
//!
//! Lives in the protocol crate because the `HRESULT` crosses the wire:
//! `uffs-vss-requestor` decides here whether to retry inside the Broker,
//! and the Broker's `SnapshotManagerResponse::Error` hands the same code
//! to the Content Coordinator (uffs-products' `uffs-content`), which
//! decides whether its own consumer should retry.  The two used to keep
//! separate copies of this table, which had to be edited together or
//! drifted silently; one shared table removes the need.
//!
//! Platform-independent on purpose: the code ↔ name mapping is plain
//! data, so its tests run on every host instead of only on Windows CI.
//! Until 2026-10-13 the names lived only in comments beside a bare
//! array, and two of them were transposed — `0x80042317` /
//! `0x80042318` were labelled as the hold/flush-writes timeouts, which
//! are really `0x80042314` / `0x80042313`.  The real timeouts were
//! therefore never retried, while "maximum snapshots reached" and
//! "writer infrastructure" were retried under false names.  Every
//! value below is a named constant asserted against the header in
//! this module's tests, so a re-transposition fails loudly.

/// `HRESULT` values are compared as the signed `i32` the native shim
/// returns; this keeps every constant below spelled in the header's
/// hex form.
const fn hr(code: u32) -> i32 {
    code.cast_signed()
}

/// `VSS_E_PROVIDER_VETO` — the provider could not service the request
/// right now.
///
/// One of the errors Microsoft's requestor guidance says to retry.
/// Observed on real hardware from back-to-back snapshot
/// create/release cycles on the same volume.
pub const VSS_E_PROVIDER_VETO: i32 = hr(0x8004_2306);
/// `VSS_E_FLUSH_WRITES_TIMEOUT` — the flush phase did not finish in
/// time; the classic symptom of a busy volume.
pub const VSS_E_FLUSH_WRITES_TIMEOUT: i32 = hr(0x8004_2313);
/// `VSS_E_HOLD_WRITES_TIMEOUT` — the write-freeze phase did not finish
/// in time; the other half of snapshot contention.
pub const VSS_E_HOLD_WRITES_TIMEOUT: i32 = hr(0x8004_2314);
/// `VSS_E_SNAPSHOT_SET_IN_PROGRESS` — another shadow-copy operation is
/// still in flight on this volume.
pub const VSS_E_SNAPSHOT_SET_IN_PROGRESS: i32 = hr(0x8004_2316);
/// `VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED` — the volume already
/// holds the maximum number of shadow copies.
///
/// Not transient: retrying
/// cannot help while the limit is reached; deleting an old shadow copy
/// does.  See [`failure_hint`].
pub const VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED: i32 = hr(0x8004_2317);
/// `VSS_E_WRITER_INFRASTRUCTURE` — the VSS writer infrastructure is not
/// operating properly.  Not a transient a retry can clear.
pub const VSS_E_WRITER_INFRASTRUCTURE: i32 = hr(0x8004_2318);
/// `VSS_E_WRITERERROR_TIMEOUT` — a writer timed out; retryable by
/// definition.
pub const VSS_E_WRITERERROR_TIMEOUT: i32 = hr(0x8004_23F2);
/// `VSS_E_WRITERERROR_RETRYABLE` — a writer reported a retryable error.
pub const VSS_E_WRITERERROR_RETRYABLE: i32 = hr(0x8004_23F3);

/// `HRESULT`s a fresh attempt is expected to clear once the underlying
/// contention passes.
///
/// A fresh attempt means a brand-new `IVssBackupComponents` session,
/// which is what every `VssSnapshotSession::create` call already does.
/// Every other failure (unsupported volume, bad arguments, access
/// denied, maximum snapshots reached, …) fails immediately — retrying
/// those only spends the backoff budget on something that cannot
/// succeed.  The two writer codes are here for completeness: this
/// requestor runs `VSS_CTX_FILE_SHARE_BACKUP` with no writer
/// coordination (see `native/vss_shim.cpp`), so writers should never be
/// in play, but both codes are retryable by definition if they appear.
pub const RETRYABLE: [i32; 6] = [
    VSS_E_PROVIDER_VETO,
    VSS_E_FLUSH_WRITES_TIMEOUT,
    VSS_E_HOLD_WRITES_TIMEOUT,
    VSS_E_SNAPSHOT_SET_IN_PROGRESS,
    VSS_E_WRITERERROR_TIMEOUT,
    VSS_E_WRITERERROR_RETRYABLE,
];

/// `true` when `hresult` is one of the [`RETRYABLE`] transients.
#[must_use]
pub fn is_retryable(hresult: i32) -> bool {
    RETRYABLE.contains(&hresult)
}

/// An operator-facing remedy for a permanent failure that has one.
///
/// Appended to the failure message the Broker receives, so a caller
/// that can act on it is told what to do instead of seeing a generic
/// failure.  `None` for every code without a specific remedy.
#[must_use]
pub const fn failure_hint(hresult: i32) -> Option<&'static str> {
    match hresult {
        VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED => Some(
            "the volume already holds the maximum number of shadow copies; delete an old one \
             (e.g. `vssadmin delete shadows /for=<volume> /oldest`) and retry",
        ),
        VSS_E_WRITER_INFRASTRUCTURE => Some(
            "the VSS writer infrastructure is not operating properly; check `vssadmin list \
             writers` and the Application event log, or restart the VSS service",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every constant against the value `vsserror.h` defines for its
    /// name (mingw-w64 `any-windows-any/vsserror.h`, the same values as
    /// the Windows SDK).  This is the assertion whose absence let the
    /// original table transpose two codes.
    #[test]
    fn each_code_matches_its_vsserror_h_name() {
        let header: [(&str, i32, u32); 8] = [
            ("VSS_E_PROVIDER_VETO", VSS_E_PROVIDER_VETO, 0x8004_2306),
            (
                "VSS_E_FLUSH_WRITES_TIMEOUT",
                VSS_E_FLUSH_WRITES_TIMEOUT,
                0x8004_2313,
            ),
            (
                "VSS_E_HOLD_WRITES_TIMEOUT",
                VSS_E_HOLD_WRITES_TIMEOUT,
                0x8004_2314,
            ),
            (
                "VSS_E_SNAPSHOT_SET_IN_PROGRESS",
                VSS_E_SNAPSHOT_SET_IN_PROGRESS,
                0x8004_2316,
            ),
            (
                "VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED",
                VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED,
                0x8004_2317,
            ),
            (
                "VSS_E_WRITER_INFRASTRUCTURE",
                VSS_E_WRITER_INFRASTRUCTURE,
                0x8004_2318,
            ),
            (
                "VSS_E_WRITERERROR_TIMEOUT",
                VSS_E_WRITERERROR_TIMEOUT,
                0x8004_23F2,
            ),
            (
                "VSS_E_WRITERERROR_RETRYABLE",
                VSS_E_WRITERERROR_RETRYABLE,
                0x8004_23F3,
            ),
        ];
        for (name, constant, from_header) in header {
            assert_eq!(
                constant.cast_unsigned(),
                from_header,
                "{name} must be {from_header:#010X} per vsserror.h"
            );
        }
    }

    /// The freeze/flush timeouts are retried; the two codes the old
    /// table retried under their names are not.
    #[test]
    fn timeouts_are_retried_and_the_transposed_codes_are_not() {
        assert!(is_retryable(VSS_E_FLUSH_WRITES_TIMEOUT));
        assert!(is_retryable(VSS_E_HOLD_WRITES_TIMEOUT));
        assert!(!is_retryable(VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED));
        assert!(!is_retryable(VSS_E_WRITER_INFRASTRUCTURE));
    }

    /// The retry set is exactly the six transients.
    #[test]
    fn retry_set_is_exactly_the_six_transients() {
        let mut codes: Vec<u32> = RETRYABLE.iter().map(|code| code.cast_unsigned()).collect();
        codes.sort_unstable();
        assert_eq!(codes, [
            0x8004_2306,
            0x8004_2313,
            0x8004_2314,
            0x8004_2316,
            0x8004_23F2,
            0x8004_23F3
        ]);
    }

    /// The actionable permanent failures carry a remedy; transients and
    /// unknown codes carry none.
    #[test]
    fn hints_only_for_actionable_permanent_failures() {
        assert!(
            failure_hint(VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED)
                .is_some_and(|hint| hint.contains("delete an old"))
        );
        assert!(failure_hint(VSS_E_WRITER_INFRASTRUCTURE).is_some());
        assert!(RETRYABLE.iter().all(|&code| failure_hint(code).is_none()));
        assert!(failure_hint(0).is_none());
    }
}
