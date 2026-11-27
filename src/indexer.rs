//! Filesystem indexing for albums.
//!
//! A normal sync scans one album folder, non-recursively: media files become
//! rows in `media`, subdirectories become child albums that are themselves
//! indexed when their own info is requested. The `nested` option walks the
//! whole subtree in one job. `albums.indexed_at` is advanced only after a
//! successful scan, so a failed sync is retried on the next request.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::helpers::hashes;
use crate::hubs::indexer::{IndexEvent, IndexHub, IndexPhase};
use crate::models::{Album, MediaType, natural_sort_key};
use crate::state::{AppState, is_safe_segment};

/// How many media rows are written per transaction.
const INSERT_BATCH: usize = 100;

/// Version of the hash input. Bump this whenever [`hashes::media_hash`] changes
/// what it hashes: rows written with an older version are re-hashed once instead
/// of being trusted forever. Migration `0006` defaults existing rows to `0`.
const MEDIA_HASH_VERSION: i64 = 1;

/// Minimum spacing between progress events while walking files. Large folders
/// would otherwise flood the broadcast channel with one event per file.
const PROGRESS_INTERVAL: StdDuration = StdDuration::from_millis(200);

/// An album is re-indexed when its index is older than this.
const STALE_AFTER_HOURS: i64 = 1;

const UPSERT_MEDIA: &str = r#"
INSERT INTO media (name, hash, "type", album_id, width, height, duration_sec, framerate,
                   frame_count, size, date, sort_name, hash_version)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
ON CONFLICT(album_id, name) DO UPDATE SET
    hash = excluded.hash,
    "type" = excluded."type",
    width = excluded.width,
    height = excluded.height,
    duration_sec = excluded.duration_sec,
    framerate = excluded.framerate,
    frame_count = excluded.frame_count,
    size = excluded.size,
    date = excluded.date,
    sort_name = excluded.sort_name,
    hash_version = excluded.hash_version
RETURNING id
"#;

const UPSERT_DUPLICA: &str = r#"
INSERT INTO duplica (name, media_id, album_id, data)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(album_id, name) DO UPDATE SET
    media_id = excluded.media_id,
    data = excluded.data
