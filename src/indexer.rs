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

use crate::helpers::hashes;
use crate::hubs::indexer::{IndexEvent, IndexHub, IndexPhase};
use crate::models::{Album, MediaType, natural_sort_key};
use crate::state::{AppState, is_safe_segment};

/// How many media rows are written per transaction.
const INSERT_BATCH: usize = 100;

/// An album is re-indexed when its index is older than this.
const STALE_AFTER_HOURS: i64 = 1;

const UPSERT_MEDIA: &str = r#"
INSERT INTO media (name, hash, "type", album_id, width, height, size, date, sort_name)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(album_id, name) DO UPDATE SET
    hash = excluded.hash,
    "type" = excluded."type",
    width = excluded.width,
    height = excluded.height,
    size = excluded.size,
    date = excluded.date,
    sort_name = excluded.sort_name
WHERE media.hash <> excluded.hash
   OR media."type" <> excluded."type"
   OR media.width <> excluded.width
   OR media.height <> excluded.height
   OR media.size <> excluded.size
   OR media.date IS NOT excluded.date
   OR media.sort_name <> excluded.sort_name
"#;

const ALBUM_SELECT: &str = "SELECT id, name, hash, parent_id, path, alias, old_aliases, \
     rating_level, order_level, guest_allow, indexed_at, sort_name, \
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

    // Listing names is cheap (no per-file `metadata`), so the total is known
    // before the slow part starts and the progress bar can advance smoothly.
    let entries = read_sorted_entries(&dir)
        .await
        .map_err(|err| step("read album directory", err))?;
    let (dirs, media) = classify_entries(entries, &album.path).await;

    // Directories are fully discovered by the listing pass, so count them as
    // done right away; only the (potentially long) metadata/insert phase is
    // left to report.
    progress.dirs_done += dirs.len() as u64;

    if count_totals {
        progress.files_total += media.len() as u64;
        progress.dirs_total += dirs.len() as u64;
        progress.publish(album, IndexPhase::Scanning, false, None);
    }

    // Snapshot of the current rows: used to count new media and to delete rows
    // whose files disappeared. The album is claimed exclusively, so a plain
    // read is enough here.
    let existing: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM media WHERE album_id = ?1")
            .bind(album.id)
            .fetch_all(&state.db)
            .await
            .map_err(|err| step("select existing media", err))?;

    let existing_names: HashSet<&str> = existing.iter().map(|(_, name)| name.as_str()).collect();
    let observed: HashSet<&str> = media.iter().map(|entry| entry.name.as_str()).collect();

    // Scanning and writing are interleaved: every 100 files are committed in
    // their own transaction while the rest of the folder is still being read.
    // This keeps the write lock free between batches, makes progress visible
    // throughout the scan instead of in one burst at the end, and is safe on
    // failure because upserts are idempotent (`indexed_at` is only bumped once
    // everything, including deletions, has landed).
    let mut batch: Vec<ScannedFile> = Vec::with_capacity(INSERT_BATCH);
    let mut batch_index = 0_usize;

    for entry in &media {
        let metadata = match tokio::fs::metadata(&entry.path).await {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("skipping `{}`: {err}", entry.path.display());
                progress.files_done += 1;
                continue;
            }
        };

        if !existing_names.contains(entry.name.as_str()) {
            progress.new_media += 1;
        }

        let (width, height, hash) = match probe_file(entry).await {
            Ok(probed) => probed,
            Err(err) => {
                eprintln!("skipping `{}`: {err}", entry.path.display());
                progress.files_done += 1;
                continue;
            }
        };

        batch.push(ScannedFile {
            name: entry.name.clone(),
            hash,
            media_type: entry.media_type,
            width,
            height,
            size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            date: metadata.modified().ok().map(DateTime::<Utc>::from),
            sort_name: natural_sort_key(&entry.name),
        });
        progress.files_done += 1;

        if batch.len() >= INSERT_BATCH {
            write_media_batch(state, album, &batch, batch_index).await?;
            batch_index += 1;
            batch.clear();
            progress.publish(album, IndexPhase::Indexing, false, None);
        }
    }

    if !batch.is_empty() {
        write_media_batch(state, album, &batch, batch_index).await?;
        progress.publish(album, IndexPhase::Indexing, false, None);
    }

    let (children, new_albums) = {
        let _write_guard = state.lock_writes().await;
        let mut tx = state
            .db
            .begin()
            .await
            .map_err(|err| step("begin metadata transaction", err))?;

        for (id, name) in &existing {
            if !observed.contains(name.as_str()) {
                sqlx::query("DELETE FROM media WHERE id = ?1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| step(&format!("delete `{name}`"), err))?;
            }
        }

        // Subdirectories are child albums. They are created here and indexed
        // lazily when their own info is requested, or as part of a nested job.
        let mut new_albums = 0_u64;
        for child in &dirs {
            let inserted = sqlx::query(
                "INSERT INTO albums (name, hash, sort_name, parent_id, path)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO NOTHING",
            )
            .bind(&child.name)
            .bind(hashes::album_hash())
            .bind(natural_sort_key(&child.name))
            .bind(album.id)
            .bind(&child.path)
            .execute(&mut *tx)
            .await
            .map_err(|err| step(&format!("insert child album `{}`", child.name), err))?;
            new_albums += inserted.rows_affected();
        }

        let children: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM albums WHERE parent_id = ?1 ORDER BY sort_name, id")
                .bind(album.id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|err| step("select child albums", err))?;

        sqlx::query("UPDATE albums SET indexed_at = ?1 WHERE id = ?2")
            .bind(Utc::now())
            .bind(album.id)
            .execute(&mut *tx)
            .await
            .map_err(|err| step("update album indexed_at", err))?;

        tx.commit()
            .await
            .map_err(|err| step("commit metadata transaction", err))?;

        (children, new_albums)
    };

    progress.new_albums += new_albums;
    progress.publish(album, IndexPhase::Indexing, false, None);

    Ok(AlbumSyncStats { children })
}

