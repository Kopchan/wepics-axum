//! Shared response types and query parsing for the `/v1` API.

use std::collections::HashMap;
use std::path::Path as FsPath;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};

use crate::error::AppError;
use crate::models::{Album, Media, MediaType};
use crate::state::AppState;

/// Columns selected for a media item together with the album it belongs to.
///
/// The album is joined in because nested requests return media from several
/// albums and every item must say which album it came from.
pub const MEDIA_COLUMNS: &str = "media.name, media.hash, media.\"type\", media.width, \
     media.height, media.duration_sec, media.date, media.size, media.rating_level, media.album_id, \
     albums.hash AS album_hash, albums.name AS album_name, albums.path AS album_path, \
     albums.alias AS album_alias, albums.rating_level AS album_rating_level";

/// FROM clause matching [`MEDIA_COLUMNS`].
pub const MEDIA_FROM: &str = " FROM media JOIN albums ON albums.id = media.album_id";

/// Qualified album columns, matching the [`Album`] fields. Used by queries that
/// join `albums` with `album_stats`.
pub const ALBUM_FIELDS_QUALIFIED: &str = "albums.id, albums.name, albums.hash, \
     albums.parent_id, albums.path, albums.alias, albums.old_aliases, albums.rating_level, \
     albums.order_level, albums.guest_allow, albums.indexed_at, albums.sort_name, \
     albums.created_at, albums.updated_at";

/// `GET /api/v1/album/{hash}` parameters.
#[derive(Debug, Deserialize)]
pub struct AlbumQuery {
    /// Media sort array, e.g. `date,size,-name`.
    pub msort: Option<String>,
    /// Child-album sort array, e.g. `order,-date,name`.
    pub asort: Option<String>,
    /// Media preview counts per layer, `0/30/4` or `*` for unlimited.
    pub mlimit: Option<String>,
    /// Child-album counts per layer, `10/10` or `*` for unlimited.
    pub alimit: Option<String>,
    /// Nested-media flags per layer, `-`/`+`.
    pub mnest: Option<String>,
    /// Media type filter, `img,vid,aud,ani,vec`.
    pub mtype: Option<String>,
    /// Accepted for compatibility; tag filtering is not implemented yet.
    #[serde(rename = "mtag")]
    #[allow(dead_code)]
    pub mtag: Option<String>,
    /// Accepted for compatibility; rating filtering is not implemented yet.
    #[serde(rename = "mrating")]
    #[allow(dead_code)]
    pub mrating: Option<String>,
    /// Accepted for compatibility; meta filtering is not implemented yet.
    #[serde(rename = "mfilter")]
    #[allow(dead_code)]
    pub mfilter: Option<String>,
    /// Bare flag: include the ancestor chain.
    pub ancests: Option<String>,
    /// Page of direct children, used by `/childs`.
    pub page: Option<u32>,
    /// Page size of direct children, used by `/childs`; `*` means unlimited.
    pub limit: Option<String>,
}

/// `GET /api/v1/album/{hash}/media` parameters.
#[derive(Debug, Deserialize)]
pub struct MediaListQuery {
    pub page: Option<u32>,
    /// Page size, `*` for unlimited.
    pub limit: Option<String>,
    /// Media sort array, e.g. `date,size,-name`.
    pub sort: Option<String>,
    /// Media type filter, `img,vid,aud,ani,vec`.
    #[serde(rename = "type")]
    pub media_type: Option<String>,
    /// Include media of all descendant albums.
    pub nest: Option<String>,
    /// Accepted for compatibility; tag filtering is not implemented yet.
    #[allow(dead_code)]
    pub tag: Option<String>,
    /// Accepted for compatibility; rating filtering is not implemented yet.
    #[allow(dead_code)]
    pub rating: Option<String>,
    /// Accepted for compatibility; meta filtering is not implemented yet.
    #[allow(dead_code)]
    pub filter: Option<String>,
}

/// `?sign=<token>&exp=<unix timestamp>` on `/thumb/v1` and `/file/v1`.
#[derive(Debug, Deserialize)]
pub struct LinkQuery {
    pub sign: Option<String>,
    pub exp: Option<i64>,
}

/// A numeric limit or `*` for "everything".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Fixed(u32),
    All,
}

/// Largest value accepted for a per-layer preview count.
const MAX_PREVIEWS: u32 = 200;

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
    Reacts,
    Duration,
    Bitrate,
    Framerate,
    Order,
}

