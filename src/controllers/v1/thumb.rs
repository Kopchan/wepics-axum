//! `GET /thumb/v1/{album}/{media}/{direction}{size}` — generated WebP previews.
//!
//! Rendering goes through libvips: `vips_thumbnail` combines decode and resize,
//! so large JPEGs are decoded directly at the target scale using shrink-on-load
//! instead of being expanded to full size in memory first.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;

use crate::error::AppError;
use crate::helpers::thumb::{parse_spec, render};
use crate::models::MediaType;
use crate::resolvers::{resolve_album, resolve_media};
use crate::state::AppState;

use super::secure::{media_cache_control, verify_link};
use super::types::LinkQuery;

/// `GET /thumb/v1/{album_hash}/{media_hash}/{spec}?sign=&exp=`.
pub async fn get_thumb(
    State(state): State<AppState>,
    Path((album_hash, media_hash, spec)): Path<(String, String, String)>,
    Query(query): Query<LinkQuery>,
) -> Result<Response, AppError> {
    let exp = verify_link(&state, &album_hash, &media_hash, &query)?;

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

    // Быстрый путь: файл уже есть на диске
    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        return Ok(webp_response(bytes, exp));
    }

    // Медленный путь: дедупликация генерации через ThumbHub
    let key = format!("{direction}{size}/{}", media.hash);
    let width = media.width;
    let height = media.height;
    let cache_path_clone = cache_path.clone();

    let shared_bytes = state
        .thumb_hub()
        .get_or_generate(key, move || async move {
            // Повторная проверка на случай, если файл появился за микросекунду до входа в хаб
            if let Ok(existing) = tokio::fs::read(&cache_path_clone).await {
                return Ok(existing);
            }

            let bytes = tokio::task::spawn_blocking(move || {
                render(&source, direction, size, width, height)
            })
            .await
            .map_err(|err| format!("thumbnail worker failed: {err}"))??;

            // Атомарная запись на диск через временный файл
            if let Some(parent) = cache_path_clone.parent()
                && tokio::fs::create_dir_all(parent).await.is_ok()
            {
                // Имя уникально для пары «медиа + размер», поэтому параллельные
                // генерации разных превью не перетирают временные файлы.
                let tmp_path = cache_path_clone.with_extension("tmp");
                if tokio::fs::write(&tmp_path, &bytes).await.is_ok() {
                    let _ = tokio::fs::rename(&tmp_path, &cache_path_clone).await;
                }
            }

            Ok(bytes)
        })
        .await
        .map_err(AppError::Internal)?;

    // Извлекаем Vec<u8> без копирования, если мы последний владелец Arc, или клонируем при необходимости
    let bytes = Arc::unwrap_or_clone(shared_bytes);
    Ok(webp_response(bytes, exp))
}

fn webp_response(bytes: Vec<u8>, exp: i64) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/webp")
        .header(header::CACHE_CONTROL, media_cache_control(exp))
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes))
        .expect("static response parts are valid")
}