WHERE duplica.media_id <> excluded.media_id
   OR duplica.data <> excluded.data
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
            Ok(()) => progress.finish(&album, IndexPhase::Done, None),
            Err(err) => {
                eprintln!("ERR: sync of album `{}` failed: {err}", album.hash);
                progress.finish(&album, IndexPhase::Error, Some(err.to_string()));
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
    /// Counters of the album currently being scanned, reset by each `sync_one`.
    album_files_done: u64,
    album_files_total: u64,
    album_dirs_done: u64,
    album_dirs_total: u64,
    new_albums: u64,
    new_media: u64,
    /// Rows of the album currently being scanned actually changed; reset by each
    /// `sync_one`, so every event describes only its own album.
    album_changed: bool,
    /// Any album of this job changed; reported by the final event.
    job_changed: bool,
    /// File or directory being processed right now.
    current: Option<String>,
    /// When the last event was published, used to throttle per-file updates.
    published_at: Instant,
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
            album_files_done: 0,
            album_files_total: 0,
            album_dirs_done: 0,
            album_dirs_total: 0,
            new_albums: 0,
            new_media: 0,
            album_changed: false,
            job_changed: false,
            current: None,
            // Allow the first throttled update to go out immediately.
            published_at: Instant::now() - PROGRESS_INTERVAL,
        }
    }

    fn publish(&mut self, album: &Album, phase: IndexPhase, done: bool, error: Option<String>) {
        self.published_at = Instant::now();
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
            album_files_done: self.album_files_done,
            album_files_total: self.album_files_total,
            album_dirs_done: self.album_dirs_done,
            album_dirs_total: self.album_dirs_total,
            new_albums: self.new_albums,
            new_media: self.new_media,
            // A finished job reports its whole subtree, a running one only the
            // album it is working on.
            changed: if done {
                self.job_changed
            } else {
                self.album_changed
            },
            current: self.current.clone(),
            done,
            error,
        });
    }

    /// Publishes at most one event per [`PROGRESS_INTERVAL`], so a folder with
    /// thousands of files still reports the current name without flooding SSE.
    fn publish_throttled(&mut self, album: &Album, phase: IndexPhase) {
        if !self.should_publish() {
            return;
        }
        self.publish(album, phase, false, None);
    }

    /// Remembers that the current album's rows really changed, so clients know
    /// they have something new to fetch.
    fn mark_changed(&mut self) {
        self.album_changed = true;
        self.job_changed = true;
    }

    /// Whether the throttle window has passed since the last publish.
    fn should_publish(&self) -> bool {
        self.published_at.elapsed() >= PROGRESS_INTERVAL
    }

    /// Marks one media file as processed for the job and for its album.
    fn file_done(&mut self) {
        self.files_done += 1;
        self.album_files_done += 1;
    }

    /// Final event; the job counters become the album counters, so a client
    /// filtering by `album_hash` still sees a consistent summary.
    fn finish(&mut self, album: &Album, phase: IndexPhase, error: Option<String>) {
        self.album_files_done = self.files_done;
        self.album_files_total = self.files_total;
        self.album_dirs_done = self.dirs_done;
        self.album_dirs_total = self.dirs_total;
        self.current = None;
        self.publish(album, phase, true, error);
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

    // Per-album counters start over with every folder, so a client viewing a
    // descendant of a nested job sees that album's own bar, not the job total.
    progress.album_files_done = 0;
    progress.album_files_total = 0;
    progress.album_dirs_done = 0;
    progress.album_dirs_total = 0;
    progress.album_changed = false;

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
    progress.album_dirs_done += dirs.len() as u64;
    progress.album_dirs_total = progress.album_dirs_done;
    progress.album_files_total = media.len() as u64;

    if count_totals {
        progress.files_total += media.len() as u64;
        progress.dirs_total += dirs.len() as u64;
    }
    progress.publish(album, IndexPhase::Scanning, false, None);

    // Totals are known; from here on the bar is determinate and each event
    // carries the file being hashed, which is what makes slow videos visible.
    progress.current = media.first().map(|entry| entry.name.clone());
    progress.publish(album, IndexPhase::Indexing, false, None);

    // Snapshot of the current rows: used to skip hashing unchanged files, to
    // resolve duplicates and to delete rows whose files disappeared. The album
    // is claimed exclusively, so a plain read is enough here.
    let existing: Vec<ExistingMedia> = sqlx::query_as(
        "SELECT id, name, hash, width, height, duration_sec, framerate, frame_count, size, date, \
         hash_version FROM media WHERE album_id = ?1",
    )
    .bind(album.id)
    .fetch_all(&state.db)
    .await
    .map_err(|err| step("select existing media", err))?;

    let existing_dups: Vec<ExistingDuplica> =
        sqlx::query_as("SELECT name, media_id, data FROM duplica WHERE album_id = ?1")
            .bind(album.id)
            .fetch_all(&state.db)
            .await
            .map_err(|err| step("select existing duplicates", err))?;

    let existing_by_name: HashMap<&str, &ExistingMedia> = existing
        .iter()
        .map(|media| (media.name.as_str(), media))
        .collect();
    let existing_dups_by_name: HashMap<&str, (i64, DuplicaData)> = existing_dups
        .iter()
        .filter_map(|dup| {
            let data = serde_json::from_str::<DuplicaData>(&dup.data).ok()?;
            Some((dup.name.as_str(), (dup.media_id, data)))
        })
        .collect();

    let existing_names: HashSet<&str> = existing.iter().map(|row| row.name.as_str()).collect();
    let existing_dup_names: HashSet<&str> =
        existing_dups.iter().map(|row| row.name.as_str()).collect();
    let observed: HashSet<&str> = media.iter().map(|entry| entry.name.as_str()).collect();

    // Scanning and writing are interleaved: every 100 files are committed in
    // their own transaction while the rest of the folder is still being read.
    // This keeps the write lock free between batches, makes progress visible
    // throughout the scan instead of in one burst at the end, and is safe on
    // failure because the next scan repairs any half-written state
    // (`indexed_at` is only bumped once everything, including deletions, has
    // landed).
    let mut batch: Vec<ScannedFile> = Vec::with_capacity(INSERT_BATCH);
    let mut batch_index = 0_usize;
    // Content hash -> media row id for every owner written so far. Further
    // files with the same hash become `duplica` rows pointing at it.
    let mut hash_owner: HashMap<String, i64> = HashMap::new();
    let mut seen_hashes: HashSet<String> = HashSet::new();

    for entry in &media {
        progress.current = Some(entry.name.clone());

        // Flush finished rows before announcing the next file, so the wall
        // picks up new media within one throttle window instead of waiting for
        // the whole folder (which matters for albums of a few large videos).
        if progress.should_publish() {
            if !batch.is_empty() {
                if write_scanned_batch(state, album, &batch, &mut hash_owner, batch_index).await? {
                    progress.mark_changed();
                }
                batch_index += 1;
                batch.clear();
            }
            progress.publish(album, IndexPhase::Indexing, false, None);
        }

        let metadata = match tokio::fs::metadata(&entry.path).await {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("ERR: skipping `{}`: {err}", entry.path.display());
                progress.file_done();
                continue;
            }
        };
        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let date = metadata.modified().ok().map(DateTime::<Utc>::from);

        // Reuse the known hash when the file looks untouched. The mtime must be
        // present: without it, two different files with the same size are
        // indistinguishable.
        let known = date.and_then(|date| {
            known_file(entry, &existing_by_name, &existing_dups_by_name, size, date)
        });

        let file = match known {
            Some(known) => ScannedFile {
                name: entry.name.clone(),
                hash: known.hash,
                media_type: entry.media_type,
                width: known.width,
                height: known.height,
                duration_sec: known.duration_sec,
                framerate: known.framerate,
                frame_count: known.frame_count,
                size,
                date,
                sort_name: natural_sort_key(&entry.name),
                known_row: Some(known.row),
            },
            None => {
                let probed = match probe_file(entry, &state.config.ffprobe_bin).await {
                    Ok(probed) => probed,
                    Err(err) => {
                        eprintln!("ERR: skipping `{}`: {err}", entry.path.display());
                        progress.file_done();
                        continue;
                    }
                };
                ScannedFile {
                    name: entry.name.clone(),
                    hash: probed.hash,
                    media_type: entry.media_type,
                    width: probed.metadata.width,
                    height: probed.metadata.height,
                    duration_sec: probed.metadata.duration_sec,
                    framerate: probed.metadata.framerate,
                    frame_count: probed.metadata.frame_count,
                    size,
                    date,
                    sort_name: natural_sort_key(&entry.name),
                    known_row: None,
                }
            }
        };

        // The first file with a given content becomes a `media` row; everything
        // else is a duplicate. Only count genuinely new names as new media.
        if seen_hashes.insert(file.hash.clone())
            && !existing_names.contains(entry.name.as_str())
            && !existing_dup_names.contains(entry.name.as_str())
        {
            progress.new_media += 1;
        }

        batch.push(file);
        progress.file_done();

        if batch.len() >= INSERT_BATCH {
            if write_scanned_batch(state, album, &batch, &mut hash_owner, batch_index).await? {
                progress.mark_changed();
            }
            batch_index += 1;
            batch.clear();
            progress.publish(album, IndexPhase::Indexing, false, None);
        }
    }

    if !batch.is_empty() {
        if write_scanned_batch(state, album, &batch, &mut hash_owner, batch_index).await? {
            progress.mark_changed();
        }
        progress.publish(album, IndexPhase::Indexing, false, None);
    }

    let (children, new_albums, deleted) = {
        let _write_guard = state.lock_writes().await;
        let mut tx = state
            .db
            .begin()
            .await
            .map_err(|err| step("begin metadata transaction", err))?;
        let mut deleted = false;

        for row in &existing {
            if !observed.contains(row.name.as_str()) {
                let removed = sqlx::query("DELETE FROM media WHERE id = ?1")
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| step(&format!("delete `{}`", row.name), err))?;
                deleted |= removed.rows_affected() > 0;
            }
        }

        // Copies whose files disappeared. Removing their owner above already
        // cascades the rows that referenced it.
        for row in &existing_dups {
            if !observed.contains(row.name.as_str()) {
                let removed = sqlx::query("DELETE FROM duplica WHERE album_id = ?1 AND name = ?2")
                    .bind(album.id)
                    .bind(&row.name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| step(&format!("delete duplicate `{}`", row.name), err))?;
                deleted |= removed.rows_affected() > 0;
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

        (children, new_albums, deleted)
    };

    if new_albums > 0 || deleted {
        progress.mark_changed();
    }
    progress.new_albums += new_albums;
    progress.current = None;
    progress.publish(album, IndexPhase::Indexing, false, None);

    Ok(AlbumSyncStats { children })
}