#[derive(Debug, Clone, Copy)]
pub struct MediaSort {
    pub field: MediaSortField,
    pub descending: bool,
    /// Seed for deterministic `random` sorting.
    pub seed: Option<i64>,
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
    Order,
}

#[derive(Debug, Clone, Copy)]
pub struct AlbumSort {
    pub field: AlbumSortField,
    pub descending: bool,
    /// Seed for deterministic `random` sorting.
    pub seed: Option<i64>,
}

/// Counters for one album. Direct counters cover the album itself, nested ones
/// cover the whole subtree below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumStats {
    pub medias_count: i64,
    pub images_count: i64,
    pub videos_count: i64,
    pub audios_count: i64,
    pub albums_count: i64,
    pub size: i64,
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct AlbumStatsPair {
    pub direct: AlbumStats,
    pub nested: AlbumStats,
}

/// An album as returned by `/api/v1/album/{hash}`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumItem {
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ancestors: Option<Vec<AlbumItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<AlbumItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_aliases: Option<Vec<String>>,
    pub stats: AlbumStatsPair,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<Vec<MediaItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_allow: Option<bool>,
    /// Present on album-info responses only: `started`, `running` or nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<String>,
}

/// The album a media item belongs to. This is a lightweight subset of
/// [`AlbumItem`], which is what the client's `MediaItem.album` expects.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAlbum {
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_level: Option<i64>,
}

/// A media row together with its short-lived link signature.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    /// Stable media identity, used in `/thumb` and `/file` paths.
    pub hash: String,
    /// Short-lived signature for the link query.
    pub sign: String,
    pub exp: i64,
    /// File stem without the extension.
    pub name: String,
    /// Extension without the leading dot.
    pub ext: String,
    #[serde(rename = "type")]
    pub media_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    pub width: i64,
    pub height: i64,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_level: Option<i64>,
    /// Present only when the media comes from another album (nested
    /// selections); the client needs it to build the viewer link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<MediaAlbum>,
    /// Reaction counters keyed by reaction code; empty until reactions exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reactions: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Bits per second, derived from `size` and `duration` when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framerate: Option<f64>,
}

/// `{ total, items }` — the shape every v1 list endpoint returns.
#[derive(Debug, Serialize)]
pub struct PaginateRes<T> {
    pub total: i64,
    pub items: Vec<T>,
}

impl AlbumItem {
    pub fn new(album: &Album, stats: AlbumStatsPair) -> Self {
        Self {
            hash: album.hash.clone(),
            alias: album.alias.clone(),
            name: album.name.clone(),
            path: (!album.path.is_empty()).then(|| album.path.clone()),
            rating_level: album.rating_level,
            ancestors: None,
            children: None,
            old_aliases: parse_old_aliases(&album.old_aliases),
            stats,
            order_level: Some(album.order_level),
            indexed_at: album.indexed_at,
            created_at: Some(album.created_at),
            content_sort: None,
            media: None,
            guest_allow: album.guest_allow,
            sync: None,
        }
    }
}

/// Parses the JSON array stored in `albums.old_aliases`; bad data is ignored so
/// one broken row cannot make album responses fail.
fn parse_old_aliases(raw: &str) -> Option<Vec<String>> {
    let aliases: Vec<String> = serde_json::from_str(raw).unwrap_or_default();
    (!aliases.is_empty()).then_some(aliases)
}

/// Owned parts of a media row that are turned into a [`MediaItem`].
struct MediaParts<'a> {
    album: MediaAlbum,
    /// Whether `album` is serialized. It is omitted when the media belongs to
    /// the album being viewed, because the URL already carries that hash.
    include_album: bool,
    name: &'a str,
    hash: &'a str,
    media_type: MediaType,
    width: i64,
    height: i64,
    duration: Option<f64>,
    date: Option<DateTime<Utc>>,
    size: i64,
    rating_level: Option<i64>,
}

impl MediaItem {
    /// Single-media response: the album is implied by the request path.
    pub fn new(state: &AppState, album: &Album, media: &Media, exp: i64) -> Self {
        Self::from_parts(
            state,
            exp,
            MediaParts {
                album: media_album(album),
                include_album: false,
                name: &media.name,
                hash: &media.hash,
                media_type: media.r#type,
                width: media.width,
                height: media.height,
                duration: media.duration_sec,
                date: media.date,
                size: media.size,
                rating_level: media.rating_level,
            },
        )
    }

