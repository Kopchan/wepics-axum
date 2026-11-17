use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

/// In-memory set of albums with an indexing task in flight.
///
/// Uniqueness is per process: one task per album, and the entry is removed when
/// the task finishes or unwinds.
#[derive(Clone, Default)]
pub(crate) struct SyncTracker {
    running: Arc<Mutex<HashSet<i64>>>,
}

impl SyncTracker {
    pub(crate) fn try_claim(&self, album_id: i64) -> Option<SyncGuard> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if !running.insert(album_id) {
            return None;
        }

        Some(SyncGuard {
            album_id,
            running: Arc::clone(&self.running),
        })
    }
}

/// Releases an album's sync slot on drop, including on panic.
pub(crate) struct SyncGuard {
    album_id: i64,
    running: Arc<Mutex<HashSet<i64>>>,
}

impl Drop for SyncGuard {
    fn drop(&mut self) {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        running.remove(&self.album_id);
    }
}
