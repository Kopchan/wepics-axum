//! `GET /thumb/v1/{album}/{media}/{direction}{size}` — generated WebP previews.
//!
//! Rendering goes through libvips: `vips_thumbnail` combines decode and resize,
//! so large JPEGs are decoded directly at the target scale using shrink-on-load
//! instead of being expanded to full size in memory first.

use std::path::Path as FsPath;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;

use crate::error::AppError;
use crate::helpers::thumb::{parse_spec, render};
use crate::models::MediaType;
use crate::resolvers::{resolve_album, resolve_media};
use crate::state::AppState;

use super::secure::verify_link;
use super::types::LinkQuery;

/// `GET /thumb/v1/{album_hash}/{media_hash}/{spec}?hash=&exp=`.
pub async fn get_thumb(
    State(state): State<AppState>,
    Path((album_hash, media_hash, spec)): Path<(String, String, String)>,
    Query(query): Query<LinkQuery>,
) -> Result<Response, AppError> {
    verify_link(&state, &album_hash, &media_hash, &query)?;

    let (direction, size) = parse_spec(&spec)?;
    let album = resolve_album(&state, &album_hash).await?;
    let media = resolve_media(&state, &album, &media_hash).await?;

    if !matches!(
        media.r#type,
        MediaType::Image | MediaType::Animated | MediaType::Vector
    ) {
        return Err(AppError::BadRequest(
            "previews are only available for images".into(),
        ));
    }

    let source = state.media_path(&album, &media).ok_or(AppError::NotFound)?;
    if !tokio::fs::metadata(&source).await?.is_file() {
        return Err(AppError::NotFound);
    }

    let cache_path = state
        .thumb_dir()
        .join(format!("{direction}{size}"))
        .join(format!("{}.webp", media.hash));

    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        return Ok(webp_response(bytes));
    }

    let bytes = generate(&source, direction, size, &media).await?;

    if let Some(parent) = cache_path.parent() {
        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            eprintln!(
                "cannot create thumbnail directory `{}`: {err}",
                parent.display()
            );
        } else if let Err(err) = tokio::fs::write(&cache_path, &bytes).await {
            eprintln!("cannot cache thumbnail `{}`: {err}", cache_path.display());
        }
    }

    Ok(webp_response(bytes))
}

async fn generate(
    source: &FsPath,
    direction: char,
    size: u32,
    media: &crate::models::Media,
) -> Result<Vec<u8>, AppError> {
    let source = source.to_path_buf();
    let width = media.width;
    let height = media.height;
    tokio::task::spawn_blocking(move || render(&source, direction, size, width, height))
        .await
        .map_err(|err| AppError::Internal(format!("thumbnail worker failed: {err}")))?
        .map_err(AppError::Internal)
}

fn webp_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/webp")
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=86400, immutable"),
        )
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes))
        .expect("static response parts are valid")
}
