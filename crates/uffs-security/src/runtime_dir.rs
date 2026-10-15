// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Runtime tempfile lifecycle for Phase 2b memory tiering.
//!
//! Daemon-private directory holding **decrypted** column views for the
//! WARM tier.  The contents are reconstructable from the encrypted
//! authoritative cache — losing the runtime dir on a crash is harmless
//! (next promote rebuilds it).  Threat model details live in
//! `docs/dev/architecture/CACHE_SECURITY_ANALYSIS.md`.
//!
//! # On-disk shape
//!
//! ```text
//! <runtime_root>/                    e.g. ~/.local/share/uffs/runtime/
//! └── <pid>/                         daemon's process id (sweep candidate)
//!     └── <volume-guid>.live         per-shard ephemeral tempfile
//! ```
//!
//! The daemon owns one `<pid>/` subdir; on every startup it sweeps
//! sibling `<pid>/` subdirs whose pid is no longer alive (orphan
//! cleanup after `kill -9`, BSOD, or unclean shutdown).
//!
//! # Cross-platform contract
//!
//! | Concern         | Unix (Mac + Linux)              | Windows                                |
//! |-----------------|---------------------------------|----------------------------------------|
//! | Owner-only file | `O_CREAT \| 0o600`              | inherits parent DACL + `FILE_SHARE_NONE` |
//! | Auto-unlink     | manual / orphan sweep           | `FILE_FLAG_DELETE_ON_CLOSE`            |
//! | Liveness probe  | `kill(pid, 0)`                  | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` |
//!
//! The unsafe `memmap2::Mmap::map(&File)` call is the *only* way to map
//! a file-backed read-only view.  It lives here behind
//! [`mmap_read_only`], gated by the [`RuntimeFile`] newtype so the
//! caller proves they obtained the file from
//! [`RuntimeDir::create_owner_only`] — that's where the soundness
//! preconditions are established.  Downstream crates
//! (`uffs-core::compact_mmap`) stay `#![forbid(unsafe_code)]`.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

// ─────────────────────────────────────────────────────────────────────
// Trait + supporting types.
// ─────────────────────────────────────────────────────────────────────

/// Newtype wrapper around an open [`File`] handle that the
/// [`RuntimeDir`] implementation produced.
///
/// The newtype is the soundness anchor for [`mmap_read_only`]: only
/// files routed through [`RuntimeDir::create_owner_only`] are
/// guaranteed to satisfy the precondition that no other process can
/// truncate the file under us (Unix: `0o600` perms in a `0o700`
/// parent dir; Windows: `FILE_SHARE_NONE` + `FILE_FLAG_DELETE_ON_CLOSE`).
///
/// The underlying file is kept open for read + write so the caller
/// can populate it before mmapping (the typical Phase 2b flow:
/// `decrypt → write → mmap`).  Mapping the same file `&File` after a
/// flushed write is sound because `Mmap::map` opens its own
/// kernel-level mapping — the writer handle does not invalidate it.
#[derive(Debug)]
pub struct RuntimeFile {
    /// The open file handle.  Read + write on Unix; read + write +
    /// delete on Windows (the `DELETE` access right is required for
    /// `FILE_FLAG_DELETE_ON_CLOSE`).
    file: File,
    /// The file's path.  Useful for logging and for callers that need
    /// to reopen / seek by path; never used for re-`open()`-ing
    /// because that would defeat the share-mode protection.
    path: PathBuf,
}

impl RuntimeFile {
    /// Borrow the underlying file for reading or `seek`-only access.
    #[must_use]
    pub const fn as_file(&self) -> &File {
        &self.file
    }

    /// Borrow the underlying file mutably for writing.
    pub const fn as_file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// The full path of the runtime tempfile.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consume the wrapper and return the inner [`File`].
    ///
    /// Use sparingly: callers that hold a bare [`File`] cannot
    /// pass it to [`mmap_read_only`] (which requires a
    /// [`RuntimeFile`]).  Provided for compatibility with APIs that
    /// take ownership of a `File` (e.g. `tokio::fs::File::from_std`).
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }
}