/// Writes one scanned batch, deciding which files become `media` rows and
/// which become `duplica` rows.
///
/// The first file with a given content hash becomes the owner; every later file
/// with the same hash points at it. Files that are already stored exactly as
/// they were found are not written at all, so re-indexing an unchanged album
/// does not touch the database rows.
async fn write_scanned_batch(
    state: &AppState,
    album: &Album,
    batch: &[ScannedFile],
    hash_owner: &mut HashMap<String, i64>,
    batch_index: usize,
) -> IndexResult<bool> {
    let write_guard = state.lock_writes().await;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|err| step("begin media batch", err))?;
    let mut written = false;

    for file in batch {
        let owner = hash_owner.get(&file.hash).copied();

        match owner {
            // A later copy of content that is already owned in this album.
            Some(owner_id) => {
                let already_stored =
                    matches!(file.known_row, Some(KnownRow::Duplica(id)) if id == owner_id);
                if already_stored {
                    continue;
                }

                // The name may currently belong to a media row (for example the
                // file used to hold different content); it becomes a copy now.
                sqlx::query("DELETE FROM media WHERE album_id = ?1 AND name = ?2")
                    .bind(album.id)
                    .bind(&file.name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| step(&format!("delete stale media `{}`", file.name), err))?;

                let data = serde_json::to_string(&DuplicaData::from(file))
                    .map_err(|err| step(&format!("serialize duplicate `{}`", file.name), err))?;
                sqlx::query(UPSERT_DUPLICA)
                    .bind(&file.name)
                    .bind(owner_id)
                    .bind(album.id)
                    .bind(data)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| {
                        step(
                            &format!(
                                "upsert duplicate `{}` (batch {})",
                                file.name,
                                batch_index + 1
                            ),
                            err,
                        )
                    })?;
                written = true;
            }
            // The row already holds exactly this content; just remember its id.
            None => {
                if let Some(KnownRow::Media(id)) = file.known_row {
                    hash_owner.insert(file.hash.clone(), id);
                    continue;
                }

                // First file with this content in the album: it owns the hash.
                sqlx::query("DELETE FROM duplica WHERE album_id = ?1 AND name = ?2")
                    .bind(album.id)
                    .bind(&file.name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| step(&format!("delete duplicate `{}`", file.name), err))?;
                // Another row may already hold this content under a different
                // name (for example the previous owner disappeared); only one
                // `media` row per hash is allowed.
                sqlx::query("DELETE FROM media WHERE album_id = ?1 AND hash = ?2 AND name <> ?3")
                    .bind(album.id)
                    .bind(&file.hash)
                    .bind(&file.name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| {
                        step(&format!("delete duplicate owner of `{}`", file.name), err)
                    })?;
                sqlx::query("DELETE FROM media WHERE album_id = ?1 AND name = ?2 AND hash <> ?3")
                    .bind(album.id)
                    .bind(&file.name)
                    .bind(&file.hash)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| step(&format!("delete old content of `{}`", file.name), err))?;

                let id: i64 = sqlx::query_scalar(UPSERT_MEDIA)
                    .bind(&file.name)
                    .bind(&file.hash)
                    .bind(file.media_type)
                    .bind(album.id)
                    .bind(file.width)
                    .bind(file.height)
                    .bind(file.duration_sec)
                    .bind(file.framerate)
                    .bind(file.frame_count)
                    .bind(file.size)
                    .bind(file.date)
                    .bind(&file.sort_name)
                    .bind(MEDIA_HASH_VERSION)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|err| {
                        step(
                            &format!("upsert `{}` (batch {})", file.name, batch_index + 1),
                            err,
                        )
                    })?;
                hash_owner.insert(file.hash.clone(), id);
                written = true;
            }
        }
    }

    tx.commit()
        .await
        .map_err(|err| step(&format!("commit media batch {}", batch_index + 1), err))?;
    drop(write_guard);
    Ok(written)
}

