//! `GET /thumb/v1/{album}/{media}/{direction}{size}` — generated WebP previews.
//!
//! Rendering goes through libvips: `vips_thumbnail` combines decode and resize,
//! so large JPEGs are decoded directly at the target scale using shrink-on-load
//! instead of being expanded to full size in memory first.
//!
//! Videos and animations cannot be decoded by libvips, so one full-size frame
//! is extracted with FFmpeg and cached under `CACHE_DIR/frames`. Every preview
//! size is then produced from that PNG, which keeps re-decoding the source
//! video out of the hot path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;

use crate::error::AppError;
use crate::helpers::thumb::{parse_spec, render};
use crate::models::{Media, MediaType};
use crate::resolvers::{resolve_album, resolve_media};
use crate::state::AppState;

use super::secure::{media_cache_control, verify_link};
use super::types::LinkQuery;

/// `GET /thumb/v1/{album_hash}/{media_hash}/{spec}?sign=&exp=`.
pub async fn get_thumb(
    State(state): State<AppState>,
    UrlPath((album_hash, media_hash, spec)): UrlPath<(String, String, String)>,
    Query(query): Query<LinkQuery>,
) -> Result<Response, AppError> {
    let exp = verify_link(&state, &album_hash, &media_hash, &query)?;

    let (direction, size) = parse_spec(&spec)?;
    let album = resolve_album(&state, &album_hash).await?;
    let media = resolve_media(&state, &album, &media_hash).await?;

    if !matches!(
        media.r#type,
        MediaType::Image | MediaType::Animated | MediaType::Vector | MediaType::Video
    ) {
        return Err(AppError::BadRequest(
            "previews are only available for images and videos".into(),
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

    // Videos and animations are rendered from a cached frame so the source
    // stream is decoded once. An animation may still be renderable directly
    // by libvips when FFmpeg cannot open it.
    let render_source = match media.r#type {
        MediaType::Video | MediaType::Animated => {
            match ensure_frame(&state, source.clone(), &media).await {
                Ok(frame) => Some(frame),
                Err(err) if media.r#type == MediaType::Video => {
                    return Err(AppError::Internal(err));
                }
                Err(_) => None,
            }
        }
        _ => None,
    };

    // Медленный путь: дедупликация генерации через ThumbHub
    let key = format!("{direction}{size}/{}", media.hash);
    let width = media.width;
    let height = media.height;
    let cache_path_clone = cache_path.clone();
    let render_source = render_source.unwrap_or(source);

    let shared_bytes = state
        .thumb_hub()
        .get_or_generate(key, move || async move {
            // Повторная проверка на случай, если файл появился за микросекунду до входа в хаб
            if let Ok(existing) = tokio::fs::read(&cache_path_clone).await {
                return Ok(existing);
            }

            let bytes = tokio::task::spawn_blocking(move || {
                render(&render_source, direction, size, width, height)
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

/// Ensures a full-size PNG frame of `media` exists in the frame cache.
///
/// Extraction is deduplicated per media hash, so concurrent requests for
/// different preview sizes wait for one FFmpeg run instead of spawning several.
async fn ensure_frame(state: &AppState, source: PathBuf, media: &Media) -> Result<PathBuf, String> {
    let frame = state.frame_path(&media.hash);
    if is_file(&frame).await {
        return Ok(frame);
    }

    let key = format!("frame/{}", media.hash);
    let target = frame.clone();
    let ffmpeg = state.config.ffmpeg_bin.clone();
    let duration = media.duration_sec;

    state
        .thumb_hub()
        .get_or_generate(key, move || async move {
            // Another request may have finished while this one waited for the hub.
            if is_file(&target).await {
                return Ok(Vec::new());
            }

            let bytes = tokio::task::spawn_blocking(move || {
                crate::helpers::video::extract_frame(&ffmpeg, &source, duration)
            })
            .await
            .map_err(|err| format!("frame worker failed: {err}"))??;

            write_atomic(&target, &bytes).await?;
            Ok(bytes)
        })
        .await
        .map_err(|err| format!("cannot cache a frame: {err}"))?;

    Ok(frame)
}

async fn is_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

/// Writes `bytes` next to `path` and renames them into place, so a reader never
/// sees a half-written frame.
async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("cannot create `{}`: {err}", parent.display()))?;
    }

    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|err| format!("cannot write `{}`: {err}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|err| format!("cannot move `{}` into place: {err}", temporary.display()))
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