/// Platform-abstracted lifecycle for the daemon's runtime tempfiles.
///
/// Implementors:
///
/// * `UnixRuntimeDir` (default on Mac + Linux): `OpenOptions` + `0o600` mode +
///   parent dir at `0o700`.  Orphan sweep via `kill(pid, 0)`.
/// * `WindowsRuntimeDir` (default on Windows): `CreateFileW` with
///   `FILE_FLAG_DELETE_ON_CLOSE` + `FILE_SHARE_NONE` + DACL inherited from a
///   `create_secure_dir`-created parent.  Orphan sweep via
///   `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`.
/// * `TestRuntimeDir`: deterministic in-process fake; explicit `mark_alive` /
///   `mark_dead` for orphan-sweep tests.
///
/// The trait is `Send + Sync` so the daemon can hold a single
/// `Arc<dyn RuntimeDir>` across all loaded shards.
pub trait RuntimeDir: Send + Sync {
    /// Create a new runtime tempfile at `path`.
    ///
    /// The parent directory **must already exist** with owner-only
    /// permissions — call [`crate::fs::create_secure_dir`] on the
    /// parent first.  This method only handles the per-file step.
    ///
    /// # Errors
    ///
    /// * `AlreadyExists` — `path` already exists (callers should pick a unique
    ///   name, e.g. via volume GUID).
    /// * `PermissionDenied` — parent dir is unwriteable, or the process lacks
    ///   permission to set the requested mode/DACL.
    /// * Any other [`std::io::Error`] forwarded from the underlying filesystem
    ///   call.
    fn create_owner_only(&self, path: &Path) -> io::Result<RuntimeFile>;

    /// Sweep `<parent_dir>/<pid>/` subdirectories whose pid is no
    /// longer alive.  Returns the number of subdirectories removed.
    ///
    /// Only immediate children of `parent_dir` named with a base-10
    /// `u32` are considered.  Anything else (regular files, dirs
    /// with non-numeric names) is ignored.
    ///
    /// Conservative on `EPERM` / `ERROR_ACCESS_DENIED`: if we can't
    /// determine whether a pid is alive, the directory is left
    /// alone.  Better to leak a tempfile than to delete one belonging
    /// to a different user's daemon.
    ///
    /// # Errors
    ///
    /// Returns an error only if `parent_dir` is unreadable.  Per-pid
    /// failures (broken symlink, permission error on remove) are
    /// logged at `warn!` and counted as "not removed"; the sweep
    /// continues.
    fn cleanup_orphans(&self, parent_dir: &Path) -> io::Result<usize>;
}

/// Memory-map a [`RuntimeFile`] read-only.
///
/// # Safety guarantees
///
/// `Mmap::map` is `unsafe` because the kernel may deliver `SIGBUS` if
/// another process truncates the underlying file while the mapping is
/// alive.  Both [`RuntimeDir`] implementations rule this out:
///
/// * **Unix:** the runtime file lives in a daemon-private directory created by
///   [`crate::fs::create_secure_dir`] (`0o700`), and the file itself is
///   `0o600`.  No other user can open it.  The daemon itself never truncates
///   the file after the initial write — the workflow is `create → write → mmap
///   → drop`.
/// * **Windows:** `FILE_SHARE_NONE` causes the kernel to reject every
///   subsequent `CreateFileW` for that path (including by the same process from
///   a different code path).  Truncation is impossible while our handle is
///   open, and `FILE_FLAG_DELETE_ON_CLOSE` guarantees the file is unlinked when
///   the handle drops.
///
/// The [`RuntimeFile`] newtype is the type-system encoding of these
/// preconditions: the only way to obtain one is via
/// [`RuntimeDir::create_owner_only`].
///
/// # Errors
///
/// Forwards any [`std::io::Error`] from the underlying mmap syscall
/// (`mmap` on Unix, `MapViewOfFile` on Windows).
pub fn mmap_read_only(file: &RuntimeFile) -> io::Result<Mmap> {
    #[expect(
        unsafe_code,
        reason = "Mmap::map preconditions verified by RuntimeFile newtype + RuntimeDir DACL/share-mode contract"
    )]
    // SAFETY: `RuntimeFile` is constructible only via
    // `RuntimeDir::create_owner_only`, which establishes the
    // truncation-immunity invariant documented above.  The mapping
    // is read-only; the caller may write to `file` *before* calling
    // this function but doing so afterwards has no effect on the
    // returned `Mmap` (it's a snapshot of the bytes at map time).
    unsafe {
        Mmap::map(&file.file)
    }
}