    /// Builds an item from a joined row. `context_album_id` is the album in the
    /// request path: media that belong to it are serialized without `album`.
    fn from_row(state: &AppState, row: MediaRow, exp: i64, context_album_id: i64) -> Self {
        let include_album = row.album_id != context_album_id;
        Self::from_parts(
            state,
            exp,
            MediaParts {
                album: MediaAlbum {
                    hash: row.album_hash,
                    alias: row.album_alias,
                    name: row.album_name,
                    path: (!row.album_path.is_empty()).then_some(row.album_path),
                    rating_level: row.album_rating_level,
                },
                include_album,
                name: &row.name,
                hash: &row.hash,
                media_type: row.media_type,
                width: row.width,
                height: row.height,
                duration: row.duration_sec,
                date: row.date,
                size: row.size,
                rating_level: row.rating_level,
            },
        )
    }

    fn from_parts(state: &AppState, exp: i64, parts: MediaParts<'_>) -> Self {
        let (name, ext) = split_file_name(parts.name);
        let bitrate = parts
            .duration
            .filter(|duration| *duration > 0.0)
            .map(|duration| parts.size as f64 * 8.0 / duration);

        Self {
            hash: parts.hash.to_string(),
            sign: state.sign_media(&parts.album.hash, parts.hash, exp),
            exp,
            name,
            ext,
            media_type: media_type_name(parts.media_type),
            ratio: (parts.height > 0).then(|| parts.width as f64 / parts.height as f64),
            width: parts.width,
            height: parts.height,
            size: parts.size,
            date: parts.date,
            rating_level: parts.rating_level,
            album: parts.include_album.then_some(parts.album),
            reactions: None,
            duration: parts.duration,
            bitrate,
            framerate: None,
        }
    }
}

fn media_album(album: &Album) -> MediaAlbum {
    MediaAlbum {
        hash: album.hash.clone(),
        alias: album.alias.clone(),
        name: album.name.clone(),
        path: (!album.path.is_empty()).then(|| album.path.clone()),
        rating_level: album.rating_level,
    }
}

/// Splits `photo.final.jpg` into `photo.final` and `jpg`.
fn split_file_name(file_name: &str) -> (String, String) {
    let path = FsPath::new(file_name);
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
        .to_string();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_string();

    (name, ext)
}

/// Client-facing name of a media kind.
pub fn media_type_name(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Animated => "animated",
        MediaType::Vector => "vector",
    }
}

/// Database spelling of a media type.
pub fn media_type_db(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Animated => "animated",
        MediaType::Vector => "vector",
    }
}

/// Parses `mtype=img,vid` / `type=ani` into concrete types.
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
            "ani" | "anim" | "animated" | "imageanimated" => MediaType::Animated,
            "vec" | "vector" | "svg" | "imagevector" => MediaType::Vector,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid media type `{other}`, expected img, vid, aud, ani or vec"
                )));
            }
        };
        if !types.contains(&media_type) {
            types.push(media_type);
        }
    }

    Ok(types)
}

/// Parses a comma-separated media sort array. A `-` prefix means descending,
/// `random:<seed>` makes the shuffle deterministic.
pub fn parse_media_sort(raw: Option<&str>) -> Result<Vec<MediaSort>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(vec![MediaSort {
            field: MediaSortField::Name,
            descending: false,
            seed: None,
        }]);
    };

    let mut sorts = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (descending, value) = split_direction(value);
        let (name, special) = split_special(value);
        let field = match name.to_ascii_lowercase().as_str() {
            "" | "name" => MediaSortField::Name,
            "date" => MediaSortField::Date,
            "random" => MediaSortField::Random,
            "size" => MediaSortField::Size,
            "width" => MediaSortField::Width,
            "height" => MediaSortField::Height,
            "ratio" => MediaSortField::Ratio,
            "square" => MediaSortField::Square,
            "reacts" => MediaSortField::Reacts,
            "duration" => MediaSortField::Duration,
            "bitrate" => MediaSortField::Bitrate,
            "framerate" => MediaSortField::Framerate,
            "order" => MediaSortField::Order,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid media sort `{other}`, expected name, date, random, size, width, \
                     height, ratio, square, reacts, duration, bitrate, framerate or order"
                )));
            }
        };
        let seed = if field == MediaSortField::Random {
            parse_seed(special)?
        } else {
            None
        };
        sorts.push(MediaSort {
            field,
            descending,
            seed,
        });
    }

    if sorts.is_empty() {
        sorts.push(MediaSort {
            field: MediaSortField::Name,
            descending: false,
            seed: None,
        });
    }

    Ok(sorts)
}