async fn write_media_batch(
    state: &AppState,
    album: &Album,
    batch: &[ScannedFile],
    batch_index: usize,
) -> IndexResult<()> {
    let write_guard = state.lock_writes().await;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|err| step("begin media batch", err))?;

    for file in batch {
        sqlx::query(UPSERT_MEDIA)
            .bind(&file.name)
            .bind(&file.hash)
            .bind(file.media_type)
            .bind(album.id)
            .bind(file.width)
            .bind(file.height)
            .bind(file.size)
            .bind(file.date)
            .bind(&file.sort_name)
            .execute(&mut *tx)
            .await
            .map_err(|err| {
                step(
                    &format!("upsert `{}` (batch {})", file.name, batch_index + 1),
                    err,
                )
            })?;
    }

    tx.commit()
        .await
        .map_err(|err| step(&format!("commit media batch {}", batch_index + 1), err))?;
    drop(write_guard);
    Ok(())
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
    hash: String,
    media_type: MediaType,
    width: i64,
    height: i64,
    size: i64,
    date: Option<DateTime<Utc>>,
    sort_name: String,
}

struct ScannedDir {
    name: String,
    path: String,
}

struct MediaEntry {
    name: String,
    path: PathBuf,
    media_type: MediaType,
}

async fn read_sorted_entries(dir: &Path) -> std::io::Result<Vec<tokio::fs::DirEntry>> {
    let mut read_dir = tokio::fs::read_dir(dir).await?;

    let mut entries = Vec::new();
    while let Some(entry) = read_dir.next_entry().await? {
        entries.push(entry);
    }
    // Deterministic insertion order keeps row ids stable across scans.
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// Splits directory entries into child albums and media files.
///
/// Only the cached `DirEntry::file_type` is used, so this pass is cheap. The
/// expensive per-file `metadata` call happens later, while batches are written,
/// which keeps progress moving on large folders.
async fn classify_entries(
    entries: Vec<tokio::fs::DirEntry>,
    album_path: &str,
) -> (Vec<ScannedDir>, Vec<MediaEntry>) {
    let mut dirs = Vec::new();
    let mut media = Vec::new();

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

        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            Err(err) => {
                eprintln!("skipping `{}`: {err}", path.display());
                continue;
            }
        };

        // `file_type` does not follow symlinks, unlike `metadata`; a symlinked
        // album directory or media file is still indexed normally.
        let followed = if file_type.is_symlink() {
            match tokio::fs::metadata(&path).await {
                Ok(metadata) => Some(metadata),
                Err(err) => {
                    eprintln!("skipping `{}`: {err}", path.display());
                    continue;
                }
            }
        } else {
            None
        };

        let is_dir = followed
            .as_ref()
            .map(std::fs::Metadata::is_dir)
            .unwrap_or_else(|| file_type.is_dir());
        if is_dir {
            dirs.push(ScannedDir {
                path: join_path(album_path, &name),
                name,
            });
            continue;
        }

        let is_file = followed
            .as_ref()
            .map(std::fs::Metadata::is_file)
            .unwrap_or_else(|| file_type.is_file());
        if !is_file {
            continue;
        }

        let Some(media_type) = media_type_for(&path) else {
            continue;
        };
        media.push(MediaEntry {
            name,
            path,
            media_type,
        });
    }

    (dirs, media)
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// Reads image dimensions and hashes the file in one blocking pass.
///
/// Still and animated images use a lightweight header parser; vectors need
/// libvips because SVG has no raster header. Videos and audio keep `0`, which
/// the API treats as "unknown". A malformed or unsupported file is not an
/// indexing error, it simply has no dimensions.
async fn probe_file(entry: &MediaEntry) -> std::io::Result<(i64, i64, String)> {
    let path = entry.path.clone();
    let media_type = entry.media_type;
    tokio::task::spawn_blocking(move || {
        let (width, height) = probe_dimensions(&path, media_type);
        let hash = hashes::media_hash(&path)?;
        Ok((width, height, hash))
    })
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))?
}

