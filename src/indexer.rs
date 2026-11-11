//! Filesystem indexing for albums.
//!
//! A normal sync scans one album folder, non-recursively: media files become
//! rows in `media`, subdirectories become child albums that are themselves
//! indexed when their own info is requested. The `nested` option walks the
//! whole subtree in one job. `albums.indexed_at` is advanced only after a
//! successful scan, so a failed sync is retried on the next request.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use crate::index_hub::{IndexEvent, IndexHub, IndexPhase};
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

const ALBUM_SELECT: &str = "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, \
     created_at, updated_at FROM albums WHERE id = ?1";

/// Options for a manual index request.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexOptions {
    /// Re-index even when `indexed_at` is younger than an hour.
    pub force: bool,
    /// Walk and index the whole subtree, not just this album.
    pub nested: bool,
}

/// An album needs a scan when it was never indexed or its index is older than
/// one hour.
pub fn needs_sync(album: &Album) -> bool {
    match album.indexed_at {
        None => true,
        Some(indexed_at) => indexed_at < Utc::now() - Duration::hours(STALE_AFTER_HOURS),
    }
}

/// Why [`start`] did or did not start a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStart {
    /// The index is fresh enough; nothing was started.
    Fresh,
    /// This request spawned a new background task.
    Started,
    /// Another task for this album is already running.
    Running,
}

impl SyncStart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Started => "started",
            Self::Running => "running",
        }
    }
}

/// Automatic lazy sync used by `GET /api/albums/{hash}`.
pub fn start_if_stale(state: &AppState, album: &Album) -> SyncStart {
    start(
        state,
        album,
        IndexOptions {
            force: false,
            nested: false,
        },
    )
}

/// Starts a background index job.
///
/// Uniqueness is per album: a second request while a sync is in flight reports
/// [`SyncStart::Running`] instead of starting another task. Nested child albums
/// are claimed individually while the job walks the tree.
pub fn start(state: &AppState, album: &Album, options: IndexOptions) -> SyncStart {
    if !options.force && !needs_sync(album) {
        return SyncStart::Fresh;
    }
    let Some(guard) = state.try_claim_sync(album.id) else {
        return SyncStart::Running;
    };

    let hub = state.index_hub().clone();
    let job_id = hub.next_job_id();
    let state = state.clone();
    let album = album.clone();

    tokio::spawn(async move {
        let _guard = guard;
        let mut progress = Progress::new(hub, job_id, &album);
        progress.publish(&album, IndexPhase::Scanning, false, None);

        let result = if options.nested {
            sync_tree(&state, &album, &mut progress).await
        } else {
            sync_one(&state, &album, &mut progress, true)
                .await
                .map(|_| ())
        };

        match result {
            Ok(()) => progress.publish(&album, IndexPhase::Done, true, None),
            Err(err) => {
                eprintln!("sync of album `{}` failed: {err}", album.hash);
                progress.publish(&album, IndexPhase::Error, true, Some(err.to_string()));
            }
        }
    });

    SyncStart::Started
}

struct Progress {
    hub: IndexHub,
    job_id: u64,
    root_hash: String,
    files_done: u64,
    files_total: u64,
    dirs_done: u64,
    dirs_total: u64,
    new_albums: u64,
    new_media: u64,
}

impl Progress {
    fn new(hub: IndexHub, job_id: u64, album: &Album) -> Self {
        Self {
            hub,
            job_id,
            root_hash: album.hash.clone(),
            files_done: 0,
            files_total: 0,
            dirs_done: 0,
            dirs_total: 0,
            new_albums: 0,
            new_media: 0,
        }
    }

    fn publish(&self, album: &Album, phase: IndexPhase, done: bool, error: Option<String>) {
        self.hub.publish(IndexEvent {
            job_id: self.job_id,
            root_hash: self.root_hash.clone(),
            album_hash: album.hash.clone(),
            album_name: album.name.clone(),
            phase,
            files_done: self.files_done,
            files_total: self.files_total,
            dirs_done: self.dirs_done,
            dirs_total: self.dirs_total,
            new_albums: self.new_albums,
            new_media: self.new_media,
            done,
            error,
        });
    }
}

struct AlbumSyncStats {
    children: Vec<i64>,
}