/// Parses a comma-separated album sort array. A `-` prefix means descending,
/// `random:<seed>` makes the shuffle deterministic.
pub fn parse_album_sort(raw: Option<&str>) -> Result<Vec<AlbumSort>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(vec![AlbumSort {
            field: AlbumSortField::Name,
            descending: false,
            seed: None,
        }]);
    };

    let mut sorts = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (descending, value) = split_direction(value);
        let (name, special) = split_special(value);
        let field = match name.to_ascii_lowercase().as_str() {
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
            "order" | "level" => AlbumSortField::Order,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid album sort `{other}`, expected name, content, random, size, duration, \
                     albums, medias, images, videos, audios, created, indexed or order"
                )));
            }
        };
        let seed = if field == AlbumSortField::Random {
            parse_seed(special)?
        } else {
            None
        };
        sorts.push(AlbumSort {
            field,
            descending,
            seed,
        });
    }

    if sorts.is_empty() {
        sorts.push(AlbumSort {
            field: AlbumSortField::Name,
            descending: false,
            seed: None,
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

fn split_special(value: &str) -> (&str, Option<&str>) {
    match value.split_once(':') {
        Some((name, special)) => (name, Some(special)),
        None => (value, None),
    }
}

fn parse_seed(special: Option<&str>) -> Result<Option<i64>, AppError> {
    match special {
        Some(seed) if !seed.is_empty() => seed.parse::<i64>().map(Some).map_err(|_| {
            AppError::BadRequest(format!("invalid random seed `{seed}`, expected an integer"))
        }),
        _ => Ok(None),
    }
}

/// Parses a `limit`/`mlimit` value: a non-negative number or `*`.
pub fn parse_limit(raw: Option<&str>, default: Limit) -> Result<Limit, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(default);
    };

    if raw == "*" {
        return Ok(Limit::All);
    }

    let value = raw.parse::<u32>().map_err(|_| {
        AppError::BadRequest(format!("invalid limit `{raw}`, expected a number or `*`"))
    })?;
    Ok(Limit::Fixed(value))
}

/// Parses a slash-separated per-layer limit list: `0/30/4`, `*/*`.
pub fn parse_layer_limits(raw: Option<&str>, defaults: &[Limit]) -> Result<Vec<Limit>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(defaults.to_vec());
    };

    let mut limits = Vec::new();
    for part in raw.split(['/', ',']).map(str::trim) {
        if part.is_empty() {
            continue;
        }
        let limit = parse_limit(Some(part), Limit::Fixed(0))?;
        if let Limit::Fixed(value) = limit
            && value > MAX_PREVIEWS
        {
            return Err(AppError::BadRequest(format!(
                "limit must be between 0 and {MAX_PREVIEWS}"
            )));
        }
        limits.push(limit);
    }

    if limits.is_empty() {
        Ok(defaults.to_vec())
    } else {
        Ok(limits)
    }
}

/// Parses a slash-separated boolean list: `-/+` means `[false, true]`.
pub fn parse_layer_bools(raw: Option<&str>) -> Vec<bool> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Vec::new();
    };

    raw.split(['/', ','])
        .map(|part| match part {
            // A literal `+` reaches the handler as a space when the query
            // string is decoded with `application/x-www-form-urlencoded` rules.
            "+" | " " => true,
            "-" => false,
            other => matches!(
                other.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
        })
        .collect()
}

/// Pushes a comma-separated list of bound album/media ids.
fn push_id_list(builder: &mut QueryBuilder<Sqlite>, ids: &[i64]) {
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push_bind(*id);
    }
}

/// A row of materialized direct counters plus derived nested counters.
#[derive(Debug, FromRow)]
struct StatsRow {
    album_id: i64,
    medias_count: i64,
    images_count: i64,
    videos_count: i64,
    audios_count: i64,
    albums_count: i64,
    size: i64,
    duration: f64,
    nested_medias_count: i64,
    nested_images_count: i64,
    nested_videos_count: i64,
    nested_audios_count: i64,
    nested_albums_count: i64,
    nested_size: i64,
    nested_duration: f64,
}