fn probe_dimensions(path: &Path, media_type: MediaType) -> (i64, i64) {
    if !matches!(
        media_type,
        MediaType::Image | MediaType::Animated | MediaType::Vector
    ) {
        return (0, 0);
    }

    let dimensions = if media_type == MediaType::Vector {
        vips_dimensions(path)
    } else {
        imagesize::size(path)
            .map(|size| (size.width, size.height))
            .ok()
    };

    match dimensions {
        Some((width, height)) => (
            i64::try_from(width).unwrap_or(i64::MAX),
            i64::try_from(height).unwrap_or(i64::MAX),
        ),
        None => (0, 0),
    }
}

/// Vector dimensions through libvips. SVG is rendered lazily, so this only
/// parses the document and reports its intrinsic size.
fn vips_dimensions(path: &Path) -> Option<(usize, usize)> {
    crate::helpers::vips::ensure().ok()?;
    let image = vips::VipsImage::from_file(path).ok()?;
    Some((image.width() as usize, image.height() as usize))
}

/// Maps a file extension to the media type stored in SQLite.
pub fn media_type_for(path: &Path) -> Option<MediaType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();

    match extension.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif" | "avif" => {
            Some(MediaType::Image)
        }
        "gif" | "apng" => Some(MediaType::Animated),
        "svg" => Some(MediaType::Vector),
        "mp4" | "m4v" | "mkv" | "mov" | "webm" | "avi" | "mpg" | "mpeg" | "wmv" | "flv" => {
            Some(MediaType::Video)
        }
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" | "wma" => Some(MediaType::Audio),
        _ => None,
    }
}

type IndexResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Adds the failing step to an error, so a bare `(code: 14)` becomes actionable.
fn step<E: std::fmt::Display>(context: &str, error: E) -> Box<dyn std::error::Error + Send + Sync> {
    format!("{context}: {error}").into()
}