async fn sync_tree(state: &AppState, album: &Album, progress: &mut Progress) -> IndexResult<()> {
    let Some(dir) = state.album_dir(album) else {
        return Err(format!("album `{}` has an unsafe path", album.hash).into());
    };

    let canonical = tokio::fs::canonicalize(&dir).await?;
    let (files_total, dirs_total) = count_tree(&dir, album, progress).await?;
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
                eprintln!("ERR: skipping child `{}`: {err}", child_dir.display());
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
/// Counts are published while the tree is walked, so the panel shows how many
/// items were discovered already instead of an empty bar.
async fn count_tree(
    dir: &Path,
    album: &Album,
    progress: &mut Progress,
) -> std::io::Result<(u64, u64)> {
    let mut stack = vec![dir.to_path_buf()];
    let mut visited = HashSet::new();
    visited.insert(tokio::fs::canonicalize(dir).await?);
    let mut files = 0_u64;
    let mut dirs = 0_u64;

    while let Some(current_dir) = stack.pop() {
        progress.current = current_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        progress.publish_throttled(album, IndexPhase::Scanning);

        let mut read_dir = tokio::fs::read_dir(&current_dir).await?;

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
                    eprintln!("ERR: skipping `{}`: {err}", path.display());
                    continue;
                }
            };

            if metadata.is_dir() {
                let canonical = match tokio::fs::canonicalize(&path).await {
                    Ok(path) => path,
                    Err(err) => {
                        eprintln!("ERR: skipping `{}`: {err}", path.display());
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

            // Live discovery counters: `files_total`/`dirs_total` are not final
            // yet, but the panel can already show the growing totals.
            progress.files_total = files;
            progress.dirs_total = dirs;
            progress.publish_throttled(album, IndexPhase::Scanning);
        }
    }

    progress.files_total = files;
    progress.dirs_total = dirs;
    Ok((files, dirs))
}

/// A `media` row loaded before scanning, used to skip hashing and to keep row
/// ids stable for files whose contents did not change.
#[derive(Debug, sqlx::FromRow)]
struct ExistingMedia {
    id: i64,
    name: String,
    hash: String,
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
    framerate: Option<f64>,
    frame_count: Option<i64>,
    size: i64,
    date: Option<DateTime<Utc>>,
    hash_version: i64,
}

/// A `duplica` row loaded before scanning. `media_id` is the owner's row id.
#[derive(Debug, sqlx::FromRow)]
struct ExistingDuplica {
    name: String,
    media_id: i64,
    data: String,
}

/// Serialized into `duplica.data`.
///
/// Keeps enough information to skip re-hashing an unchanged copy and to promote
/// it to a `media` row without re-reading the file (width/height included).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DuplicaData {
    hash: String,
    size: i64,
    date: Option<DateTime<Utc>>,
    #[serde(default)]
    width: i64,
    #[serde(default)]
    height: i64,
    #[serde(default)]
    duration_sec: Option<f64>,
    #[serde(default)]
    framerate: Option<f64>,
    #[serde(default)]
    frame_count: Option<i64>,
    /// Hash input version, see [`MEDIA_HASH_VERSION`].
    #[serde(default)]
    version: i64,
}

