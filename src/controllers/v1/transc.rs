//! `GET /transc/v2/{album}/{media}/{direction}{size}` — streaming video previews.
//!
//! FFmpeg encodes the first seconds of a video to a fragmented MP4 and the
//! response streams the process output as it is produced. The scale, frame
//! rate and audio rules mirror `/thumb/v1`: a fixed cap on one axis, 30 fps and
//! no audio.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio_stream::Stream;

use crate::error::AppError;
use crate::helpers::thumb::parse_spec;
use crate::helpers::video::spawn_transcode;
use crate::models::MediaType;
use crate::resolvers::{resolve_album, resolve_media};
use crate::state::AppState;

use super::secure::{media_cache_control, verify_link};
use super::types::LinkQuery;

/// `GET /transc/v2/{album_hash}/{media_hash}/{spec}?sign=&exp=`.
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
    if media.r#type != MediaType::Video {
        return Err(AppError::BadRequest(
            "transcodes are only available for videos".into(),
        ));
    }

    let source = state.media_path(&album, &media).ok_or(AppError::NotFound)?;
    if !tokio::fs::metadata(&source).await?.is_file() {
        return Err(AppError::NotFound);
    }

    let mut child = spawn_transcode(&state.config.ffmpeg_bin, &source, direction, size)
        .map_err(AppError::Internal)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Internal("ffmpeg produced no output pipe".into()))?;

    // Keep the pipe drained; FFmpeg would block once the OS buffer fills.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(log_stderr(stderr, media.hash.clone()));
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CACHE_CONTROL, media_cache_control(exp))
        .body(Body::from_stream(TranscodeStream {
            _child: child,
            stdout,
        }))
        .map_err(|err| AppError::Internal(err.to_string()))
}

/// Streams FFmpeg stdout as the response body.
///
/// The process handle is kept alive here; `kill_on_drop` on the command stops
/// FFmpeg as soon as the client goes away and this stream is dropped.
struct TranscodeStream {
    _child: Child,
    stdout: ChildStdout,
}

impl Stream for TranscodeStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut buffer = [0_u8; 64 * 1024];
        let mut read = ReadBuf::new(&mut buffer);

        match Pin::new(&mut this.stdout).poll_read(cx, &mut read) {
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

/// Collects FFmpeg diagnostics and logs them once the process ends. An empty
/// log means the transcode was clean.
async fn log_stderr(mut stderr: ChildStderr, media_hash: String) {
    let mut text = String::new();
    let _ = stderr.read_to_string(&mut text).await;
    let text = text.trim();
    if !text.is_empty() {
        eprintln!("ERR: transcode {media_hash}: {text}");
    }
}