// ─────────────────────────────────────────────────────────────────────
// Shared orphan-sweep helper.
// ─────────────────────────────────────────────────────────────────────

/// Walk `<parent_dir>/<pid>/` subdirs, calling `is_dead(pid)` for
/// each.  Recursively removes subdirs for which `is_dead` returns
/// `true`; logs and skips on per-entry errors.  Returns the count of
/// successfully removed subdirs.
///
/// Shared between every [`RuntimeDir`] impl — the only platform
/// difference is the liveness predicate.
fn sweep_pid_directories<F>(parent_dir: &Path, is_dead: F) -> io::Result<usize>
where
    F: Fn(u32) -> bool,
{
    if !parent_dir.exists() {
        return Ok(0);
    }
    let entries = std::fs::read_dir(parent_dir)?;
    let mut removed: usize = 0;
    for entry_result in entries {
        let Some((pid_path, pid)) = parse_pid_dir_entry(entry_result) else {
            continue;
        };
        if !is_dead(pid) {
            continue;
        }
        match std::fs::remove_dir_all(&pid_path) {
            Ok(()) => {
                tracing::info!(pid, path = %pid_path.display(), "orphan runtime dir swept");
                removed = removed.saturating_add(1);
            }
            Err(err) => {
                tracing::warn!(
                    pid,
                    path = %pid_path.display(),
                    error = %err,
                    "orphan sweep: remove failed"
                );
            }
        }
    }
    Ok(removed)
}

/// Decide whether a `read_dir` entry looks like a `<pid>/` runtime
/// subdirectory we own.  Returns `Some((path, pid))` if the entry
/// is a directory whose filename parses as a `u32`; `None` for
/// regular files, non-numeric names, transient `read_dir` errors,
/// or `file_type()` failures (e.g. broken symlink).
///
/// Extracted from [`sweep_pid_directories`] to keep that function's
/// cognitive complexity under the workspace threshold.
fn parse_pid_dir_entry(entry_result: io::Result<std::fs::DirEntry>) -> Option<(PathBuf, u32)> {
    let entry = entry_result.ok()?;
    let file_type = entry.file_type().ok()?;
    if !file_type.is_dir() {
        return None;
    }
    let name = entry.file_name();
    let name_str = name.to_str()?;
    let pid = name_str.parse::<u32>().ok()?;
    Some((entry.path(), pid))
}

// ─────────────────────────────────────────────────────────────────────
// Unix implementation (Mac + Linux).
// ─────────────────────────────────────────────────────────────────────

/// Unix [`RuntimeDir`] backend.
///
/// Hidden inner module so the trait impl + private liveness probe
/// share state without leaking implementation details onto the
/// crate's public API.  Re-exported as [`UnixRuntimeDir`] below.
#[cfg(unix)]
mod unix_impl {
    use std::fs::OpenOptions;
    use std::io;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::Path;

    use super::{RuntimeDir, RuntimeFile, sweep_pid_directories};

    /// Default Unix [`RuntimeDir`] implementation.
    ///
    /// `create_owner_only` opens the file with `O_CREAT | O_EXCL` and
    /// `0o600` mode bits.  The parent directory is the caller's
    /// responsibility (use [`crate::fs::create_secure_dir`] which
    /// applies `0o700`).
    ///
    /// `cleanup_orphans` probes `kill(pid, 0)` for each `<pid>/`
    /// subdir.  Treats `EPERM` (process exists but is owned by
    /// another user) as alive — the conservative choice that avoids
    /// stomping on someone else's daemon.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct UnixRuntimeDir;

    impl RuntimeDir for UnixRuntimeDir {
        fn create_owner_only(&self, path: &Path) -> io::Result<RuntimeFile> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            Ok(RuntimeFile {
                file,
                path: path.to_path_buf(),
            })
        }

