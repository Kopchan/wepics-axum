//! Shared response types and query parsing for the `/v1` API.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};

use crate::error::AppError;
use crate::models::{Album, Media, MediaType};
use crate::state::AppState;

/// Columns selected for a media row. Kept in one place so every v1 query reads
/// the same shape as [`Media`].
pub const MEDIA_SELECT: &str = "SELECT id, name, hash, \"type\", album_id, width, height, \
     duration_sec, date, size, sort_name, created_at, updated_at FROM media";

/// Single media row by album and media hash.
pub const MEDIA_BY_HASH: &str = "SELECT id, name, hash, \"type\", album_id, width, height, \
     duration_sec, date, size, sort_name, created_at, updated_at FROM media \
     WHERE album_id = ?1 AND hash = ?2";

/// `GET /api/v1/albums/{hash}` parameters.
#[derive(Debug, Deserialize)]
pub struct AlbumQuery {
    /// Media sort array, e.g. `date,size,-name`.
    pub msort: Option<String>,
    /// Child-album sort array, e.g. `level,-date,name`.
    pub asort: Option<String>,
    /// Preview count for the requested album.
    pub mlimit: Option<u32>,
    /// Preview count for every child and grandchild album.
    pub mlimitnest: Option<u32>,
    /// `0` current album, `1` children, `2` grandchildren.
    pub layers: Option<u8>,
    /// Media type filter, `img,vid,aud,anim`.
    pub mtype: Option<String>,
}

/// `GET /api/v1/album/{hash}/media` parameters.
#[derive(Debug, Deserialize)]
pub struct MediaListQuery {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    /// Media sort array, e.g. `date,size,-name`.
    pub sort: Option<String>,
    /// Media type filter, `img,vid,aud,anim`.
    #[serde(rename = "type")]
    pub media_type: Option<String>,
}

/// `?hash=<token>&exp=<unix timestamp>` on `/thumb/v1` and `/file/v1`.
#[derive(Debug, Deserialize)]
pub struct LinkQuery {
    pub hash: Option<String>,
    pub exp: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaSortField {
    Name,
    Date,
    Random,
    Size,
    Width,
    Height,
    Ratio,
    Square,
    Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct MediaSort {
    pub field: MediaSortField,
    pub descending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumSortField {
    Name,
    Content,
    Random,
    Size,
    Duration,
    Albums,
    Medias,
    Images,
    Videos,
    Audios,
    Created,
    Indexed,
    /// Placeholder for Laravel's `order_level`; the schema has no such column,
    /// so this key only documents intent and never changes the order.
    Level,
}

#[derive(Debug, Clone, Copy)]
pub struct AlbumSort {
    pub field: AlbumSortField,
    pub descending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlbumStats {
    pub medias_count: i64,
    pub images_count: i64,
    pub videos_count: i64,
    pub audios_count: i64,
    pub albums_count: i64,
    pub size: i64,
    pub duration: f64,
}

/// An album as returned by `/api/v1/albums/{hash}`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumItem {
    pub name: String,
    pub hash: String,
    pub indexed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub medias_count: i64,
    pub images_count: i64,
    pub videos_count: i64,
    pub audios_count: i64,
    pub albums_count: i64,
    pub size: i64,
    pub duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<MediaItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<AlbumItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ancestors: Option<Vec<AlbumItem>>,
}

/// A media row together with its short-lived link signature.
///
/// `media_hash` is the media identity used in the path, while `hash`/`exp` are
/// the secure-link query parameters. Keeping them apart avoids overloading the
/// old Laravel `hash` field with two different values.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub name: String,
    #[serde(rename = "type")]
    pub media_type: &'static str,
    pub media_hash: String,
    pub hash: String,
    pub exp: i64,
    pub date: Option<DateTime<Utc>>,
    pub size: i64,
    pub width: i64,
    pub height: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    pub album: MediaAlbum,
}

#[derive(Debug, Serialize)]
pub struct MediaAlbum {
    pub hash: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct MediaPage {
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
    pub pages: i64,
    pub pictures: Vec<MediaItem>,
}

impl AlbumItem {
    pub fn new(album: &Album, stats: AlbumStats) -> Self {
        Self {
            name: album.name.clone(),
            hash: album.hash.clone(),
            indexed_at: album.indexed_at,
            created_at: album.created_at,
            medias_count: stats.medias_count,
            images_count: stats.images_count,
            videos_count: stats.videos_count,
            audios_count: stats.audios_count,
            albums_count: stats.albums_count,
            size: stats.size,
            duration: stats.duration,
            sync: None,
            images: None,
            children: None,
            ancestors: None,
        }
    }
}

impl MediaItem {
    pub fn new(state: &AppState, album: &Album, media: &Media, exp: i64) -> Self {
        Self {
            name: media.name.clone(),
            media_type: media_type_name(media.r#type),
            media_hash: media.hash.clone(),
            hash: state.sign_media(&album.hash, &media.hash, exp),
            exp,
            date: media.date,
            size: media.size,
            width: media.width,
            height: media.height,
            duration: media.duration_sec,
            album: MediaAlbum {
                hash: album.hash.clone(),
                name: album.name.clone(),
            },
        }
    }
}

/// Laravel's media type vocabulary, which the client already understands.
pub fn media_type_name(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Animated => "imageAnimated",
    }
}

/// Database spelling of a media type.
pub fn media_type_db(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Animated => "animated",
    }
}

/// Parses `mtype=img,vid` / `type=img,anim` into concrete types.
pub fn parse_media_types(raw: Option<&str>) -> Result<Vec<MediaType>, AppError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };

    let mut types = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let media_type = match value.to_ascii_lowercase().as_str() {
            "img" | "image" => MediaType::Image,
            "vid" | "video" => MediaType::Video,
            "aud" | "audio" => MediaType::Audio,
            "anim" | "animated" | "imageanimated" => MediaType::Animated,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid media type `{other}`, expected img, vid, aud or anim"
                )));
            }
        };
        if !types.contains(&media_type) {
            types.push(media_type);
        }
    }

    Ok(types)
}

/// Parses a comma-separated media sort array. A `-` prefix means descending.
pub fn parse_media_sort(raw: Option<&str>) -> Result<Vec<MediaSort>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(vec![MediaSort {
            field: MediaSortField::Name,
            descending: false,
        }]);
    };

    let mut sorts = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (descending, value) = split_direction(value);
        let field = match value {
            "" | "name" => MediaSortField::Name,
            "date" => MediaSortField::Date,
            "random" => MediaSortField::Random,
            "size" => MediaSortField::Size,
            "width" => MediaSortField::Width,
            "height" => MediaSortField::Height,
            "ratio" => MediaSortField::Ratio,
            "square" => MediaSortField::Square,
            "duration" => MediaSortField::Duration,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid media sort `{other}`, expected name, date, random, size, width, \
                     height, ratio, square or duration"
                )));
            }
        };
        sorts.push(MediaSort { field, descending });
    }

    if sorts.is_empty() {
        sorts.push(MediaSort {
            field: MediaSortField::Name,
            descending: false,
        });
    }

    Ok(sorts)
}

/// Parses a comma-separated album sort array. A `-` prefix means descending.
pub fn parse_album_sort(raw: Option<&str>) -> Result<Vec<AlbumSort>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(vec![AlbumSort {
            field: AlbumSortField::Name,
            descending: false,
        }]);
    };

    let mut sorts = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (descending, value) = split_direction(value);
        let field = match value {
            "" | "name" => AlbumSortField::Name,
            "content" => AlbumSortField::Content,
            "random" => AlbumSortField::Random,
            "size" => AlbumSortField::Size,
            "duration" => AlbumSortField::Duration,
            "albums" => AlbumSortField::Albums,
            "medias" => AlbumSortField::Medias,
            "images" => AlbumSortField::Images,
            "videos" => AlbumSortField::Videos,
            "audios" => AlbumSortField::Audios,
            "created" => AlbumSortField::Created,
            "indexed" => AlbumSortField::Indexed,
            "level" => AlbumSortField::Level,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid album sort `{other}`, expected name, content, random, size, duration, \
                     albums, medias, images, videos, audios, created, indexed or level"
                )));
            }
        };
        sorts.push(AlbumSort { field, descending });
    }

    if sorts.is_empty() {
        sorts.push(AlbumSort {
            field: AlbumSortField::Name,
            descending: false,
        });
    }

    Ok(sorts)
}

fn split_direction(value: &str) -> (bool, &str) {
    match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    }
}

/// Validates a preview-count parameter. `0` disables previews.
pub fn parse_preview_count(value: Option<u32>, default: u32) -> Result<u32, AppError> {
    const MAX_PREVIEWS: u32 = 200;

    let value = value.unwrap_or(default);
    if value > MAX_PREVIEWS {
        return Err(AppError::BadRequest(format!(
            "preview count must be between 0 and {MAX_PREVIEWS}"
        )));
    }
    Ok(value)
}

/// Loads per-album counters with one round-trip.
pub async fn load_album_stats(pool: &SqlitePool, album_id: i64) -> Result<AlbumStats, AppError> {
    #[derive(FromRow)]
    struct Row {
        medias_count: i64,
        images_count: i64,
        videos_count: i64,
        audios_count: i64,
        albums_count: i64,
        size: i64,
        duration: f64,
    }

    let row: Row = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM media WHERE album_id = ?1) AS medias_count,
            (SELECT COUNT(*) FROM media WHERE album_id = ?1 AND \"type\" = 'image') AS images_count,
            (SELECT COUNT(*) FROM media WHERE album_id = ?1 AND \"type\" IN ('video', 'animated')) AS videos_count,
            (SELECT COUNT(*) FROM media WHERE album_id = ?1 AND \"type\" = 'audio') AS audios_count,
            (SELECT COUNT(*) FROM albums WHERE parent_id = ?1) AS albums_count,
            (SELECT COALESCE(SUM(size), 0) FROM media WHERE album_id = ?1) AS size,
            (SELECT COALESCE(SUM(duration_sec), 0.0) FROM media WHERE album_id = ?1) AS duration",
    )
    .bind(album_id)
    .fetch_one(pool)
    .await?;

    Ok(AlbumStats {
        medias_count: row.medias_count,
        images_count: row.images_count,
        videos_count: row.videos_count,
        audios_count: row.audios_count,
        albums_count: row.albums_count,
        size: row.size,
        duration: row.duration,
    })
}

/// Appends `AND "type" IN (...)` to a media query.
pub fn push_media_type_filter(builder: &mut QueryBuilder<Sqlite>, types: &[MediaType]) {
    if types.is_empty() {
        return;
    }

    builder.push(" AND \"type\" IN (");
    for (index, media_type) in types.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push_bind(media_type_db(*media_type));
    }
    builder.push(")");
}

