use std::io::SeekFrom;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use super::pagination::Pagination;
use crate::controllers::album::resolve_album;
use crate::error::AppError;
use crate::models::Media;
use crate::state::AppState;

/// Content-addressed paths never change, so files may be cached forever.
const CACHE_CONTROL_VALUE: &str = "public, max-age=31536000, immutable";

const MEDIA_SELECT: &str = "SELECT id, name, hash, \"type\", album_id, width, height, \
     duration_sec, date, size, sort_name, created_at, updated_at FROM media";

/// `GET /api/album/{hash}/media?page=1&limit=30&sort=date`
#[derive(Debug, Deserialize)]
pub struct MediaQuery {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    /// `date`, `name`, `size`, optionally prefixed with `-` for descending order.
    pub sort: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaPage {
    pub items: Vec<Media>,
    pub page: u32,
    pub limit: u32,
    pub total: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortField {
    Date,
    Name,
    Size,
}

#[derive(Debug, Clone, Copy)]
struct Sort {
    field: SortField,
    descending: bool,
}

impl Sort {
    fn parse(raw: Option<&str>) -> Result<Self, AppError> {
        let raw = raw.unwrap_or_default();
        let (descending, field) = match raw.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };

        let field = match field {
            // Default sort: the first documented option, `date` ascending.
            "" | "date" => SortField::Date,
            "name" => SortField::Name,
            "size" => SortField::Size,
            other => {
                return Err(AppError::BadRequest(format!(
                    "invalid sort `{other}`, expected one of: date, -date, name, -name, size, -size"
                )));
            }
        };

        Ok(Self { field, descending })
    }
}

pub async fn list_media(
    State(state): State<AppState>,
    Path(album_hash): Path<String>,
    Query(query): Query<MediaQuery>,
) -> Result<Json<MediaPage>, AppError> {
    let album = resolve_album(&state, &album_hash).await?;
    let pagination = Pagination::new(query.page, query.limit)?;
    let sort = Sort::parse(query.sort.as_deref())?;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE album_id = ?1")
        .bind(album.id)
        .fetch_one(&state.db)
        .await?;

    let mut builder = QueryBuilder::<Sqlite>::new(MEDIA_SELECT);
    builder.push(" WHERE album_id = ");
    builder.push_bind(album.id);
    builder.push(" ORDER BY ");
    match sort.field {
        // `date IS NULL` keeps undated media at the end in both directions.
        SortField::Date => builder.push("date IS NULL, date"),
        SortField::Name => builder.push("sort_name"),
        SortField::Size => builder.push("size"),
    };
    builder.push(if sort.descending { " DESC" } else { " ASC" });
    builder.push(", id ");
    builder.push(if sort.descending { " DESC" } else { " ASC" });
    builder.push(" LIMIT ");
    builder.push_bind(i64::from(pagination.limit));
    builder.push(" OFFSET ");
    builder.push_bind(pagination.offset);

    let items: Vec<Media> = builder.build_query_as().fetch_all(&state.db).await?;

    Ok(Json(MediaPage {
        items,
        page: pagination.page,
        limit: pagination.limit,
        total,
        pages: pagination.pages(total),
    }))
}

/// `GET /files/{albumHash}/{mediaHash}` — streams the stored file.
///
/// Supports a single `Range: bytes=...` and advertises long-lived caching:
/// storage paths are content-addressed and never change.
pub async fn get_file(
    State(state): State<AppState>,
    Path((album_hash, media_hash)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let album = resolve_album(&state, &album_hash).await?;

    let media: Media = sqlx::query_as(concat!(
        "SELECT id, name, hash, \"type\", album_id, width, height, duration_sec, date, size,",
        " sort_name, created_at, updated_at FROM media WHERE album_id = ?1 AND hash = ?2"
    ))
    .bind(album.id)
    .bind(&media_hash)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let path = state.media_path(&album, &media).ok_or(AppError::NotFound)?;

    let file = tokio::fs::File::open(&path).await?;
    let len = file.metadata().await?.len();
    let mime = mime_guess::from_path(&path).first_or_octet_stream();

    let range = headers
        .get(RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|raw| parse_range(raw, len))
        .unwrap_or(RangeRequest::Full);

    match range {
        RangeRequest::Full => {
            let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
            base_headers(response.headers_mut(), mime.as_ref(), len);
            Ok(response)
        }
        RangeRequest::Partial { start, end } => {
            let length = end - start + 1;
            let mut file = file;
            file.seek(SeekFrom::Start(start)).await?;

            let mut response =
                Response::new(Body::from_stream(ReaderStream::new(file.take(length))));
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;

            let headers = response.headers_mut();
            base_headers(headers, mime.as_ref(), length);
            headers.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{len}"))
                    .expect("content-range is ASCII"),
            );

            Ok(response)
        }
        RangeRequest::Unsatisfiable => Ok(range_not_satisfiable(len)),
    }
}

fn base_headers(headers: &mut HeaderMap, content_type: &str, length: u64) {
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE));
}

