use std::collections::HashSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::index_hub::IndexHub;
use crate::models::{Album, Media, ROOT_ALBUM_HASH};

/// Shared application state, cloned into every handler by axum.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    /// Storage root, `./data/media` by default.
    pub media_dir: PathBuf,
    /// Generated artifacts, `./data/cache` by default. Kept outside the media
    /// tree so previews can never be indexed as albums or media.
    pub cache_dir: PathBuf,
    /// Per-process secret for media link signatures.
    secret: Arc<[u8; 32]>,
    /// Albums with a background indexing task in flight.
    syncing: SyncTracker,
    /// Serializes indexing transactions: SQLite has one writer at a time.
    write_gate: Arc<tokio::sync::Mutex<()>>,
    /// Progress broadcast for running index jobs.
    index_hub: IndexHub,
}

impl AppState {
    pub fn new(
        db: SqlitePool,
        media_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            db,
            media_dir: media_dir.into(),
            cache_dir: cache_dir.into(),
            secret: Arc::new(random_secret()),
            syncing: SyncTracker::default(),
            write_gate: Arc::new(tokio::sync::Mutex::new(())),
            index_hub: IndexHub::default(),
        }
    }

    /// Opens the database, creating the file if needed, then applies migrations
    /// and bootstraps the storage (root album + media directory).
    pub async fn connect(
        database_url: &str,
        media_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            // SQLite does not enforce FKs unless asked to.
            .foreign_keys(true)
            // Background album syncs used to collide on the rollback journal.
            .journal_mode(SqliteJournalMode::Wal)
            // Every upsert opens a statement journal. On this Windows setup
            // creating those temp files eventually fails with
            // `(code: 14) unable to open database file`, so keep them in RAM.
            .pragma("temp_store", "MEMORY")
            .busy_timeout(Duration::from_secs(30));

        // `create_if_missing` creates the file, not its parent directory.
        if let Some(parent) = options.get_filename().parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let db = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&db).await?;

        let state = Self::new(db, media_dir, cache_dir);
        state.initialize().await?;
        Ok(state)
    }

    /// Startup-only bootstrap: makes sure the media directory and the root
    /// album row exist. Requests never create them.
    pub async fn initialize(&self) -> Result<(), Box<dyn std::error::Error>> {
        tokio::fs::create_dir_all(&self.media_dir).await?;
        // Previews live in the cache root, never inside the media tree, so the
        // indexer cannot mistake them for albums or media.
        tokio::fs::create_dir_all(self.thumb_dir()).await?;

        // `OR IGNORE` makes restarts idempotent.
        sqlx::query("INSERT OR IGNORE INTO albums (name, hash, sort_name) VALUES (?1, ?1, ?2)")
            .bind(ROOT_ALBUM_HASH)
            .bind(crate::models::natural_sort_key(ROOT_ALBUM_HASH))
            .execute(&self.db)
            .await?;

        Ok(())
    }

    /// Root directory for generated previews.
    pub fn thumb_dir(&self) -> PathBuf {
        self.cache_dir.join("thumbs").join("v1")
    }

    /// Short-lived signature for `/thumb/v1` and `/file/v1`.
    pub fn sign_media(&self, album_hash: &str, media_hash: &str, exp: i64) -> String {
        crate::secure_link::token(self.secret.as_ref(), album_hash, media_hash, exp)
    }

    /// Validates a media link signature and its expiry.
    pub fn verify_media(&self, album_hash: &str, media_hash: &str, token: &str, exp: i64) -> bool {
        crate::secure_link::verify(
            self.secret.as_ref(),
            album_hash,
            media_hash,
            token,
            exp,
            chrono::Utc::now().timestamp(),
        )
    }

    /// Directory that holds an album's files, relative to the storage root.
    ///
    /// The root album has an empty [`Album::path`]; every other album maps to a
    /// subdirectory. Returns `None` if any path segment is unsafe.
    pub fn album_dir(&self, album: &Album) -> Option<PathBuf> {
        let mut dir = self.media_dir.clone();

        for segment in album.path.split('/').filter(|segment| !segment.is_empty()) {
            if !is_safe_segment(segment) {
                return None;
            }
            dir.push(segment);
        }

        Some(dir)
    }

    /// Path of a stored file: `<media>/<name>` for the root album,
    /// `<media>/<albumPath>/<name>` otherwise.
    ///
    /// Returns `None` if any path segment is unsafe.
    pub fn media_path(&self, album: &Album, media: &Media) -> Option<PathBuf> {
        if !is_safe_segment(&media.name) {
            return None;
        }
        let dir = self.album_dir(album)?;

        Some(dir.join(&media.name))
    }

    /// Marks `album_id` as syncing. `None` means another task already owns it.
    pub(crate) fn try_claim_sync(&self, album_id: i64) -> Option<SyncGuard> {
        self.syncing.try_claim(album_id)
    }

    /// Serializes database-writing sync transactions across albums.
    pub(crate) async fn lock_writes(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.write_gate.lock().await
    }

    pub(crate) fn index_hub(&self) -> &IndexHub {
        &self.index_hub
    }
}

pub(crate) fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\', '\0'])
}

/// Salt for link signatures. Falls back to a process-unique value if the OS
/// random source is unavailable, which keeps the server usable and still makes
/// links invalid after a restart.
fn random_secret() -> [u8; 32] {
    let mut secret = [0_u8; 32];
    if getrandom::fill(&mut secret).is_ok() {
        return secret;
    }

    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(now.as_nanos().to_le_bytes());
    }
    hasher.update(std::process::id().to_le_bytes());
    secret.copy_from_slice(&hasher.finalize());
    secret
}

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

#[cfg(test)]
mod tests {
    use super::SyncTracker;

    #[test]
    fn sync_claim_is_unique_per_album() {
        let tracker = SyncTracker::default();

        let guard = tracker.try_claim(7).expect("first claim wins");
        assert!(
            tracker.try_claim(7).is_none(),
            "second claim must be refused"
        );
        assert!(
            tracker.try_claim(8).is_some(),
            "other albums are independent"
        );

        drop(guard);
        assert!(
            tracker.try_claim(7).is_some(),
            "slot is released when the task ends"
        );
    }
}