/// Appends the `ORDER BY` clause for a media query.
pub fn push_media_order(builder: &mut QueryBuilder<Sqlite>, sorts: &[MediaSort]) {
    builder.push(" ORDER BY ");
    for (index, sort) in sorts.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        let direction = if sort.descending { " DESC" } else { " ASC" };
        match sort.field {
            MediaSortField::Name => {
                builder.push("sort_name").push(direction);
            }
            MediaSortField::Date => {
                // Undated files stay last regardless of direction.
                builder.push("date IS NULL ASC, date").push(direction);
            }
            MediaSortField::Random => {
                builder.push("RANDOM()");
            }
            MediaSortField::Size => {
                builder.push("size").push(direction);
            }
            MediaSortField::Width => {
                builder.push("width").push(direction);
            }
            MediaSortField::Height => {
                builder.push("height").push(direction);
            }
            MediaSortField::Ratio => {
                builder
                    .push("(CAST(width AS REAL) / NULLIF(height, 0))")
                    .push(direction);
            }
            MediaSortField::Square => {
                builder
                    .push(
                        "ABS((CAST(MAX(width, height) AS REAL) / NULLIF(MIN(width, height), 0)) - 1)",
                    )
                    .push(direction);
            }
            MediaSortField::Duration => {
                builder
                    .push("duration_sec IS NULL ASC, duration_sec")
                    .push(direction);
            }
        }
    }
    // Stable order for equal keys and pagination.
    builder.push(", sort_name ASC, id ASC");
}

/// Appends the `ORDER BY` clause for a child-album query. The query must use
/// the `albums` table without an alias.
pub fn push_album_order(builder: &mut QueryBuilder<Sqlite>, sorts: &[AlbumSort]) {
    builder.push(" ORDER BY ");
    for (index, sort) in sorts.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        let direction = if sort.descending { " DESC" } else { " ASC" };
        match sort.field {
            AlbumSortField::Name => {
                builder.push("sort_name").push(direction);
            }
            AlbumSortField::Content => {
                builder
                    .push(
                        "COALESCE((SELECT m.sort_name FROM media m WHERE m.album_id = albums.id \
                         ORDER BY m.sort_name LIMIT 1), albums.sort_name)",
                    )
                    .push(direction);
            }
            AlbumSortField::Random => {
                builder.push("RANDOM()");
            }
            AlbumSortField::Size => {
                builder
                    .push("(SELECT COALESCE(SUM(size), 0) FROM media WHERE album_id = albums.id)")
                    .push(direction);
            }
            AlbumSortField::Duration => {
                builder
                    .push(
                        "(SELECT COALESCE(SUM(duration_sec), 0.0) FROM media \
                         WHERE album_id = albums.id)",
                    )
                    .push(direction);
            }
            AlbumSortField::Albums => {
                builder
                    .push("(SELECT COUNT(*) FROM albums child WHERE child.parent_id = albums.id)")
                    .push(direction);
            }
            AlbumSortField::Medias => {
                builder
                    .push("(SELECT COUNT(*) FROM media WHERE album_id = albums.id)")
                    .push(direction);
            }
            AlbumSortField::Images => {
                builder
                    .push(
                        "(SELECT COUNT(*) FROM media WHERE album_id = albums.id AND \"type\" = 'image')",
                    )
                    .push(direction);
            }
            AlbumSortField::Videos => {
                builder
                    .push(
                        "(SELECT COUNT(*) FROM media WHERE album_id = albums.id \
                         AND \"type\" IN ('video', 'animated'))",
                    )
                    .push(direction);
            }
            AlbumSortField::Audios => {
                builder
                    .push(
                        "(SELECT COUNT(*) FROM media WHERE album_id = albums.id AND \"type\" = 'audio')",
                    )
                    .push(direction);
            }
            AlbumSortField::Created => {
                builder.push("created_at").push(direction);
            }
            AlbumSortField::Indexed => {
                builder
                    .push("indexed_at IS NULL ASC, indexed_at")
                    .push(direction);
            }
            AlbumSortField::Level => {
                // No `order_level` column in this schema; accept the key so
                // clients can express "don't reorder by level" explicitly.
                builder.push("0");
            }
        }
    }
    builder.push(", sort_name ASC, id ASC");
}