impl From<&ScannedFile> for DuplicaData {
    fn from(file: &ScannedFile) -> Self {
        Self {
            hash: file.hash.clone(),
            size: file.size,
            date: file.date,
            width: file.width,
            height: file.height,
            duration_sec: file.duration_sec,
            framerate: file.framerate,
            frame_count: file.frame_count,
            version: MEDIA_HASH_VERSION,
        }
    }
}

/// Where a file was found before the scan: a `media` row or a `duplica` row.
#[derive(Debug, Clone, Copy)]
enum KnownRow {
    Media(i64),
    Duplica(i64),
}

/// Result of matching a file against its stored row by name, size and mtime.
struct KnownFile {
    hash: String,
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
    framerate: Option<f64>,
    frame_count: Option<i64>,
    row: KnownRow,
}

/// Reuses the stored hash when the file has the same name, size and mtime.
///
/// The check is deliberately conservative: a missing mtime or a different hash
/// algorithm version forces a real read, so changed content is never missed.
fn known_file(
    entry: &MediaEntry,
    existing_by_name: &HashMap<&str, &ExistingMedia>,
    existing_dups_by_name: &HashMap<&str, (i64, DuplicaData)>,
    size: i64,
    date: DateTime<Utc>,
) -> Option<KnownFile> {
    if let Some(media) = existing_by_name.get(entry.name.as_str()) {
        if media.hash_version == MEDIA_HASH_VERSION
            && media.size == size
            && same_mtime(media.date, date)
            && !needs_metadata_backfill(
                entry.media_type,
                media.width,
                media.height,
                media.duration_sec,
                media.framerate,
            )
        {
            return Some(KnownFile {
                hash: media.hash.clone(),
                width: media.width,
                height: media.height,
                duration_sec: media.duration_sec,
                framerate: media.framerate,
                frame_count: media.frame_count,
                row: KnownRow::Media(media.id),
            });
        }
        return None;
    }

    let (media_id, dup) = existing_dups_by_name.get(entry.name.as_str())?;
    if dup.version == MEDIA_HASH_VERSION
        && dup.size == size
        && same_mtime(dup.date, date)
        && !needs_metadata_backfill(
            entry.media_type,
            dup.width,
            dup.height,
            dup.duration_sec,
            dup.framerate,
        )
    {
        return Some(KnownFile {
            hash: dup.hash.clone(),
            width: dup.width,
            height: dup.height,
            duration_sec: dup.duration_sec,
            framerate: dup.framerate,
            frame_count: dup.frame_count,
            row: KnownRow::Duplica(*media_id),
        });
    }
    None
}

