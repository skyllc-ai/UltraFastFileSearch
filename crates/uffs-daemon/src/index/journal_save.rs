// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Background persistence for the journal loop's save tick.
//!
//! A save writes a drive's whole compact cache: fold the delta overlay
//! (rebuild the trigram, extension and children indexes), serialise,
//! compress, encrypt, write.  Measured on the winbox 2026-10-28 (issue #639)
//! for a 4 M-record C:, one save cost 15-35 CPU-seconds on 4-9 cores and
//! 1-5 GB of transient memory, and under heavy churn saves were started
//! faster than they finished, each holding its own copy of the index.  Three
//! rules keep that in check:
//!
//! * **One save at a time, across all drives** ([`SaveSlot`]).  A save tick
//!   that finds the slot taken applies its batch in memory and leaves the write
//!   to a later tick.
//! * **Small and background.**  The fold (the serializer rebuilding the search
//!   sections from the live records, without copying them) runs on a two-thread
//!   pool whose threads are in background-I/O mode, and zstd gets
//!   [`SAVE_ZSTD_WORKERS`] workers, so a save stays near two cores instead of
//!   taking the machine away from the build that caused the churn.
//! * **Cursor strictly after the file.**  The on-disk USN cursor advances only
//!   once the atomic write has succeeded.  A cursor ahead of the file would
//!   make the next cold start skip the changes in between for good.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use uffs_core::compact::DriveCompactIndex;

use super::IndexManager;
use crate::cache::background_io::{
    BackgroundIoPriority as _, BackgroundIoScope, PlatformBackgroundIoPriority,
};
use crate::cache::journal_loop::CursorStore;

/// zstd workers a background save may use.
pub(crate) const SAVE_ZSTD_WORKERS: u32 = 2;

/// Threads that fold a save's delta overlay.
const SAVE_FOLD_THREADS: usize = 2;

/// The single background-save slot shared by every drive's save tick.
#[derive(Debug, Default)]
pub(crate) struct SaveSlot {
    /// `true` while a save holds the slot.
    busy: AtomicBool,
}

impl SaveSlot {
    /// Take the slot, or `None` when another save still holds it.
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<SavePermit> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_was_free| SavePermit {
                slot: Arc::clone(self),
            })
    }
}

/// Holds the [`SaveSlot`] until dropped — including on a panic inside the
/// save, so a failed save can never wedge every later one.
#[derive(Debug)]
pub(crate) struct SavePermit {
    /// The slot released on drop.
    slot: Arc<SaveSlot>,
}

impl Drop for SavePermit {
    fn drop(&mut self) {
        self.slot.busy.store(false, Ordering::Release);
    }
}

impl IndexManager {
    /// Persist `body` for `letter` on the blocking pool, then store `cursor`.
    ///
    /// Fire-and-forget: the body is already serving queries, so a failed
    /// write only means the next save tick tries again from the newer body;
    /// the cursor stays at the last body that did reach disk.  `permit` is
    /// held for the whole write.
    pub(crate) fn spawn_journal_persist(
        &self,
        letter: uffs_mft::platform::DriveLetter,
        body: Arc<DriveCompactIndex>,
        cursor: u64,
        cursor_store: Arc<dyn CursorStore>,
        permit: SavePermit,
    ) {
        let background_io = Arc::clone(&self.background_io);
        let _persist = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _bg_scope = BackgroundIoScope::enter(background_io);
            match persist_body(&body) {
                Ok(()) => cursor_store.store(letter, cursor),
                Err(err) => tracing::warn!(
                    target: "shard.journal",
                    drive = %letter,
                    error = %err,
                    "Background compact-cache save failed (in-memory body still serving fresh data; \
                     cursor left at the last saved body)",
                ),
            }
        });
    }
}

/// Write `body` as it is.  A delta-carrying body is written folded by the
/// serializer, which rebuilds only the children and trigram sections from the
/// live records (on the save pool) — no copy of the records and names, which
/// for a 30 M-record drive is over 3 GB (winbox run 2, 2026-10-29: folding a
/// private copy pushed D:'s age save to a 17.6 GB peak).
fn persist_body(body: &Arc<DriveCompactIndex>) -> std::io::Result<()> {
    in_save_pool(|| {
        uffs_core::compact_cache::save_compact_cache_with_workers(body, SAVE_ZSTD_WORKERS)
    })
}

/// Run `work` on the save pool, or inline when the pool could not be built.
fn in_save_pool<R: Send>(work: impl FnOnce() -> R + Send) -> R {
    match save_pool() {
        Some(pool) => pool.install(work),
        None => work(),
    }
}

/// The two-thread rayon pool background saves fold on.  Its threads enter
/// background-I/O mode once, at start, and stay there: they do nothing else.
fn save_pool() -> Option<&'static rayon::ThreadPool> {
    /// Built on first use; `None` if the OS refused the threads.
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(SAVE_FOLD_THREADS)
            .thread_name(|index| format!("uffs-save-{index}"))
            .start_handler(|_index| {
                if let Err(err) = PlatformBackgroundIoPriority.begin() {
                    tracing::debug!(error = %err, "save pool thread stays at normal priority");
                }
            })
            .build()
            .map_err(|err| {
                tracing::warn!(error = %err, "save pool unavailable; folding on the calling thread");
            })
            .ok()
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::SaveSlot;

    #[test]
    fn slot_admits_one_save_until_the_permit_drops() {
        let slot = Arc::new(SaveSlot::default());
        let first = slot.try_acquire();
        assert!(first.is_some(), "a free slot must be taken");
        assert!(
            slot.try_acquire().is_none(),
            "a held slot must refuse a second save"
        );
        drop(first);
        assert!(
            slot.try_acquire().is_some(),
            "dropping the permit frees the slot"
        );
    }
}
