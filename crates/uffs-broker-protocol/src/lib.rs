// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Wire protocol between `uffs-broker` (elevated handle vendor) and
//! `uffs-daemon::broker_client` (handle consumer).
//!
//! Also the Snapshot Manager messages ([`snapshot_manager`]) and the VSS
//! `HRESULT` table ([`vss_hresult`]) every component classifies a
//! snapshot failure by.
//!
//! This crate is a dedicated cross-platform Layer-0 library — pure
//! byte-shuffling, no Windows FFI, no I/O.  Both sides of the wire
//! (the broker binary on Windows, the daemon-side client also on
//! Windows) import the types defined here so the wire format has a
//! single source of truth.
//!
//! # Wire format (1-RTT, little-endian)
//!
//! Request: `[u8; 1]`
//!
//! | byte | semantics |
//! |---:|---|
//! | 0 | ASCII drive letter, case-folded to upper on parse (`b'A'..=b'Z'`) |
//!
//! Response: `[u8; 9]`
//!
//! | bytes | semantics |
//! |---:|---|
//! | 0     | [`Status`] (0 = `Ok`, 1 = `Error`) |
//! | 1..=8 | duplicated Windows `HANDLE` value, little-endian u64 — semantically meaningful only when `status == Ok` |
//!
//! When `status == Error`, the 8 handle bytes are unspecified and MUST
//! be ignored by the caller.  Implementations SHOULD zero them but are
//! not required to.
//!
//! # Why this module is cross-platform
//!
//! The wire protocol is pure-logic byte-shuffling; it has no Windows
//! FFI surface.  Keeping it in the `[lib]` (rather than gating it
//! behind `#[cfg(windows)]` inside the binary) means:
//!
//! 1. Linux + macOS PR-fast CI lanes execute the protocol tests on every commit
//!    (was previously: zero coverage on non-Windows hosts)
//! 2. Future fuzzing / mutation testing can target this module without a
//!    Windows runner
//! 3. The daemon (running on Windows in production but built on Linux in some
//!    dev workflows) can depend on the protocol module unconditionally
//!
//! See issue #205 +
//! `docs/dev/baseline/2026-05-12/f5_broker_test_coverage_decision.md`
//! (local-only) for the full F5 rust-master decision.

use thiserror::Error;

pub mod snapshot_manager;
pub mod vss_hresult;

/// Named-pipe path the broker listens on.
///
/// Both the broker server (`uffs-broker::broker`) and the daemon client
/// (`uffs-daemon::broker_client`) connect to this exact path.  Changing
/// it is a wire-format break: existing daemons would fail
/// `broker_available()` until they're rebuilt against the new constant.
///
/// On non-Windows platforms the string is still defined (so the
/// daemon's stub compiles), but no kernel object exists at that path.
pub const PIPE_NAME: &str = r"\\.\pipe\uffs-broker";

/// Registered Windows service name of the Access Broker.
///
/// The single source of truth shared by everything that names the
/// service: `uffs-broker` (install / control), `uffs-update`
/// (quiesce / restore), and `uffs-cli` (`uffs --status` + update
/// detection). It belongs here, next to [`PIPE_NAME`], because both are
/// the broker's *identity* on the wire / on the box — the SCM control
/// *mechanism* lives separately in `uffs-winsvc`.
///
/// Defined on every platform so cross-platform consumers compile; only
/// Windows has an actual service registered under it.
pub const SERVICE_NAME: &str = "UffsAccessBroker";

/// Total size of an encoded [`HandleRequest`] on the wire.
pub const REQUEST_WIRE_LEN: usize = 1;

/// Total size of an encoded [`HandleResponse`] on the wire.
pub const RESPONSE_WIRE_LEN: usize = 9;