/// Loads direct and nested counters for a set of albums in one query.
///
/// Direct counters come from the trigger-maintained `album_stats`. Nested
/// counters are summed from the same rows over the subtree, so this never scans
/// `media` and no per-album query is needed.
pub async fn load_album_stats_map(
    pool: &SqlitePool,
    album_ids: &[i64],
) -> Result<HashMap<i64, AlbumStatsPair>, AppError> {
    if album_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut builder = QueryBuilder::<Sqlite>::new("");
    builder.push("WITH RECURSIVE wanted(id) AS (VALUES ");
    for (index, id) in album_ids.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push("(").push_bind(*id).push(")");
    }
    // `UNION` (not `UNION ALL`) also stops the walk if bad data ever forms a
    // cycle, which the schema cannot prevent for cycles longer than one edge.
    builder.push(
        "), closure(root_id, id) AS ( \
             SELECT id, id FROM wanted \
             UNION \
             SELECT closure.root_id, albums.id FROM albums JOIN closure \
               ON albums.parent_id = closure.id\
         ) ",
    );
    builder.push(
        "SELECT s.album_id, s.medias_count, s.images_count, s.videos_count, \
                s.audios_count, s.albums_count, s.size, s.duration, \
                COALESCE(n.medias_count, 0) AS nested_medias_count, \
                COALESCE(n.images_count, 0) AS nested_images_count, \
                COALESCE(n.videos_count, 0) AS nested_videos_count, \
                COALESCE(n.audios_count, 0) AS nested_audios_count, \
                COALESCE(n.albums_count, 0) AS nested_albums_count, \
                COALESCE(n.size, 0) AS nested_size, \
                COALESCE(n.duration, 0.0) AS nested_duration \
         FROM album_stats s \
         LEFT JOIN ( \
             SELECT closure.root_id, \
                    SUM(CASE WHEN closure.id = closure.root_id THEN 0 \
                             ELSE st.medias_count END) AS medias_count, \
                    SUM(CASE WHEN closure.id = closure.root_id THEN 0 \
                             ELSE st.images_count END) AS images_count, \
                    SUM(CASE WHEN closure.id = closure.root_id THEN 0 \
                             ELSE st.videos_count END) AS videos_count, \
                    SUM(CASE WHEN closure.id = closure.root_id THEN 0 \
                             ELSE st.audios_count END) AS audios_count, \
                    COUNT(CASE WHEN closure.id = closure.root_id THEN NULL \
                               ELSE 1 END) AS albums_count, \
                    SUM(CASE WHEN closure.id = closure.root_id THEN 0 \
                             ELSE st.size END) AS size, \
                    SUM(CASE WHEN closure.id = closure.root_id THEN 0.0 \
                             ELSE st.duration END) AS duration \
             FROM closure JOIN album_stats st ON st.album_id = closure.id \
             GROUP BY closure.root_id \
         ) n ON n.root_id = s.album_id \
         WHERE s.album_id IN (SELECT id FROM wanted)",
    );

    let rows: Vec<StatsRow> = builder.build_query_as().fetch_all(pool).await?;
    let mut stats = HashMap::with_capacity(rows.len());
    for row in rows {
        stats.insert(
            row.album_id,
            AlbumStatsPair {
                direct: AlbumStats {
                    medias_count: row.medias_count,
                    images_count: row.images_count,
                    videos_count: row.videos_count,
                    audios_count: row.audios_count,
                    albums_count: row.albums_count,
                    size: row.size,
                    duration: row.duration,
                },
                nested: AlbumStats {
                    medias_count: row.nested_medias_count,
                    images_count: row.nested_images_count,
                    videos_count: row.nested_videos_count,
                    audios_count: row.nested_audios_count,
                    albums_count: row.nested_albums_count,
                    size: row.nested_size,
                    duration: row.nested_duration,
                },
            },
        );
    }
    Ok(stats)
}

/// Appends `AND media."type" IN (...)` to a media query.
pub fn push_media_type_filter(builder: &mut QueryBuilder<Sqlite>, types: &[MediaType]) {
    if types.is_empty() {
        return;
    }

    builder.push(" AND media.\"type\" IN (");
    for (index, media_type) in types.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push_bind(media_type_db(*media_type));
    }
    builder.push(")");
}

/// Appends the `ORDER BY` clause for a media query. The query must alias the
/// `media` table as `media`.
pub fn push_media_order(builder: &mut QueryBuilder<Sqlite>, sorts: &[MediaSort]) {
    builder.push(" ORDER BY ");
    push_media_order_exprs(builder, sorts);
}