        fn cleanup_orphans(&self, parent_dir: &Path) -> io::Result<usize> {
            sweep_pid_directories(parent_dir, |pid| !is_pid_alive(pid))
        }
    }

    /// `kill(pid, 0)` liveness probe.
    ///
    /// Returns `true` for alive (kill succeeded, or `EPERM` —
    /// "exists but we lack permission to signal").  Returns `false`
    /// for dead (`ESRCH` — "no such process") or invalid pid.
    ///
    /// The `EPERM` arm is the difference between this probe and the
    /// daemon's `lifecycle::is_process_alive`: that helper treats
    /// `EPERM` as dead because the daemon is checking whether *its
    /// previous incarnation* is still running (same uid).  Here we
    /// might be looking at another user's daemon, and falsely
    /// declaring it dead would lead to deleting their tempfiles.
    fn is_pid_alive(pid: u32) -> bool {
        let Ok(target_pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: `kill(pid, 0)` is a standard POSIX liveness probe.
        // It sends no signal — only the kernel's permission /
        // existence check runs.
        #[expect(unsafe_code, reason = "libc::kill FFI for liveness probe")]
        let ret = unsafe { libc::kill(target_pid, 0_i32) };
        if ret == 0_i32 {
            return true;
        }
        let last_err = io::Error::last_os_error();
        // EPERM = process exists but is owned by another uid.  Treat
        // as alive (don't sweep someone else's runtime tempfiles).
        last_err.raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(unix)]
pub use unix_impl::UnixRuntimeDir;

// ─────────────────────────────────────────────────────────────────────
// Windows implementation.
// ─────────────────────────────────────────────────────────────────────

/// Windows [`RuntimeDir`] backend.
///
/// Hidden inner module so the trait impl + private helpers (wide
/// path encoding, liveness probe) share state without leaking
/// implementation details onto the crate's public API.  Re-exported
/// as [`WindowsRuntimeDir`] below.
#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use std::path::Path;

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_DELETE_ON_CLOSE,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_MODE,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::core::PCWSTR;

    use super::{RuntimeDir, RuntimeFile, sweep_pid_directories};

    /// Default Windows [`RuntimeDir`] implementation.
    ///
    /// `create_owner_only` opens the file via `CreateFileW` with:
    ///
    /// * Access mask: `FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE`
    ///   (`DELETE` is required for `FILE_FLAG_DELETE_ON_CLOSE`).
    /// * Share mode: `0` (no sharing — kernel rejects all other `CreateFileW`
    ///   calls for this path).
    /// * Security attributes: `NULL` — file inherits the parent directory's
    ///   owner-only DACL set by [`crate::fs::create_secure_dir`].
    /// * Disposition: `CREATE_NEW` (atomic create-or-fail).
    /// * Flags: `FILE_FLAG_DELETE_ON_CLOSE` — the kernel unlinks the file when
    ///   the last handle drops, even on `kill -9` / blue-screen / power loss.
    ///
    /// `cleanup_orphans` probes
    /// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` for each `<pid>/`
    /// subdir.  `ERROR_ACCESS_DENIED` is treated as alive (process exists,
    /// ACL prohibits us).
    ///
    /// Belt-and-suspenders: even if the daemon crashes before
    /// `cleanup_orphans` runs, `FILE_FLAG_DELETE_ON_CLOSE` already
    /// vaporized the per-shard tempfiles — only the empty
    /// `<pid>/` directory remains, which the sweep removes.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct WindowsRuntimeDir;

    impl RuntimeDir for WindowsRuntimeDir {
        fn create_owner_only(&self, path: &Path) -> io::Result<RuntimeFile> {
            let wide = path_to_wide(path.as_os_str());
            let access = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | DELETE.0;
            // SAFETY: `wide` is a valid null-terminated UTF-16 buffer
            // owned for the duration of this call; `CreateFileW`
            // returns `Err` (not an invalid handle) on failure;
            // `path` is the caller's responsibility to validate.
            #[expect(unsafe_code, reason = "Win32 CreateFileW FFI")]
            let handle_result = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    access,
                    FILE_SHARE_MODE(0),
                    None,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_DELETE_ON_CLOSE,
                    None,
                )
            };
            // Unwrap the FACILITY_WIN32 envelope so std can map the
            // raw Win32 code to the documented `ErrorKind`
            // (`AlreadyExists` / `PermissionDenied` per the trait
            // contract at `RuntimeDir::create_owner_only`).
            //
            // `windows::core::Error::code()` returns an HRESULT.  Win32
            // failures arrive wrapped via `HRESULT_FROM_WIN32`, which
            // sets facility = 7 (`FACILITY_WIN32`, severity = 1) and
            // stuffs `GetLastError()` into the low 16 bits:
            //
            //     0x8007_XXXX  ← XXXX = GetLastError()
            //
            // `io::Error::from_raw_os_error` expects the bare Win32
            // code (not the wrapped HRESULT) — its internal
            // `decode_error_kind` table is keyed on `ERROR_FILE_EXISTS`
            // (80), `ERROR_ACCESS_DENIED` (5), etc., NOT on their
            // HRESULT envelopes.  Passing the raw HRESULT instead
            // would degrade every error to `ErrorKind::Other`, which
            // is exactly what produced the nightly cargo-mutants
            // baseline failure on Windows runners (`gh run 26037964594`,
            // `runtime_dir::tests::create_owner_only_rejects_existing_file`).
            //
            // Non-WIN32-facility HRESULTs fall through unchanged —
            // std has no portable kind mapping for them anyway, and
            // `from_raw_os_error` will just preserve them verbatim.
            let handle = handle_result.map_err(|err| {
                let hresult = err.code().0;
                let win32_code = if (hresult.cast_unsigned() & 0xFFFF_0000_u32) == 0x8007_0000_u32 {
                    hresult & 0xFFFF_i32
                } else {
                    hresult
                };
                io::Error::from_raw_os_error(win32_code)
            })?;
            #[expect(
                unsafe_code,
                reason = "transfer ownership Win32 HANDLE → std::fs::File"
            )]
            // SAFETY: `handle` is a valid kernel handle returned by
            // `CreateFileW`; we transfer ownership into the `File`
            // wrapper, which closes it on drop.  No other code path
            // observes the raw handle.
            let file = unsafe { File::from_raw_handle(handle.0) };
            Ok(RuntimeFile {
                file,
                path: path.to_path_buf(),
            })
        }

        fn cleanup_orphans(&self, parent_dir: &Path) -> io::Result<usize> {
            sweep_pid_directories(parent_dir, |pid| !is_pid_alive(pid))
        }
    }

    /// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` liveness probe.
    ///
    /// `OpenProcess` requires the most minimal access mask
    /// (`PROCESS_QUERY_LIMITED_INFORMATION`) which most processes
    /// grant by default — protected processes (Windows kernel,
    /// security software) deny it but those PIDs aren't ours
    /// anyway.  Closing the handle immediately is fine — we only
    /// needed to know whether the open succeeded.
    fn is_pid_alive(pid: u32) -> bool {
        // SAFETY: `OpenProcess` is a well-defined Win32 API.  `pid`
        // is an integer; the kernel validates it.  On failure the
        // function returns `Err`, never a dangling handle.
        #[expect(unsafe_code, reason = "Win32 OpenProcess FFI for liveness probe")]
        let result = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
        match result {
            Ok(handle) => {
                // SAFETY: `handle` was just returned by `OpenProcess`
                // and is not aliased.  Closing balances the open.
                #[expect(unsafe_code, reason = "CloseHandle balances OpenProcess above")]
                let close_result = unsafe { CloseHandle(handle) };
                if let Err(err) = close_result {
                    tracing::debug!(pid, err = ?err, "is_pid_alive: CloseHandle failed");
                }
                true
            }
            Err(err) => {
                // ERROR_ACCESS_DENIED = pid exists but ACL forbids us.
                // Treat as alive (don't stomp on a protected or
                // other-user process).  `code()` is an HRESULT
                // (`0x80070005`), so compare against the HRESULT form:
                // the bare Win32 value 5 never matched, and a live
                // process we may not open read as dead, its runtime
                // directory swept as an orphan.
                err.code() == windows::Win32::Foundation::ERROR_ACCESS_DENIED.to_hresult()
            }
        }
    }

    /// Encode an `OsStr` as a null-terminated UTF-16 buffer.
    fn path_to_wide(path: &OsStr) -> Vec<u16> {
        path.encode_wide().chain(Some(0_u16)).collect()
    }
}