/// Errors produced by parsing wire bytes into protocol types.
///
/// All variants are recoverable at the protocol layer — the caller can
/// surface them as a structured error to the user, log them, and
/// continue serving subsequent requests.  Encoding never fails: it's a
/// pure byte-layout operation.
///
/// `#[non_exhaustive]` is applied per Phase 5 §5c: future protocol
/// extensions (e.g. a `Truncated { expected, got }` variant when a
/// reader supplies fewer than `REQUEST_WIRE_LEN` / `RESPONSE_WIRE_LEN`
/// bytes, or a `RateLimited` variant once the wire format gains an
/// out-of-band error channel) can land as additive minor-version
/// bumps without breaking downstream exhaustive matchers.  Downstream
/// crates (`uffs-broker`, `uffs-daemon::broker_client`) currently
/// treat this error opaquely — verified zero exhaustive-match sites
/// workspace-wide at the Phase 5b audit (refs #192).
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolError {
    /// Drive-letter byte was outside the ASCII range (high bit set or
    /// otherwise non-ASCII).  The broker should respond with
    /// [`Status::Error`] and bail.
    #[error("non-ASCII drive letter byte: 0x{0:02x}")]
    NonAsciiDriveByte(u8),

    /// Drive-letter byte was ASCII but not alphabetic (e.g. `b'1'`,
    /// `b' '`, `b'\0'`, `b':'`).  The broker should respond with
    /// [`Status::Error`] and bail.
    ///
    /// Payload is the offending byte (always `<= 0x7F` because we
    /// passed the ASCII check first).  Use `byte as char` at the call
    /// site if a printable form is needed for logs.
    #[error("non-alphabetic drive letter byte: 0x{0:02x}")]
    NonAlphabeticDriveLetter(u8),

    /// Response status byte was neither 0 ([`Status::Ok`]) nor 1
    /// ([`Status::Error`]).  Indicates a protocol-version skew or a
    /// corrupt pipe; the daemon should treat the broker as misbehaving
    /// and refuse to use its handle.
    #[error("unknown status code: {0}")]
    UnknownStatusCode(u8),
}

/// A request from the daemon to the broker for a volume handle.
///
/// Encodes to a single byte on the wire (the upper-cased drive letter).
///
/// # Construction
///
/// Construct via the struct literal `HandleRequest { drive: 'C' }`.
/// The [`encode`](Self::encode) and round-trip property are valid for
/// any `drive` that satisfies `drive.is_ascii_alphabetic()`; out-of-
/// range values are accepted at construction time but rejected at
/// [`parse`](Self::parse) time.
///
/// # Example
///
/// ```
/// use uffs_broker_protocol::{HandleRequest, REQUEST_WIRE_LEN};
///
/// let req = HandleRequest { drive: 'c' };
/// let bytes = req.encode();
/// assert_eq!(bytes.len(), REQUEST_WIRE_LEN);
/// assert_eq!(bytes[0], b'C', "encode upper-cases the drive letter");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleRequest {
    /// Drive letter (case-insensitive on input, upper-cased on the wire).
    pub drive: char,
}

impl HandleRequest {
    /// Serialize this request to its 1-byte wire representation.
    ///
    /// Upper-cases the drive letter so the broker's
    /// `handle_pipe_request_with_rate_limit` rate-limit map keys
    /// (which were already upper-cased) keep working unchanged.
    ///
    /// Cast `c as u8`: the only callers that should observe a lossy
    /// cast pass non-ASCII chars, which is a caller bug — see
    /// [`HandleRequest::parse`] for the validated round-trip.
    #[must_use]
    pub const fn encode(self) -> [u8; REQUEST_WIRE_LEN] {
        // `char as u8` truncates the Unicode scalar to its low byte.
        // Safe-by-construction here: callers are expected to construct
        // `HandleRequest` from an ASCII-alphabetic char (the only kind
        // `parse` ever yields).  A misbehaving direct constructor that
        // passes a non-ASCII char would produce a byte the round-trip
        // `parse` of which trips `ProtocolError::NonAsciiDriveByte` —
        // the wire stays self-validating.
        let upper = self.drive.to_ascii_uppercase();
        [upper as u8]
    }

    /// Parse a single byte from the wire into a [`HandleRequest`].
    ///
    /// # Errors
    ///
    /// - [`ProtocolError::NonAsciiDriveByte`] if `byte > 0x7F`
    /// - [`ProtocolError::NonAlphabeticDriveLetter`] if the ASCII byte is not
    ///   in `b'A'..=b'Z'` or `b'a'..=b'z'`
    pub const fn parse(byte: u8) -> Result<Self, ProtocolError> {
        if !byte.is_ascii() {
            return Err(ProtocolError::NonAsciiDriveByte(byte));
        }
        if !byte.is_ascii_alphabetic() {
            return Err(ProtocolError::NonAlphabeticDriveLetter(byte));
        }
        // SAFETY of `as char`: `byte` is ASCII-alphabetic, which is a
        // strict subset of the Unicode scalar values that fit in `char`.
        Ok(Self {
            drive: (byte as char).to_ascii_uppercase(),
        })
    }
}

/// Status code in the first byte of a [`HandleResponse`].
///
/// Wire representation is `repr(u8)`; values 0 and 1 are reserved.
/// Any other byte value parses as [`ProtocolError::UnknownStatusCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Status {
    /// Request succeeded; the following 8 bytes contain a valid
    /// duplicated Windows `HANDLE` value (little-endian u64).
    Ok = 0,
    /// Request failed (invalid drive, rate-limited, FFI error, etc.);
    /// the following 8 bytes are unspecified.
    Error = 1,
}

