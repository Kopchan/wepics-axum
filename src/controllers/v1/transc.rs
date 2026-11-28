//! `GET /transc/v1/{album}/{media}/{direction}{size}` — streaming video previews.
//!
//! FFmpeg encodes the first seconds of a video to a fragmented MP4 and the
//! response streams the process output as it is produced. The scale, frame
//! rate and audio rules mirror `/thumb/v1`: a fixed cap on one axis, 30 fps and
//! no audio.
//!
//! Transcodes are shared: every request for the same clip and size attaches to
//! one background job, and the finished clip is cached for later requests.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use tokio::io::{AsyncRead, ReadBuf};
use tokio_stream::Stream;

use crate::error::AppError;
use crate::helpers::thumb::parse_spec;
use crate::helpers::video::spawn_transcode;
use crate::models::MediaType;
use crate::resolvers::{resolve_album, resolve_media};
use crate::state::AppState;

use super::secure::{media_cache_control, original_redirect, verify_link};
use super::types::LinkQuery;

/// `GET /transc/v1/{album_hash}/{media_hash}/{spec}?sign=&exp=`.
pub async fn get_transcode(
    State(state): State<AppState>,
    Path((album_hash, media_hash, spec)): Path<(String, String, String)>,
    Query(query): Query<LinkQuery>,
) -> Result<Response, AppError> {
    let exp = verify_link(&state, &album_hash, &media_hash, &query)?;

    let (direction, size) = parse_spec(&spec)?;
    if !matches!(direction, 'w' | 'h') {
        return Err(AppError::BadRequest(
            "transcode direction must be w or h".into(),
        ));
    }

    let album = resolve_album(&state, &album_hash).await?;
    let media = resolve_media(&state, &album, &media_hash).await?;
    if !matches!(media.r#type, MediaType::Video | MediaType::Animated) {
        return Err(AppError::BadRequest(
            "transcodes are only available for videos and animations".into(),
        ));
    }

    let source = state.media_path(&album, &media).ok_or(AppError::NotFound)?;
    if !tokio::fs::metadata(&source).await?.is_file() {
        return Err(AppError::NotFound);
    }

    // Without FFmpeg the server cannot produce an MP4. Redirect to the
    // original file instead of failing the viewer.
    if !state.capabilities.ffmpeg {
        return Ok(original_redirect(&album_hash, &media_hash, &query, exp));
    }
    
    let variant_dir = state
        .config
        .transcode_dir()
        .join(format!("{direction}{size}"));
    let cached_path = variant_dir.join(format!("{}.mp4", media.hash));
    let cache_key = format!("{direction}{size}/{}", media.hash);

    // 1. Если уже закэшировано на диске — отдаём сразу
    if let Ok(file) = tokio::fs::File::open(&cached_path).await {
        let usable = file
            .metadata()
            .await
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false);
        if usable {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "video/mp4")
                .header(header::CACHE_CONTROL, media_cache_control(exp))
                .body(Body::from_stream(FileStream { file }))
                .map_err(|err| AppError::Internal(err.to_string()));
        }
    }

    // 2. Иначе запускаем/подключаемся к общему процессу в TranscodeHub
    let ffmpeg_bin = state.config.ffmpeg_bin.clone();
    let stream = state.transcode_hub().subscribe(
        cache_key,
        variant_dir,
        cached_path,
        move || spawn_transcode(&ffmpeg_bin, &source, direction, size),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CACHE_CONTROL, media_cache_control(exp))
        .body(Body::from_stream(stream))
        .map_err(|err| AppError::Internal(err.to_string()))
}

struct FileStream {
    file: tokio::fs::File,
}

impl Stream for FileStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut buffer = [0_u8; 64 * 1024];
        let mut read = ReadBuf::new(&mut buffer);

        match Pin::new(&mut self.file).poll_read(cx, &mut read) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(err)) => Poll::Ready(Some(Err(err))),
            Poll::Ready(Ok(())) => {
                let filled = read.filled();
                if filled.is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Ok(Bytes::copy_from_slice(filled))))
                }
            }
        }
    }
}