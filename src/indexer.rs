//! Filesystem indexing for albums.
//!
//! A sync scans one album folder, non-recursively: media files become rows in
//! `media`, subdirectories become child albums that are themselves indexed when
//! their own info is requested. `albums.indexed_at` is advanced only after a
//! successful scan, so a failed sync is retried on the next request.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};

use crate::models::{Album, MediaType, natural_sort_key};
use crate::state::{AppState, is_safe_segment};

/// An album is re-indexed when its index is older than this.
const STALE_AFTER_HOURS: i64 = 1;

const UPSERT_MEDIA: &str = r#"
INSERT INTO media (name, hash, "type", album_id, size, date, sort_name)
VALUES (?1, lower(hex(randomblob(12))), ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(album_id, name) DO UPDATE SET
    "type" = excluded."type",
    size = excluded.size,
    date = excluded.date,
    sort_name = excluded.sort_name
WHERE media."type" <> excluded."type"
   OR media.size <> excluded.size
   OR media.date IS NOT excluded.date
   OR media.sort_name <> excluded.sort_name
"#;

/// An album needs a scan when it was never indexed or its index is older than
/// one hour.
pub fn needs_sync(album: &Album) -> bool {
    match album.indexed_at {
        None => true,
        Some(indexed_at) => indexed_at < Utc::now() - Duration::hours(STALE_AFTER_HOURS),
    }
}

/// Why [`start_if_stale`] did or did not start a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStart {
    /// The index is fresh enough; nothing was started.
    Fresh,
    /// This request spawned a new background task.
    Started,
    /// Another task for this album is already running.
    Running,
}

/// Starts a background sync when the album is stale and no task is running for
/// it.
///
/// Uniqueness is per album: a second request while a sync is in flight reports
/// [`SyncStart::Running`] instead of starting another task.
pub fn start_if_stale(state: &AppState, album: &Album) -> SyncStart {
    if !needs_sync(album) {
        return SyncStart::Fresh;
    }
    let Some(guard) = state.try_claim_sync(album.id) else {
        return SyncStart::Running;
    };

    let state = state.clone();
    let album = album.clone();
    tokio::spawn(async move {
        let _guard = guard;
        if let Err(err) = sync_album(&state, &album).await {
            eprintln!("sync of album `{}` failed: {err}", album.hash);
        }
    });

    SyncStart::Started
}

async fn sync_album(
    state: &AppState,
    album: &Album,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(dir) = state.album_dir(album) else {
        return Err(format!("album `{}` has an unsafe path", album.hash).into());
    };

    let scan = scan_dir(&dir, &album.path).await?;

    let mut tx = state.db.begin().await?;

    let existing: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM media WHERE album_id = ?1")
            .bind(album.id)
            .fetch_all(&mut *tx)
            .await?;

    for file in &scan.files {
        sqlx::query(UPSERT_MEDIA)
            .bind(&file.name)
            .bind(file.media_type)
            .bind(album.id)
            .bind(file.size)
            .bind(file.date)
            .bind(&file.sort_name)
            .execute(&mut *tx)
            .await?;
    }

    let observed: HashSet<&str> = scan.files.iter().map(|file| file.name.as_str()).collect();
    for (id, name) in &existing {
        if !observed.contains(name.as_str()) {
            sqlx::query("DELETE FROM media WHERE id = ?1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }

    // Subdirectories are child albums. They are created here and indexed
    // lazily when their own info is requested.
    for child in &scan.dirs {
        sqlx::query(
            "INSERT INTO albums (name, hash, sort_name, parent_id, path)
             VALUES (?1, lower(hex(randomblob(12))), ?2, ?3, ?4)
             ON CONFLICT(path) DO NOTHING",
        )
        .bind(&child.name)
        .bind(natural_sort_key(&child.name))
        .bind(album.id)
        .bind(&child.path)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("UPDATE albums SET indexed_at = ?1 WHERE id = ?2")
        .bind(Utc::now())
        .bind(album.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

struct ScannedFile {
    name: String,
    media_type: MediaType,
    size: i64,
    date: Option<DateTime<Utc>>,
    sort_name: String,
}

struct ScannedDir {
    name: String,
    path: String,
}

struct Scan {
    files: Vec<ScannedFile>,
    dirs: Vec<ScannedDir>,
}

async fn scan_dir(dir: &Path, album_path: &str) -> std::io::Result<Scan> {
    let mut read_dir = tokio::fs::read_dir(dir).await?;

    let mut entries = Vec::new();
    while let Some(entry) = read_dir.next_entry().await? {
        entries.push(entry);
    }
    // Deterministic insertion order keeps row ids stable across scans.
    entries.sort_by_key(|entry| entry.file_name());

    let mut scan = Scan {
        files: Vec::new(),
        dirs: Vec::new(),
    };

    for entry in entries {
        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            Err(err) => {
                eprintln!("skipping `{}`: {err}", entry.path().display());
                continue;
            }
        };

        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            eprintln!(
                "skipping non-UTF-8 file name in `{}`",
                entry.path().display()
            );
            continue;
        };
        if name.starts_with('.') || !is_safe_segment(&name) {
            continue;
        }

        if file_type.is_dir() {
            scan.dirs.push(ScannedDir {
                path: join_path(album_path, &name),
                name,
            });
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        let Some(media_type) = media_type_for(&path) else {
            continue;
        };

        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("skipping `{name}`: {err}");
                continue;
            }
        };

        scan.files.push(ScannedFile {
            name: name.clone(),
            media_type,
            size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            date: metadata.modified().ok().map(DateTime::<Utc>::from),
            sort_name: natural_sort_key(&name),
        });
    }

    Ok(scan)
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn media_type_for(path: &Path) -> Option<MediaType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();

    match extension.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif" | "avif"
        | "svg" => Some(MediaType::Image),
        "gif" | "apng" => Some(MediaType::Animated),
        "mp4" | "m4v" | "mkv" | "mov" | "webm" | "avi" | "mpg" | "mpeg" | "wmv" | "flv" => {
            Some(MediaType::Video)
        }
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" | "wma" => Some(MediaType::Audio),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{media_type_for, needs_sync};
    use crate::models::{Album, MediaType};
    use chrono::{Duration, Utc};
    use std::path::Path;

    fn album(indexed_at: Option<chrono::DateTime<Utc>>) -> Album {
        let now = Utc::now();
        Album {
            id: 1,
            name: "album".into(),
            hash: "hash".into(),
            parent_id: None,
            path: "album".into(),
            sort_name: "album".into(),
            indexed_at,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn freshness_threshold_is_one_hour() {
        assert!(needs_sync(&album(None)));
        assert!(!needs_sync(&album(Some(
            Utc::now() - Duration::minutes(30)
        ))));
        assert!(needs_sync(&album(Some(Utc::now() - Duration::minutes(61)))));
    }

    #[test]
    fn classifies_media_extensions() {
        assert_eq!(
            media_type_for(Path::new("photo.JPG")),
            Some(MediaType::Image)
        );
        assert_eq!(
            media_type_for(Path::new("clip.mp4")),
            Some(MediaType::Video)
        );
        assert_eq!(
            media_type_for(Path::new("song.flac")),
            Some(MediaType::Audio)
        );
        assert_eq!(
            media_type_for(Path::new("anim.gif")),
            Some(MediaType::Animated)
        );
        assert_eq!(media_type_for(Path::new("notes.txt")), None);
    }
}