/// Rows written before video probing existed have video/animation dimensions
/// and timing left empty. Such a file is re-read once so the new columns are
/// filled in by the next scan instead of staying NULL forever.
fn needs_metadata_backfill(
    media_type: MediaType,
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
    framerate: Option<f64>,
) -> bool {
    matches!(media_type, MediaType::Video | MediaType::Animated)
        && (width <= 0 || height <= 0 || duration_sec.is_none() || framerate.is_none())
}

/// Compares filesystem timestamps at millisecond precision, the precision
/// SQLite reliably round-trips.
fn same_mtime(stored: Option<DateTime<Utc>>, actual: DateTime<Utc>) -> bool {
    stored.is_some_and(|stored| stored.timestamp_millis() == actual.timestamp_millis())
}

struct ScannedFile {
    name: String,
    hash: String,
    media_type: MediaType,
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
    framerate: Option<f64>,
    frame_count: Option<i64>,
    size: i64,
    date: Option<DateTime<Utc>>,
    sort_name: String,
    /// Row this file was recognised as before the scan, if its content did not
    /// have to be read again.
    known_row: Option<KnownRow>,
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
                "ERR: skipping non-UTF-8 file name in `{}`",
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
                eprintln!("ERR: skipping `{}`: {err}", path.display());
                continue;
            }
        };

        // `file_type` does not follow symlinks, unlike `metadata`; a symlinked
        // album directory or media file is still indexed normally.
        let followed = if file_type.is_symlink() {
            match tokio::fs::metadata(&path).await {
                Ok(metadata) => Some(metadata),
                Err(err) => {
                    eprintln!("ERR: skipping `{}`: {err}", path.display());
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

/// Metadata read from a file during indexing.
#[derive(Debug, Clone, Copy, Default)]
struct ProbedMetadata {
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
    framerate: Option<f64>,
    frame_count: Option<i64>,
}

/// A file whose metadata was read and whose content was hashed.
struct ProbedFile {
    hash: String,
    metadata: ProbedMetadata,
}

/// Reads media metadata and hashes the file in one blocking pass.
///
/// Still images and vectors use a lightweight header parser; videos and
/// animations go through `ffprobe`, with an image-header fallback for animated
/// files when FFmpeg is unavailable. A malformed or unsupported file is not an
/// indexing error, it simply has no metadata.
async fn probe_file(entry: &MediaEntry, ffprobe: &Path) -> std::io::Result<ProbedFile> {
    let path = entry.path.clone();
    let media_type = entry.media_type;
    let ffprobe = ffprobe.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let metadata = probe_metadata(&path, media_type, &ffprobe);
        let hash = hashes::media_hash(&path)?;
        Ok(ProbedFile { hash, metadata })
    })
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))?
}