#[cfg(windows)]
pub use windows_impl::WindowsRuntimeDir;

// ─────────────────────────────────────────────────────────────────────
// Default alias (cfg-gated).
// ─────────────────────────────────────────────────────────────────────

/// Platform-default [`RuntimeDir`] type alias.
///
/// Resolves to `UnixRuntimeDir` on Mac/Linux, `WindowsRuntimeDir`
/// on Windows.  Use this in production code so the platform split is
/// invisible at the call site.
#[cfg(unix)]
pub type DefaultRuntimeDir = UnixRuntimeDir;

/// Platform-default [`RuntimeDir`] type alias.
///
/// Resolves to `UnixRuntimeDir` on Mac/Linux, `WindowsRuntimeDir`
/// on Windows.  Use this in production code so the platform split is
/// invisible at the call site.
#[cfg(windows)]
pub type DefaultRuntimeDir = WindowsRuntimeDir;

// ─────────────────────────────────────────────────────────────────────
// Test fake.
// ─────────────────────────────────────────────────────────────────────

/// Test-only `RuntimeDir` impl with a deterministic, in-process
/// "is pid alive?" oracle.
///
/// Use [`TestRuntimeDir::new`] to create one, then call
/// [`mark_alive`](Self::mark_alive) / [`mark_dead`](Self::mark_dead)
/// to drive the orphan-sweep predicate from your test.  Any pid not
/// explicitly marked is considered **dead** by default — the test
/// must opt every "alive" pid in.  This keeps tests honest: a test
/// fixture that forgets to mark its own daemon pid alive will see it
/// swept, surfacing the bug immediately.
///
/// `create_owner_only` opens a regular file (no DACL / `0o600`
/// shenanigans) — tests don't need real share-mode protection
/// because nothing else is competing for the file inside a unit
/// test's `tempfile::TempDir`.
#[cfg(test)]
pub(crate) mod test_fake {
    use std::collections::HashSet;
    use std::fs::OpenOptions;
    use std::io;
    use std::path::Path;
    use std::sync::Mutex;