fn range_not_satisfiable(total: u64) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;

    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(0_u64));
    headers.insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{total}")).expect("content-range is ASCII"),
    );

    response
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeRequest {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

/// Parses a single `bytes=` range against a file of `len` bytes.
///
/// Malformed or multi-range values fall back to `Full`: RFC 9110 lets a server
/// ignore `Range`, and refusing to stream anything would be worse.
fn parse_range(raw: &str, len: u64) -> RangeRequest {
    let Some((unit, spec)) = raw.trim().split_once('=') else {
        return RangeRequest::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return RangeRequest::Full;
    }

    let spec = spec.trim();
    if spec.contains(',') {
        return RangeRequest::Full;
    }

    let Some((from, to)) = spec.split_once('-') else {
        return RangeRequest::Full;
    };
    let (from, to) = (from.trim(), to.trim());

    if from.is_empty() {
        // Suffix range: the last N bytes.
        let Ok(suffix) = to.parse::<u64>() else {
            return RangeRequest::Full;
        };
        if suffix == 0 || len == 0 {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Partial {
            start: len.saturating_sub(suffix),
            end: len - 1,
        };
    }

    let Ok(start) = from.parse::<u64>() else {
        return RangeRequest::Full;
    };
    if len == 0 || start >= len {
        return RangeRequest::Unsatisfiable;
    }

    let end = if to.is_empty() {
        len - 1
    } else {
        match to.parse::<u64>() {
            Ok(end) => end.min(len - 1),
            Err(_) => return RangeRequest::Full,
        }
    };
    if end < start {
        return RangeRequest::Unsatisfiable;
    }

    RangeRequest::Partial { start, end }
}

#[cfg(test)]
mod tests {
    use super::{RangeRequest, parse_range};

    #[test]
    fn parses_range_forms() {
        assert_eq!(
            parse_range("bytes=0-3", 10),
            RangeRequest::Partial { start: 0, end: 3 }
        );
        assert_eq!(
            parse_range("bytes=5-", 10),
            RangeRequest::Partial { start: 5, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=-3", 10),
            RangeRequest::Partial { start: 7, end: 9 }
        );
        // A suffix longer than the file means the whole file.
        assert_eq!(
            parse_range("bytes=-100", 10),
            RangeRequest::Partial { start: 0, end: 9 }
        );
        // An end past the file is clamped to its last byte.
        assert_eq!(
            parse_range("bytes=8-100", 10),
            RangeRequest::Partial { start: 8, end: 9 }
        );
    }

    #[test]
    fn rejects_unsatisfiable_ranges() {
        assert_eq!(parse_range("bytes=10-", 10), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=5-3", 10), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 10), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), RangeRequest::Unsatisfiable);
    }

    #[test]
    fn ignores_malformed_and_multi_ranges() {
        for raw in [
            "items=0-1",
            "bytes=abc",
            "bytes=0-1,3-4",
            "bytes=",
            "bytes=1-2-3",
        ] {
            assert_eq!(parse_range(raw, 10), RangeRequest::Full, "raw: {raw}");
        }
    }
}
