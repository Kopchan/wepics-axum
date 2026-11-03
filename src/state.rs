use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::models::ROOT_ALBUM_HASH;

/// Shared application state, cloned into every handler by axum.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    /// Storage root, `./data/media` by default.
    pub media_dir: std::path::PathBuf,
}

impl AppState {
    pub fn new(db: SqlitePool, media_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            db,
            media_dir: media_dir.into(),
        }
    }

    /// Opens the database, creating the file if needed, then applies migrations
    /// and bootstraps the storage (root album + media directory).
    pub async fn connect(
        database_url: &str,
        media_dir: impl Into<std::path::PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            // SQLite does not enforce FKs unless asked to.
            .foreign_keys(true);

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

        let state = Self::new(db, media_dir);
        state.initialize().await?;
        Ok(state)
    }

    /// Startup-only bootstrap: makes sure the media directory and the root
    /// album row exist. Requests never create them.
    pub async fn initialize(&self) -> Result<(), Box<dyn std::error::Error>> {
        tokio::fs::create_dir_all(&self.media_dir).await?;

        // `OR IGNORE` makes restarts idempotent.
        sqlx::query("INSERT OR IGNORE INTO albums (name, hash, sort_name) VALUES (?1, ?1, ?2)")
            .bind(ROOT_ALBUM_HASH)
            .bind(crate::models::natural_sort_key(ROOT_ALBUM_HASH))
            .execute(&self.db)
            .await?;

        Ok(())
    }

    /// Path of a stored file: `<media>/<mediaHash><ext>` for the root album,
    /// `<media>/<albumHash>/<mediaHash><ext>` otherwise.
    ///
    /// Returns `None` if either hash cannot safely be used as a path segment.
    pub fn media_path(
        &self,
        album: &crate::models::Album,
        media: &crate::models::Media,
    ) -> Option<std::path::PathBuf> {
        if !is_safe_segment(&album.hash) || !is_safe_segment(&media.hash) {
            return None;
        }

        let dir = if album.hash == ROOT_ALBUM_HASH {
            self.media_dir.clone()
        } else {
            self.media_dir.join(&album.hash)
        };

        let ext = std::path::Path::new(&media.name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!(".{ext}"))
            .unwrap_or_default();

        Some(dir.join(format!("{}{ext}", media.hash)))
    }
}

fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\', '\0'])
}