    use super::{RuntimeDir, RuntimeFile, sweep_pid_directories};

    /// In-process test fake for [`RuntimeDir`].  See module docs.
    #[derive(Debug, Default)]
    pub(crate) struct TestRuntimeDir {
        /// Set of pids that the test has declared "alive".  Any pid
        /// not in this set is treated as dead by `cleanup_orphans`.
        live_pids: Mutex<HashSet<u32>>,
    }

    impl TestRuntimeDir {
        /// Create a new fake with no live pids registered.
        #[must_use]
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// Declare `pid` to be alive (orphan sweep skips its dir).
        ///
        /// # Panics
        ///
        /// Panics if the internal mutex is poisoned, which only
        /// happens if a previous test panicked while holding the
        /// lock — surfacing test bugs is the point.
        pub(crate) fn mark_alive(&self, pid: u32) {
            self.live_pids
                .lock()
                .expect("TestRuntimeDir mutex poisoned")
                .insert(pid);
        }

        /// Declare `pid` to be dead (orphan sweep removes its dir).
        ///
        /// # Panics
        ///
        /// Panics if the internal mutex is poisoned.
        pub(crate) fn mark_dead(&self, pid: u32) {
            self.live_pids
                .lock()
                .expect("TestRuntimeDir mutex poisoned")
                .remove(&pid);
        }
    }

    impl RuntimeDir for TestRuntimeDir {
        fn create_owner_only(&self, path: &Path) -> io::Result<RuntimeFile> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)?;
            Ok(RuntimeFile {
                file,
                path: path.to_path_buf(),
            })
        }

        fn cleanup_orphans(&self, parent_dir: &Path) -> io::Result<usize> {
            let snapshot = self
                .live_pids
                .lock()
                .map_err(|err| io::Error::other(format!("TestRuntimeDir mutex poisoned: {err}")))?
                .clone();
            sweep_pid_directories(parent_dir, |pid| !snapshot.contains(&pid))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tests.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
