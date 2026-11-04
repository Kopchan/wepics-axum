use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, Method, Request};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite};
use tower::{Layer, ServiceExt};
use tower_http::services::ServeFile;
use tower_http::set_header::SetResponseHeaderLayer;

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

/// `GET /files/{albumHash}/{mediaHash}` — serves the stored file.
///
/// Range, conditional requests (`If-*)`, `ETag` and `Last-Modified` are handled
/// by [`ServeFile`]; the long-lived cache policy is added by
/// [`SetResponseHeaderLayer`].
pub async fn get_file(
    State(state): State<AppState>,
    Path((album_hash, media_hash)): Path<(String, String)>,
    method: Method,
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
    if !tokio::fs::metadata(&path).await?.is_file() {
        return Err(AppError::NotFound);
    }

    // `ServeFile` only reads the method and headers; the URI is irrelevant.
    let mut request = Request::builder()
        .method(method)
        .uri("/")
        .body(Body::empty())
        .expect("static request parts are valid");
    *request.headers_mut() = headers;

    let cache = SetResponseHeaderLayer::overriding(
        CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL_VALUE),
    );
    let service = cache.layer(ServeFile::new(&path).ignore_multi_range_requests(true));
    let response = service
        .oneshot(request)
        .await
        .expect("ServeFile is infallible");

    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(parts, Body::new(body)))
}