/// Appends the media ordering expressions without a leading `ORDER BY`.
///
/// Used both for regular queries and for `ROW_NUMBER() OVER (...)` window
/// functions, where the expressions must sit inside the window clause.
pub fn push_media_order_exprs(builder: &mut QueryBuilder<Sqlite>, sorts: &[MediaSort]) {
    for (index, sort) in sorts.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        let direction = if sort.descending { " DESC" } else { " ASC" };
        match sort.field {
            MediaSortField::Name => {
                builder.push("media.sort_name").push(direction);
            }
            MediaSortField::Date => {
                // Undated files stay last regardless of direction.
                builder
                    .push("media.date IS NULL ASC, media.date")
                    .push(direction);
            }
            MediaSortField::Random => match sort.seed {
                Some(seed) => {
                    builder
                        .push("ABS(media.id * 2654435761 + ")
                        .push_bind(seed)
                        .push(") % 2147483647")
                        .push(direction);
                }
                None => {
                    builder.push("RANDOM()");
                }
            },
            MediaSortField::Size => {
                builder.push("media.size").push(direction);
            }
            MediaSortField::Width => {
                builder.push("media.width").push(direction);
            }
            MediaSortField::Height => {
                builder.push("media.height").push(direction);
            }
            MediaSortField::Ratio => {
                builder
                    .push("(CAST(media.width AS REAL) / NULLIF(media.height, 0))")
                    .push(direction);
            }
            MediaSortField::Square => {
                builder
                    .push(
                        "ABS((CAST(MAX(media.width, media.height) AS REAL) \
                         / NULLIF(MIN(media.width, media.height), 0)) - 1)",
                    )
                    .push(direction);
            }
            MediaSortField::Reacts => {
                // Reactions are not stored yet; accept the key without changing
                // the order.
                builder.push("0");
            }
            MediaSortField::Duration => {
                builder
                    .push("media.duration_sec IS NULL ASC, media.duration_sec")
                    .push(direction);
            }
            MediaSortField::Bitrate => {
                builder
                    .push(
                        "media.duration_sec IS NULL ASC, \
                         (CAST(media.size AS REAL) * 8.0 / NULLIF(media.duration_sec, 0))",
                    )
                    .push(direction);
            }
            MediaSortField::Framerate => {
                builder.push("0");
            }
            MediaSortField::Order => {
                builder.push("media.id").push(direction);
            }
        }
    }
    // Stable order for equal keys and pagination.
    builder.push(", media.sort_name ASC, media.id ASC");
}

/// Appends the `ORDER BY` clause for a child-album query. The query must use
/// the `albums` table without an alias and join `album_stats`.
pub fn push_album_order(builder: &mut QueryBuilder<Sqlite>, sorts: &[AlbumSort]) {
    builder.push(" ORDER BY ");
    push_album_order_exprs(builder, sorts);
}

/// Appends album ordering expressions without a leading `ORDER BY`.
///
/// The query must use `albums` unaliased and join `album_stats`; the counters
/// come from the materialized table instead of per-row media scans.
pub fn push_album_order_exprs(builder: &mut QueryBuilder<Sqlite>, sorts: &[AlbumSort]) {
    for (index, sort) in sorts.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        let direction = if sort.descending { " DESC" } else { " ASC" };
        match sort.field {
            AlbumSortField::Name => {
                builder.push("albums.sort_name").push(direction);
            }
            AlbumSortField::Content => {
                builder
                    .push(
                        "COALESCE((SELECT m.sort_name FROM media m WHERE m.album_id = albums.id \
                         ORDER BY m.sort_name LIMIT 1), albums.sort_name)",
                    )
                    .push(direction);
            }
            AlbumSortField::Random => match sort.seed {
                Some(seed) => {
                    builder
                        .push("ABS(albums.id * 2654435761 + ")
                        .push_bind(seed)
                        .push(") % 2147483647")
                        .push(direction);
                }
                None => {
                    builder.push("RANDOM()");
                }
            },
            AlbumSortField::Size => {
                builder
                    .push("COALESCE(album_stats.size, 0)")
                    .push(direction);
            }
            AlbumSortField::Duration => {
                builder
                    .push("COALESCE(album_stats.duration, 0.0)")
                    .push(direction);
            }
            AlbumSortField::Albums => {
                builder
                    .push("COALESCE(album_stats.albums_count, 0)")
                    .push(direction);
            }
            AlbumSortField::Medias => {
                builder
                    .push("COALESCE(album_stats.medias_count, 0)")
                    .push(direction);
            }
            AlbumSortField::Images => {
                builder
                    .push("COALESCE(album_stats.images_count, 0)")
                    .push(direction);
            }
            AlbumSortField::Videos => {
                builder
                    .push("COALESCE(album_stats.videos_count, 0)")
                    .push(direction);
            }
            AlbumSortField::Audios => {
                builder
                    .push("COALESCE(album_stats.audios_count, 0)")
                    .push(direction);
            }
            AlbumSortField::Created => {
                builder.push("albums.created_at").push(direction);
            }
            AlbumSortField::Indexed => {
                builder
                    .push("albums.indexed_at IS NULL ASC, albums.indexed_at")
                    .push(direction);
            }
            AlbumSortField::Order => {
                // Manual position from the edit form; `0` falls back to the
                // name tiebreaker below.
                builder.push("albums.order_level").push(direction);
            }
        }
    }
    builder.push(", albums.sort_name ASC, albums.id ASC");
}

