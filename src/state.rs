use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::config::AppConfig;
use crate::config::tools::Capabilities;
use crate::helpers::secure_link;
use crate::hubs::{IndexHub, ThumbHub, TranscodeHub};
use crate::hubs::sync_tracker::{SyncGuard, SyncTracker};
use crate::models::{Album, Media, ROOT_ALBUM_HASH};

/// Shared application state, cloned into every handler by axum.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    /// All runtime settings. Cheap to clone, shared by every request.
    pub config: Arc<AppConfig>,
    /// Which optional media tools are usable in this environment.
    pub capabilities: Capabilities,
    /// Per-process secret for media link signatures.
    secret: Arc<[u8; 32]>,
    /// Albums with a background indexing task in flight.
    syncing: SyncTracker,
    /// Serializes indexing transactions: SQLite has one writer at a time.
    write_gate: Arc<tokio::sync::Mutex<()>>,
    /// Progress broadcast for running index jobs.
    index_hub: IndexHub,
    /// Deduplicates concurrent thumbnail generation requests.
    thumb_hub: ThumbHub,
    /// Deduplicates concurrent transcode generation requests.
    transcode_hub: TranscodeHub,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Arc<AppConfig>) -> Self {
        Self {
            db,
            config,
            capabilities: Capabilities::assume_available(),
            secret: Arc::new(random_secret()),
            write_gate: Arc::new(tokio::sync::Mutex::new(())),
            syncing: SyncTracker::default(),
            index_hub: IndexHub::default(),
            thumb_hub: ThumbHub::default(),
            transcode_hub: TranscodeHub::default(),
        }
    }

    /// Opens the database, creating the file if needed, then applies migrations
    /// and bootstraps the storage (root album + media directory).
    pub async fn connect(config: Arc<AppConfig>) -> Result<Self, Box<dyn std::error::Error>> {
        // A URL can specify modes and pragmas, but it also needs escaping for
        // Windows paths, so the default path is passed as a plain filename.
        let options = match config.database_url.as_deref() {
            Some(url) => SqliteConnectOptions::from_str(url)?,
            None => SqliteConnectOptions::new().filename(&config.database_file),
        }
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

        let capabilities = Capabilities::detect(&config.ffmpeg_bin, &config.ffprobe_bin);
        let mut state = Self::new(db, config);
        state.capabilities = capabilities;
        state.initialize().await?;
        Ok(state)
    }

    /// Startup-only bootstrap: makes sure the media directory and the root
    /// album row exist. Requests never create them.
    pub async fn initialize(&self) -> Result<(), Box<dyn std::error::Error>> {
        tokio::fs::create_dir_all(&self.config.media_dir).await?;
        // Previews live in the cache root, never inside the media tree, so the
        // indexer cannot mistake them for albums or media.
        tokio::fs::create_dir_all(self.thumb_dir()).await?;
        // Original-size frames are shared by every video preview size.
        tokio::fs::create_dir_all(self.config.frames_dir()).await?;
        // Video-previews in cache for video and animations
        tokio::fs::create_dir_all(self.config.transcode_dir()).await?;

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
        self.config.thumb_dir()
    }

    /// Cached original-size frame for a video or animation.
    pub fn frame_path(&self, media_hash: &str) -> PathBuf {
        self.config.frames_dir().join(format!("{media_hash}.png"))
    }

    /// Short-lived signature for `/thumb/v1` and `/file/v1`.
    pub fn sign_media(&self, album_hash: &str, media_hash: &str, exp: i64) -> String {
        secure_link::token(self.secret.as_ref(), album_hash, media_hash, exp)
    }

    /// Validates a media link signature and its expiry.
    pub fn verify_media(&self, album_hash: &str, media_hash: &str, token: &str, exp: i64) -> bool {
        secure_link::verify(
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
        let mut dir = self.config.media_dir.clone();

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

    /// Shared thumbnail deduplication hub.
    pub(crate) fn thumb_hub(&self) -> &ThumbHub {
        &self.thumb_hub
    }
    /// Shared transcode deduplication hub.
    pub(crate) fn transcode_hub(&self) -> &TranscodeHub {
        &self.transcode_hub
    }

    /// Highest number of FFmpeg preview jobs that ran at the same time.
    ///
    /// Exposed so tests and diagnostics can assert the concurrency cap.
    pub fn peak_transcodes(&self) -> usize {
        self.transcode_hub.peak_concurrency()
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
