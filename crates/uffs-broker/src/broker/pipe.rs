// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Broker named-pipe creation with its SDDL security descriptor, and the
//! accept wait every Broker pipe shares.
//!
//! Split out of `broker.rs` to keep that file under the 800-LOC ceiling
//! (alongside `service.rs`, `process_handle.rs`, `authenticode.rs`,
//! `owned_handle.rs`).  The accept loop, connection handling, and pipe I/O
//! stay in `broker.rs`; this module owns *creating* a listening instance
//! with the right ACL + integrity label and *waiting* for its client.

#[cfg(windows)]
use uffs_broker_protocol::PIPE_NAME;

/// Create a broker named-pipe instance reachable by the non-elevated daemon.
///
/// `first_instance` sets `FILE_FLAG_FIRST_PIPE_INSTANCE`, which fails the call
/// (`ERROR_ACCESS_DENIED`) if an instance of the name already exists.  The
/// **first** instance the accept loop creates passes `true` (anti-squatting:
/// fail loudly if another process already owns `\\.\pipe\uffs-broker`); every
/// subsequent instance passes `false`, or it would fail against the instance we
/// just made.
#[cfg(windows)]
#[expect(unsafe_code, reason = "CreateNamedPipeW is an FFI call")]
pub(super) fn create_broker_pipe(
    first_instance: bool,
) -> anyhow::Result<windows::Win32::Foundation::HANDLE> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::core::PCWSTR;

    let pipe_name: Vec<u16> = std::ffi::OsStr::new(PIPE_NAME)
        .encode_wide()
        .chain(Some(0))
        .collect();

    // The whole point of the broker is to serve a NON-elevated daemon.  With
    // `None` security, an elevated/SYSTEM creator gets an owner-only DACL AND
    // a high mandatory-integrity label, so the medium-integrity daemon can't
    // even open the pipe.  Build an explicit descriptor that lets the daemon
    // connect; the broker still verifies the client is uffsd + Authenticode
    // before granting any handle (`check_client_identity`), so a permissive
    // *connect* ACL is not a privilege leak.
    let security = PipeSecurity::build()?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: security.descriptor_ptr(),
        bInheritHandle: false.into(),
    };

    // FIRST_PIPE_INSTANCE only on the first instance — see the fn doc.
    let open_mode = if first_instance {
        PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        PIPE_ACCESS_DUPLEX
    };

    // SAFETY: `pipe_name` is a NUL-terminated UTF-16 buffer and `sa` (with its
    // security descriptor) both live until after this call returns; the pipe
    // copies the descriptor, so `security` may be dropped afterwards.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(pipe_name.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            super::MAX_PIPE_INSTANCES, // max instances (FU-5)
            1024,                      // out buffer
            1024,                      // in buffer
            0,                         // default timeout
            Some(&raw const sa),
        )
    };
    drop(security);

    if handle.is_invalid() {
        anyhow::bail!(
            "CreateNamedPipeW failed: {}",
            std::io::Error::last_os_error()
        );
    }

    Ok(handle)
}

/// Owns a self-relative security descriptor built from SDDL, for the broker
/// pipe.  Frees the descriptor (`LocalFree`) on drop.
///
/// SDDL: `D:(A;;GRGW;;;AU)S:(ML;;NW;;;LW)`
/// - DACL grants Authenticated Users generic read+write (enough to open and
///   talk to the pipe — identity is checked at the app layer per request).
/// - SACL sets a **low** mandatory-integrity label so the medium-integrity
///   (non-elevated) daemon is not blocked by the high label an elevated/SYSTEM
///   creator would otherwise stamp on the object.
#[cfg(windows)]
struct PipeSecurity {
    /// `LocalAlloc`-ed self-relative security descriptor; freed on drop.
    descriptor: windows::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl PipeSecurity {
    /// Build the broker-pipe security descriptor from SDDL (see the type docs).
    #[expect(
        unsafe_code,
        reason = "FFI: ConvertStringSecurityDescriptorToSecurityDescriptorW"
    )]
    fn build() -> anyhow::Result<Self> {
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows::Win32::Security::PSECURITY_DESCRIPTOR;
        use windows::core::PCWSTR;

        let sddl: Vec<u16> = "D:(A;;GRGW;;;AU)S:(ML;;NW;;;LW)"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a NUL-terminated UTF-16 string valid for the call;
        // `descriptor` is a valid out-pointer that receives a `LocalAlloc`-ed
        // descriptor we free in `Drop`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(|err| anyhow::anyhow!("failed to build pipe security descriptor: {err}"))?;
        Ok(Self { descriptor })
    }

    /// Raw pointer to the descriptor, for `SECURITY_ATTRIBUTES`.
    const fn descriptor_ptr(&self) -> *mut core::ffi::c_void {
        self.descriptor.0
    }
}

