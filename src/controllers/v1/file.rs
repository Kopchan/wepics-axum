//! `GET /file/v1/{album}/{media}` — the original media file.

use std::path::Path as FsPath;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION};
use axum::http::{HeaderMap, HeaderValue, Method, Request};
use axum::response::Response;
use tower::{Layer, ServiceExt};
use tower_http::services::ServeFile;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::controllers::album::resolve_album;
use crate::error::AppError;
use crate::models::Media;
use crate::state::AppState;

use super::secure::verify_link;
use super::types::{LinkQuery, MEDIA_BY_HASH};

const CACHE_CONTROL_VALUE: &str = "public, max-age=3600, immutable";

/// `GET /file/v1/{album_hash}/{media_hash}?hash=&exp=`.
pub async fn get_file(
    State(state): State<AppState>,
    Path((album_hash, media_hash)): Path<(String, String)>,
    Query(query): Query<LinkQuery>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    verify_link(&state, &album_hash, &media_hash, &query)?;

    let album = resolve_album(&state, &album_hash).await?;
    let media: Media = sqlx::query_as(MEDIA_BY_HASH)
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

    let extension = FsPath::new(&media.name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .unwrap_or("bin");
    let disposition = HeaderValue::from_str(&format!(
        "inline; filename=\"wpx {}.{}\"",
        media.hash, extension
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("inline"));

    let cache = SetResponseHeaderLayer::overriding(
        CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL_VALUE),
    );
    let content_disposition = SetResponseHeaderLayer::overriding(CONTENT_DISPOSITION, disposition);
    let service = content_disposition
        .layer(cache.layer(ServeFile::new(&path).ignore_multi_range_requests(true)));

    let response = service
        .oneshot(request)
        .await
        .expect("ServeFile is infallible");
    let (parts, body) = response.into_parts();

    Ok(Response::from_parts(parts, Body::new(body)))
}
