//! Progress events for background album indexing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexPhase {
    Scanning,
    Indexing,
    Done,
    Error,
}

/// One progress notification for a running index job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEvent {
    pub job_id: u64,
    /// Album that started the job; SSE clients filter by this.
    pub root_hash: String,
    /// Album currently being processed (differs for nested jobs).
    pub album_hash: String,
    pub album_name: String,
    pub phase: IndexPhase,
    pub files_done: u64,
    pub files_total: u64,
    pub dirs_done: u64,
    pub dirs_total: u64,
    /// Counters of the album named by `album_hash` only, so a client viewing a
    /// descendant sees that album's own progress instead of job-wide numbers.
    pub album_files_done: u64,
    pub album_files_total: u64,
    pub album_dirs_done: u64,
    pub album_dirs_total: u64,
    pub new_albums: u64,
    pub new_media: u64,
    /// Whether this album actually gained, lost or changed rows since the
    /// previous event of the job. Clients do not refetch unchanged albums, so a
    /// repeated scan of an untouched folder costs no API requests.
    #[serde(default)]
    pub changed: bool,
    /// File or directory the job is working on right now; `None` between steps.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub current: Option<String>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

/// Broadcast hub plus the latest in-flight event per root album.
#[derive(Clone)]
pub struct IndexHub {
    sender: broadcast::Sender<IndexEvent>,
    latest: Arc<Mutex<HashMap<String, IndexEvent>>>,
    next_job: Arc<AtomicU64>,
}

impl Default for IndexHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            sender,
            latest: Arc::new(Mutex::new(HashMap::new())),
            next_job: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl IndexHub {
    pub fn subscribe(&self) -> broadcast::Receiver<IndexEvent> {
        self.sender.subscribe()
    }

    pub fn next_job_id(&self) -> u64 {
        self.next_job.fetch_add(1, Ordering::Relaxed)
    }

    /// Progress of a job that is still running, for clients that connect late.
    pub fn latest(&self, root_hash: &str) -> Option<IndexEvent> {
        self.latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(root_hash)
            .cloned()
    }

    /// Progress of every job that is still running, for a global subscriber.
    pub fn latest_all(&self) -> Vec<IndexEvent> {
        self.latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn publish(&self, event: IndexEvent) {
        {
            let mut latest = self
                .latest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if event.done {
                latest.remove(&event.root_hash);
            } else {
                latest.insert(event.root_hash.clone(), event.clone());
            }
        }

        // No subscribers is a normal case, not an error.
        let _ = self.sender.send(event);
    }
}