async fn sync_one(
    state: &AppState,
    album: &Album,
    progress: &mut Progress,
    count_totals: bool,
) -> IndexResult<AlbumSyncStats> {
    let Some(dir) = state.album_dir(album) else {
        return Err(format!("album `{}` has an unsafe path", album.hash).into());
    };

    let scan = scan_dir(&dir, &album.path).await?;

    if count_totals {
        progress.files_total += scan.files.len() as u64;
        progress.dirs_total += scan.dirs.len() as u64;
        progress.publish(album, IndexPhase::Scanning, false, None);
    }

    // SQLite allows one writer; serializing here also prevents deferred
    // transactions from deadlocking each other on the upgrade to a write lock.
    let _write_guard = state.lock_writes().await;
    let mut tx = state.db.begin().await?;

    let existing: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM media WHERE album_id = ?1")
            .bind(album.id)
            .fetch_all(&mut *tx)
            .await?;

    let existing_names: HashSet<&str> = existing.iter().map(|(_, name)| name.as_str()).collect();
    let new_media = scan
        .files
        .iter()
        .filter(|file| !existing_names.contains(file.name.as_str()))
        .count() as u64;

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
    // lazily when their own info is requested, or as part of a nested job.
    let mut new_albums = 0_u64;
    for child in &scan.dirs {
        let inserted = sqlx::query(
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
        new_albums += inserted.rows_affected();
    }

    let children: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM albums WHERE parent_id = ?1 ORDER BY sort_name, id")
            .bind(album.id)
            .fetch_all(&mut *tx)
            .await?;

    sqlx::query("UPDATE albums SET indexed_at = ?1 WHERE id = ?2")
        .bind(Utc::now())
        .bind(album.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    progress.files_done += scan.files.len() as u64;
    progress.dirs_done += scan.dirs.len() as u64;
    progress.new_albums += new_albums;
    progress.new_media += new_media;
    progress.publish(album, IndexPhase::Indexing, false, None);

    Ok(AlbumSyncStats { children })
}

async fn sync_tree(state: &AppState, album: &Album, progress: &mut Progress) -> IndexResult<()> {
    let Some(dir) = state.album_dir(album) else {
        return Err(format!("album `{}` has an unsafe path", album.hash).into());
    };

    let canonical = tokio::fs::canonicalize(&dir).await?;
    let (files_total, dirs_total) = count_tree(&dir).await?;
    progress.files_total = files_total;
    progress.dirs_total = dirs_total;
    progress.publish(album, IndexPhase::Scanning, false, None);

    let mut visited_albums = HashSet::new();
    visited_albums.insert(album.id);
    let mut visited_dirs = HashSet::new();
    visited_dirs.insert(canonical);
    sync_subtree(
        state,
        album,
        progress,
        &mut visited_albums,
        &mut visited_dirs,
    )
    .await
}

async fn sync_subtree(
    state: &AppState,
    album: &Album,
    progress: &mut Progress,
    visited_albums: &mut HashSet<i64>,
    visited_dirs: &mut HashSet<PathBuf>,
) -> IndexResult<()> {
    let stats = sync_one(state, album, progress, false).await?;

    for child_id in stats.children {
        if !visited_albums.insert(child_id) {
            continue;
        }
        let child = load_album(&state.db, child_id).await?;
        let Some(child_guard) = state.try_claim_sync(child.id) else {
            // Another request owns this child; skip it rather than duplicate work.
            continue;
        };

        let child_dir = match state.album_dir(&child) {
            Some(dir) => dir,
            None => continue,
        };
        let canonical = match tokio::fs::canonicalize(&child_dir).await {
            Ok(path) => path,
            Err(err) => {
                eprintln!("skipping child `{}`: {err}", child_dir.display());
                continue;
            }
        };
        // A symlink can point at an ancestor (or itself); do not walk in circles.
        if !visited_dirs.insert(canonical) {
            continue;
        }

        let result = Box::pin(sync_subtree(
            state,
            &child,
            progress,
            visited_albums,
            visited_dirs,
        ))
        .await;
        drop(child_guard);
        result?;
    }

    Ok(())
}

async fn load_album(pool: &SqlitePool, id: i64) -> IndexResult<Album> {
    Ok(sqlx::query_as(ALBUM_SELECT)
        .bind(id)
        .fetch_one(pool)
        .await?)
}

/// Counts media files and subdirectories in the whole subtree.
///
/// Symlinks are followed; canonical paths are used to avoid walking in circles.
async fn count_tree(dir: &Path) -> std::io::Result<(u64, u64)> {
    let mut stack = vec![dir.to_path_buf()];
    let mut visited = HashSet::new();
    visited.insert(tokio::fs::canonicalize(dir).await?);
    let mut files = 0_u64;
    let mut dirs = 0_u64;

    while let Some(current) = stack.pop() {
        let mut read_dir = tokio::fs::read_dir(&current).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') || !is_safe_segment(&name) {
                continue;
            }

            let path = entry.path();
            // `metadata` follows symlinks, unlike `DirEntry::file_type`.
            let metadata = match tokio::fs::metadata(&path).await {
                Ok(metadata) => metadata,
                Err(err) => {
                    eprintln!("skipping `{}`: {err}", path.display());
                    continue;
                }
            };

            if metadata.is_dir() {
                let canonical = match tokio::fs::canonicalize(&path).await {
                    Ok(path) => path,
                    Err(err) => {
                        eprintln!("skipping `{}`: {err}", path.display());
                        continue;
                    }
                };
                if !visited.insert(canonical) {
                    continue;
                }
                dirs += 1;
                stack.push(path);
            } else if metadata.is_file() && media_type_for(&path).is_some() {
                files += 1;
            }
        }
    }

    Ok((files, dirs))
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
        let path = entry.path();
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

        // `metadata` follows symlinks, unlike `DirEntry::file_type`; a symlinked
        // album directory or media file is therefore indexed normally.
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("skipping `{}`: {err}", path.display());
                continue;
            }
        };

        if metadata.is_dir() {
            scan.dirs.push(ScannedDir {
                path: join_path(album_path, &name),
                name,
            });
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let Some(media_type) = media_type_for(&path) else {
            continue;
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

type IndexResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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