impl Status {
    /// Serialize this status to its 1-byte wire representation.
    #[must_use]
    pub const fn encode(self) -> u8 {
        self as u8
    }

    /// Parse a single byte into a [`Status`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnknownStatusCode`] for any byte other
    /// than 0 or 1.  Forward-compatible extensions to the protocol
    /// (e.g. adding `Status::RateLimited = 2`) MUST bump the protocol
    /// version (currently implicit, future work) before reusing this
    /// space.
    pub const fn parse(byte: u8) -> Result<Self, ProtocolError> {
        match byte {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownStatusCode(other)),
        }
    }
}

/// A response from the broker to the daemon.
///
/// Encodes to 9 bytes on the wire: 1 status byte + 8 little-endian
/// handle bytes.
///
/// # Semantics by status
///
/// - [`Status::Ok`] — `handle` is a valid duplicated Windows `HANDLE` value the
///   daemon may use for read-only MFT access.
/// - [`Status::Error`] — `handle` is unspecified (the [`encode`] / [`parse`]
///   round-trip preserves the value, but callers MUST NOT treat it as a valid
///   handle).
///
/// [`encode`]: Self::encode
/// [`parse`]: Self::parse
///
/// # Example
///
/// ```
/// use uffs_broker_protocol::{HandleResponse, RESPONSE_WIRE_LEN, Status};
///
/// let resp = HandleResponse {
///     status: Status::Ok,
///     handle: 0xDEAD_BEEF,
/// };
/// let bytes = resp.encode();
/// assert_eq!(bytes.len(), RESPONSE_WIRE_LEN);
///
/// let round_trip = HandleResponse::parse(bytes).unwrap();
/// assert_eq!(round_trip, resp);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleResponse {
    /// Whether the request succeeded.
    pub status: Status,
    /// Duplicated Windows `HANDLE` value (only meaningful when
    /// `status == Status::Ok`).
    pub handle: u64,
}

impl HandleResponse {
    /// Convenience builder for a success response.
    #[must_use]
    pub const fn ok(handle: u64) -> Self {
        Self {
            status: Status::Ok,
            handle,
        }
    }

    /// Convenience builder for an error response.  The handle bytes
    /// are zeroed (callers MUST ignore them anyway).
    #[must_use]
    pub const fn error() -> Self {
        Self {
            status: Status::Error,
            handle: 0,
        }
    }

    /// Serialize this response to its 9-byte wire representation.
    #[must_use]
    pub const fn encode(self) -> [u8; RESPONSE_WIRE_LEN] {
        let mut buf = [0_u8; RESPONSE_WIRE_LEN];
        buf[0] = self.status.encode();
        let handle_bytes = self.handle.to_le_bytes();
        // `copy_from_slice` is not `const`-stable yet; unroll the
        // 8-byte copy by index so this function can be `const`.
        buf[1] = handle_bytes[0];
        buf[2] = handle_bytes[1];
        buf[3] = handle_bytes[2];
        buf[4] = handle_bytes[3];
        buf[5] = handle_bytes[4];
        buf[6] = handle_bytes[5];
        buf[7] = handle_bytes[6];
        buf[8] = handle_bytes[7];
        buf
    }

    /// Parse 9 bytes from the wire into a [`HandleResponse`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnknownStatusCode`] if `bytes[0]` is
    /// not a valid [`Status`] discriminant.
    pub fn parse(bytes: [u8; RESPONSE_WIRE_LEN]) -> Result<Self, ProtocolError> {
        let status = Status::parse(bytes[0])?;
        // The 8 handle bytes always parse — they're a plain u64 LE.
        // Whether they're *meaningful* is a function of `status`.
        let handle_bytes: [u8; 8] = [
            bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        ];
        let handle = u64::from_le_bytes(handle_bytes);
        Ok(Self { status, handle })
    }
}

#[cfg(test)]
mod tests {
    use super::{HandleRequest, HandleResponse, PIPE_NAME, ProtocolError, Status};

    // ───── PIPE_NAME regression anchor ──────────────────────────────────

    #[test]
    fn pipe_name_matches_legacy_literal() {
        // Both `uffs-broker/src/broker.rs` and
        // `uffs-daemon/src/broker_client.rs` carried this exact string
        // before F5.  Changing it is a wire-protocol break.
        assert_eq!(PIPE_NAME, r"\\.\pipe\uffs-broker");
    }

    // ───── HandleRequest ────────────────────────────────────────────────

    #[test]
    fn request_encode_upper_cases_lower_input() {
        let req = HandleRequest { drive: 'c' };
        assert_eq!(req.encode(), [b'C']);
    }

    #[test]
    fn request_encode_identity_for_upper_input() {
        let req = HandleRequest { drive: 'C' };
        assert_eq!(req.encode(), [b'C']);
    }