/// A joined media row, ready to be turned into a [`MediaItem`].
#[derive(Debug, FromRow)]
struct MediaRow {
    name: String,
    hash: String,
    #[sqlx(rename = "type")]
    media_type: MediaType,
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
    date: Option<DateTime<Utc>>,
    size: i64,
    rating_level: Option<i64>,
    /// Album that owns the row; drives whether `album` is serialized.
    album_id: i64,
    album_hash: String,
    album_name: String,
    album_path: String,
    album_alias: Option<String>,
    album_rating_level: Option<i64>,
    /// Album whose preview slot this row belongs to. Equal to `media.album_id`
    /// for non-nested queries and to the subtree root for `mnest` previews.
    viewer_id: i64,
}

/// Everything needed to select a portion of an album's media.
pub struct MediaSelection<'a> {
    pub album_id: i64,
    /// Include media of every descendant album.
    pub nested: bool,
    pub types: &'a [MediaType],
    pub sorts: &'a [MediaSort],
    /// `None` means "no limit".
    pub limit: Option<u32>,
    pub offset: i64,
    pub exp: i64,
}

/// Counts media in one album or its whole subtree.
pub async fn count_media(
    pool: &SqlitePool,
    album_id: i64,
    nested: bool,
    types: &[MediaType],
) -> Result<i64, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new("");
    if nested {
        builder
            .push("WITH RECURSIVE tree(id) AS (SELECT ")
            .push_bind(album_id)
            .push(
                " UNION SELECT albums.id FROM albums JOIN tree ON albums.parent_id = tree.id) \
                 SELECT COUNT(*) FROM media WHERE media.album_id IN (SELECT id FROM tree)",
            );
    } else {
        builder
            .push("SELECT COUNT(*) FROM media WHERE media.album_id = ")
            .push_bind(album_id);
    }
    push_media_type_filter(&mut builder, types);

    Ok(builder.build_query_scalar().fetch_one(pool).await?)
}

/// Loads media items for one album or its whole subtree.
pub async fn load_media_items(
    state: &AppState,
    selection: &MediaSelection<'_>,
) -> Result<Vec<MediaItem>, AppError> {
    let mut builder = QueryBuilder::<Sqlite>::new("");
    if selection.nested {
        builder
            .push("WITH RECURSIVE tree(id) AS (SELECT ")
            .push_bind(selection.album_id)
            .push(" UNION SELECT albums.id FROM albums JOIN tree ON albums.parent_id = tree.id) ");
        builder
            .push("SELECT ")
            .push(MEDIA_COLUMNS)
            .push(", media.album_id AS viewer_id")
            .push(MEDIA_FROM);
        builder.push(" WHERE media.album_id IN (SELECT id FROM tree)");
    } else {
        builder
            .push("SELECT ")
            .push(MEDIA_COLUMNS)
            .push(", media.album_id AS viewer_id")
            .push(MEDIA_FROM);
        builder
            .push(" WHERE media.album_id = ")
            .push_bind(selection.album_id);
    }
    push_media_type_filter(&mut builder, selection.types);
    push_media_order(&mut builder, selection.sorts);

    match selection.limit {
        Some(limit) => {
            builder.push(" LIMIT ").push_bind(i64::from(limit));
            if selection.offset > 0 {
                builder.push(" OFFSET ").push_bind(selection.offset);
            }
        }
        None if selection.offset > 0 => {
            // SQLite requires a LIMIT before OFFSET; `-1` means unlimited.
            builder
                .push(" LIMIT -1 OFFSET ")
                .push_bind(selection.offset);
        }
        None => {}
    }

    let rows: Vec<MediaRow> = builder.build_query_as().fetch_all(&state.db).await?;
    Ok(rows
        .into_iter()
        .map(|row| MediaItem::from_row(state, row, selection.exp, selection.album_id))
        .collect())
}