#[cfg(windows)]
impl Drop for PipeSecurity {
    #[expect(
        unsafe_code,
        reason = "FFI: LocalFree of the SDDL-allocated descriptor"
    )]
    fn drop(&mut self) {
        if !self.descriptor.0.is_null() {
            // SAFETY: `descriptor.0` was allocated by
            // `ConvertStringSecurityDescriptorToSecurityDescriptorW` via
            // `LocalAlloc`; freeing it once on drop is the documented contract.
            _ = unsafe {
                windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
                    self.descriptor.0,
                )))
            };
        }
    }
}

/// Wait for a client to connect to the pipe.
///
/// Shared by every pipe the Broker serves: the daemon-facing handle pipe,
/// the Snapshot Manager pipe and the VSS helper's control pipe.
#[cfg(windows)]
#[expect(unsafe_code, reason = "ConnectNamedPipe is an FFI call")]
pub(super) fn wait_for_client(pipe: windows::Win32::Foundation::HANDLE) -> anyhow::Result<()> {
    use windows::Win32::System::Pipes::ConnectNamedPipe;

    // SAFETY: `pipe` is an owned valid pipe HANDLE (HANDLE is Copy); the
    // second argument is None (synchronous wait).
    let result = unsafe { ConnectNamedPipe(pipe, None) };

    match result {
        Ok(()) => Ok(()),
        Err(win_err) if client_connected_first(&win_err) => Ok(()),
        Err(win_err) => anyhow::bail!("ConnectNamedPipe failed: {win_err}"),
    }
}

/// Whether a `ConnectNamedPipe` error is `ERROR_PIPE_CONNECTED`: the client
/// opened the instance before the call ran, so the pipe IS connected.
///
/// `windows::core::Error::code()` is an HRESULT (`0x80070217`), never the
/// bare Win32 code 535.  Comparing against `535` never matched, so this
/// success was treated as a failure and the instance was disconnected under
/// a client that had connected fine.  It happens whenever a client arrives in
/// the gap between `CreateNamedPipeW` and `ConnectNamedPipe` — two
/// Coordinators asking for a lease in the same instant, or a VSS helper that
/// connects before the Broker starts waiting for it — and the dropped client
/// read `ERROR_PIPE_NOT_CONNECTED` (233).  Field capture 2026-10-15
/// 03:43:42Z, Broker log: `ConnectNamedPipe failed: There is a process on
/// other end of the pipe. (0x80070217)`.
#[cfg(windows)]
fn client_connected_first(err: &windows::core::Error) -> bool {
    err.code() == windows::Win32::Foundation::ERROR_PIPE_CONNECTED.to_hresult()
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use windows::Win32::Foundation::{ERROR_NO_DATA, ERROR_PIPE_CONNECTED};

    #[cfg(windows)]
    use super::client_connected_first;

    #[cfg(windows)]
    #[test]
    fn pipe_connected_error_counts_as_a_connected_client() {
        let err = windows::core::Error::from(ERROR_PIPE_CONNECTED.to_hresult());
        assert!(client_connected_first(&err), "{err}");
    }

    #[cfg(windows)]
    #[test]
    fn other_connect_errors_are_failures() {
        let err = windows::core::Error::from(ERROR_NO_DATA.to_hresult());
        assert!(!client_connected_first(&err), "{err}");
    }
}