    #[test]
    fn request_round_trip_every_ascii_alphabetic() {
        // Property: for every ASCII alphabetic byte, parse-then-encode
        // must yield the upper-cased input.
        for byte in (b'A'..=b'Z').chain(b'a'..=b'z') {
            let req = HandleRequest::parse(byte).expect("ASCII alphabetic byte parses");
            let encoded = req.encode();
            assert_eq!(
                encoded[0],
                byte.to_ascii_uppercase(),
                "round-trip preserves upper-case identity for 0x{byte:02x}",
            );
        }
    }

    #[test]
    fn request_parse_rejects_digit() {
        let err = HandleRequest::parse(b'1').unwrap_err();
        assert_eq!(err, ProtocolError::NonAlphabeticDriveLetter(b'1'));
    }

    #[test]
    fn request_parse_rejects_space() {
        let err = HandleRequest::parse(b' ').unwrap_err();
        assert_eq!(err, ProtocolError::NonAlphabeticDriveLetter(b' '));
    }

    #[test]
    fn request_parse_rejects_null() {
        let err = HandleRequest::parse(0).unwrap_err();
        assert_eq!(err, ProtocolError::NonAlphabeticDriveLetter(0));
    }

    #[test]
    fn request_parse_rejects_non_ascii() {
        // 0xC4 is the lead byte of UTF-8 "Ä" — not ASCII, must fail
        // with the non-ASCII variant, not the non-alphabetic variant.
        let err = HandleRequest::parse(0xC4).unwrap_err();
        assert_eq!(err, ProtocolError::NonAsciiDriveByte(0xC4));
    }

    // ───── Status ───────────────────────────────────────────────────────

    #[test]
    fn status_encode_is_repr_u8() {
        assert_eq!(Status::Ok.encode(), 0);
        assert_eq!(Status::Error.encode(), 1);
    }

    #[test]
    fn status_parse_rejects_unknown_codes() {
        for byte in 2_u8..=255 {
            let err = Status::parse(byte).unwrap_err();
            assert_eq!(err, ProtocolError::UnknownStatusCode(byte));
        }
    }

    // ───── HandleResponse ───────────────────────────────────────────────

    #[test]
    fn response_round_trip_ok_with_arbitrary_handle() {
        let resp = HandleResponse {
            status: Status::Ok,
            handle: 0xDEAD_BEEF_CAFE_BABE_u64,
        };
        let round_trip = HandleResponse::parse(resp.encode()).unwrap();
        assert_eq!(round_trip, resp);
    }

    #[test]
    fn response_round_trip_error_zero_handle() {
        let resp = HandleResponse::error();
        let bytes = resp.encode();
        assert_eq!(bytes, [1, 0, 0, 0, 0, 0, 0, 0, 0]);
        let round_trip = HandleResponse::parse(bytes).unwrap();
        assert_eq!(round_trip, resp);
    }

    #[test]
    fn response_round_trip_u64_boundary_values() {
        // Property: encode/parse round-trip preserves any u64 value
        // verbatim, regardless of status (the bytes are always shaped
        // identically — semantics are policy at a higher layer).
        for handle in [
            u64::MIN,
            1_u64,
            0xFF_u64,
            0x0100_u64,
            0xFFFF_u64,
            0x0001_0000_u64,
            0x1234_5678_9ABC_DEF0_u64,
            u64::MAX - 1,
            u64::MAX,
        ] {
            for status in [Status::Ok, Status::Error] {
                let resp = HandleResponse { status, handle };
                let round_trip = HandleResponse::parse(resp.encode()).unwrap();
                assert_eq!(
                    round_trip, resp,
                    "round-trip preserves handle 0x{handle:016x} under {status:?}",
                );
            }
        }
    }

    #[test]
    fn response_parse_rejects_unknown_status() {
        // Status byte 2..=255 is reserved for future extensions; today
        // any such byte must fail parsing.
        let bytes = [2_u8, 0, 0, 0, 0, 0, 0, 0, 0];
        let err = HandleResponse::parse(bytes).unwrap_err();
        assert_eq!(err, ProtocolError::UnknownStatusCode(2));
    }

    #[test]
    fn response_encode_is_little_endian() {
        // Anchor the LE byte order explicitly — wire-format regression
        // guard if a future contributor were tempted to use BE.
        let resp = HandleResponse {
            status: Status::Ok,
            handle: 0x0102_0304_0506_0708_u64,
        };
        let bytes = resp.encode();
        assert_eq!(
            bytes,
            [0, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
            "wire layout: status, handle in little-endian",
        );
    }

    #[test]
    fn response_ok_constructor() {
        let resp = HandleResponse::ok(42);
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.handle, 42);
    }
}