fn probe_metadata(path: &Path, media_type: MediaType, ffprobe: &Path) -> ProbedMetadata {
    // FFprobe understands every video container the indexer accepts, plus GIF
    // and APNG. It also reports duration and frame rate, which header parsers
    // cannot provide.
    if matches!(media_type, MediaType::Video | MediaType::Animated)
        && let Some(meta) = crate::helpers::video::probe(ffprobe, path)
    {
        let mut metadata = ProbedMetadata {
            width: meta.width,
            height: meta.height,
            duration_sec: meta.duration_sec,
            framerate: meta.framerate,
            frame_count: meta.frame_count,
        };

        // Some containers omit the stream dimensions; the image header still
        // knows them for animations (and costs nothing for videos that do not).
        if (metadata.width <= 0 || metadata.height <= 0)
            && let Some((width, height)) = image_dimensions(path)
        {
            metadata.width = width;
            metadata.height = height;
        }

        return metadata;
    }

    if !matches!(
        media_type,
        MediaType::Image | MediaType::Animated | MediaType::Vector
    ) {
        return ProbedMetadata::default();
    }

    let dimensions = if media_type == MediaType::Vector {
        vips_dimensions(path)
    } else {
        image_dimensions(path)
    };

    match dimensions {
        Some((width, height)) => ProbedMetadata {
            width,
            height,
            ..ProbedMetadata::default()
        },
        None => ProbedMetadata::default(),
    }
}

fn image_dimensions(path: &Path) -> Option<(i64, i64)> {
    imagesize::size(path).ok().map(|size| {
        (
            i64::try_from(size.width).unwrap_or(i64::MAX),
            i64::try_from(size.height).unwrap_or(i64::MAX),
        )
    })
}

/// Vector dimensions through libvips. SVG is rendered lazily, so this only
/// parses the document and reports its intrinsic size.
fn vips_dimensions(path: &Path) -> Option<(i64, i64)> {
    crate::config::vips::ensure().ok()?;
    let image = vips::VipsImage::from_file(path).ok()?;
    Some((i64::from(image.width()), i64::from(image.height())))
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