/// Loads the first `limit` children of every parent in one query.
///
/// `limit == None` means unlimited. The window function keeps the per-parent
/// limit in SQL, so callers never issue one query per album. Results are
/// ordered by parent and then by the requested album sort.
pub async fn load_children_batch(
    pool: &SqlitePool,
    parent_ids: &[i64],
    sorts: &[AlbumSort],
    limit: Option<u32>,
) -> Result<Vec<Album>, AppError> {
    if parent_ids.is_empty() || limit == Some(0) {
        return Ok(Vec::new());
    }

    let mut builder = QueryBuilder::<Sqlite>::new("SELECT * FROM (SELECT ");
    builder.push(ALBUM_FIELDS_QUALIFIED);
    builder.push(", ROW_NUMBER() OVER (PARTITION BY albums.parent_id ORDER BY ");
    push_album_order_exprs(&mut builder, sorts);
    builder.push(
        ") AS rn FROM albums \
         LEFT JOIN album_stats ON album_stats.album_id = albums.id \
         WHERE albums.parent_id IN (",
    );
    push_id_list(&mut builder, parent_ids);
    builder.push("))");
    if let Some(limit) = limit {
        builder.push(" WHERE rn <= ").push_bind(i64::from(limit));
    }
    builder.push(" ORDER BY parent_id, rn");

    Ok(builder.build_query_as().fetch_all(pool).await?)
}

/// Everything needed to select media previews for a batch of albums.
pub struct MediaPreview<'a> {
    /// Album from the request path; media owned by it are serialized without
    /// their `album`, which the client already knows from the URL.
    pub context_album_id: i64,
    /// Include media of every descendant album.
    pub nested: bool,
    pub types: &'a [MediaType],
    pub sorts: &'a [MediaSort],
    /// `None` means "no limit".
    pub limit: Option<u32>,
    pub exp: i64,
}

/// Loads media previews for every viewer album in one query.
///
/// With `nested == false` each viewer gets its own media, capped per album by
/// `limit`. With `nested == true` each viewer also gets media from its whole
/// subtree, capped per viewer. The returned map contains an entry, possibly
/// empty, for every requested viewer.
pub async fn load_media_previews(
    state: &AppState,
    viewer_ids: &[i64],
    preview: &MediaPreview<'_>,
) -> Result<HashMap<i64, Vec<MediaItem>>, AppError> {
    let mut previews: HashMap<i64, Vec<MediaItem>> =
        viewer_ids.iter().map(|id| (*id, Vec::new())).collect();

    if viewer_ids.is_empty() || preview.limit == Some(0) {
        return Ok(previews);
    }

    let mut builder = QueryBuilder::<Sqlite>::new("");
    if preview.nested {
        builder.push(
            "WITH RECURSIVE tree(root_id, id) AS (SELECT albums.id, albums.id \
             FROM albums WHERE albums.id IN (",
        );
        push_id_list(&mut builder, viewer_ids);
        builder.push(
            ") UNION SELECT tree.root_id, albums.id FROM albums JOIN tree \
             ON albums.parent_id = tree.id) ",
        );
        builder.push("SELECT * FROM (SELECT ");
        builder.push(MEDIA_COLUMNS);
        builder.push(
            ", tree.root_id AS viewer_id, \
             ROW_NUMBER() OVER (PARTITION BY tree.root_id ORDER BY ",
        );
        push_media_order_exprs(&mut builder, preview.sorts);
        builder.push(
            ") AS rn FROM tree \
             JOIN media ON media.album_id = tree.id \
             JOIN albums ON albums.id = media.album_id \
             WHERE 1 = 1",
        );
    } else {
        builder.push("SELECT * FROM (SELECT ");
        builder.push(MEDIA_COLUMNS);
        builder.push(
            ", media.album_id AS viewer_id, \
             ROW_NUMBER() OVER (PARTITION BY media.album_id ORDER BY ",
        );
        push_media_order_exprs(&mut builder, preview.sorts);
        builder.push(") AS rn").push(MEDIA_FROM);
        builder.push(" WHERE media.album_id IN (");
        push_id_list(&mut builder, viewer_ids);
        builder.push(")");
    }
    push_media_type_filter(&mut builder, preview.types);
    builder.push(")");
    if let Some(limit) = preview.limit {
        builder.push(" WHERE rn <= ").push_bind(i64::from(limit));
    }
    builder.push(" ORDER BY viewer_id, rn");

    let rows: Vec<MediaRow> = builder.build_query_as().fetch_all(&state.db).await?;
    for row in rows {
        let viewer_id = row.viewer_id;
        previews
            .entry(viewer_id)
            .or_default()
            .push(MediaItem::from_row(
                state,
                row,
                preview.exp,
                preview.context_album_id,
            ));
    }

    Ok(previews)
}
