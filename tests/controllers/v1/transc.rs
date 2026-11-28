//! `GET /transc/v1/{album}/{media}/{direction}{size}` — streaming video previews.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio_stream::StreamExt;
use tower::ServiceExt;

use crate::common::{
    app, ffmpeg_available, insert_typed_media, link, root_album_id, send, setup, write_sample_video,
};
use wepics_axum::hubs::transcode::MAX_CONCURRENT_TRANSCODES;
use wepics_axum::models::MediaType;

#[tokio::test]
async fn transcodes_stream_a_scaled_h264_clip_without_audio() {
    let (dir, state) = setup().await;

    if !ffmpeg_available() {
        return;
    }

    let root_id = root_album_id(&state).await;
    let source = state.config.media_dir.join("clip.mp4");
    if !write_sample_video(&source, 320, 240, 1) {
        return;
    }

    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "vid",
        MediaType::Video,
        320,
        240,
        None,
        4096,
    )
    .await;

    let app = app(state.clone());
    let query = link(&state, "root", "vid");

    let (status, headers, body) = send(&app, &format!("/transc/v1/root/vid/h144?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[axum::http::header::CONTENT_TYPE], "video/mp4");
    assert!(body.len() > 1_000, "the clip must contain encoded frames");
    assert_eq!(&body[4..8], b"ftyp", "the body is an MP4 container");

    // Inspect the produced stream: one video track, H.264, height capped at
    // 144 and a constant 30 fps, with no audio track.
    let output = dir.path().join("out.mp4");
    std::fs::write(&output, &body).unwrap();
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(probe.status.success(), "ffprobe must accept the output");

    let value: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let streams = value["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1, "audio and subtitle tracks are dropped");
    assert_eq!(streams[0]["codec_type"], "video");
    assert_eq!(streams[0]["codec_name"], "h264");
    assert_eq!(streams[0]["height"], 144);
    assert_eq!(streams[0]["avg_frame_rate"], "30/1");

    drop(dir);
}

#[tokio::test]
async fn transcodes_are_cached_and_reused() {
    let (dir, state) = setup().await;

    if !ffmpeg_available() {
        return;
    }

    let root_id = root_album_id(&state).await;
    let source = state.config.media_dir.join("clip.mp4");
    if !write_sample_video(&source, 640, 480, 2) {
        return;
    }

    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "vid",
        MediaType::Video,
        640,
        480,
        None,
        4096,
    )
    .await;

    let app = app(state.clone());
    let query = link(&state, "root", "vid");
    let uri = format!("/transc/v1/root/vid/h144?{query}");

    let (status, _, first) = send(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);

    let cached = state.config.transcode_dir().join("h144").join("vid.mp4");
    let stored = std::fs::read(&cached).expect("the first request fills the cache");
    assert_eq!(stored, first, "the stream and the cache hold the same clip");

    // The second request must not invoke FFmpeg again; a fresh job would also
    // succeed, but the body must be byte-identical either way.
    let (status, _, second) = send(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second, first);

    drop(dir);
}

#[tokio::test]
async fn transcode_finishes_in_the_background_after_the_client_disconnects() {
    let (dir, state) = setup().await;

    if !ffmpeg_available() {
        return;
    }

    let root_id = root_album_id(&state).await;
    let source = state.config.media_dir.join("clip.mp4");
    if !write_sample_video(&source, 640, 480, 2) {
        return;
    }

    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "vid",
        MediaType::Video,
        640,
        480,
        None,
        4096,
    )
    .await;

    let app = app(state.clone());
    let query = link(&state, "root", "vid");
    let uri = format!("/transc/v1/root/vid/h144?{query}");

    // Take the first chunk and drop the body: the viewer is gone while FFmpeg
    // is still encoding.
    let request = Request::builder()
        .uri(uri.as_str())
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body().into_data_stream();
    let first = body.next().await.expect("first chunk").expect("no error");
    assert_eq!(&first[4..8], b"ftyp");
    drop(body);

    // The cache must still be completed without any subscriber.
    let cached = state.config.transcode_dir().join("h144").join("vid.mp4");
    wait_for_file(&cached).await;

    let (status, _, after) = send(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert!(after.len() > 1_000);
    assert_eq!(after, std::fs::read(&cached).unwrap());

    drop(dir);
}

#[tokio::test]
async fn transcodes_limit_parallel_ffmpeg_jobs() {
    let (dir, state) = setup().await;

    if !ffmpeg_available() {
        return;
    }

    let root_id = root_album_id(&state).await;
    let mut uris = Vec::new();

    for index in 0..4 {
        let name = format!("clip{index}.mp4");
        let hash = format!("vid{index}");
        let source = state.config.media_dir.join(&name);
        if !write_sample_video(&source, 1280, 720, 3) {
            return;
        }

        insert_typed_media(
            &state.db,
            root_id,
            &name,
            &hash,
            MediaType::Video,
            1280,
            720,
            None,
            4096,
        )
        .await;

        let query = link(&state, "root", &hash);
        uris.push(format!("/transc/v1/root/{hash}/h144?{query}"));
    }

    let app = app(state.clone());
    let (a, b, c, d) = tokio::join!(
        send(&app, &uris[0]),
        send(&app, &uris[1]),
        send(&app, &uris[2]),
        send(&app, &uris[3]),
    );

    for (status, _, body) in [a, b, c, d] {
        assert_eq!(status, StatusCode::OK);
        assert!(body.len() > 1_000, "every viewer receives a complete clip");
    }

    assert!(
        state.peak_transcodes() <= MAX_CONCURRENT_TRANSCODES,
        "the hub must never run more than {MAX_CONCURRENT_TRANSCODES} FFmpeg jobs, saw {}",
        state.peak_transcodes()
    );
    assert!(state.peak_transcodes() >= 1, "at least one job must run");

    drop(dir);
}

#[tokio::test]
async fn transcodes_redirect_to_the_original_without_ffmpeg() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "vid",
        MediaType::Video,
        320,
        240,
        None,
        10,
    )
    .await;
    std::fs::write(state.config.media_dir.join("clip.mp4"), b"mp4").unwrap();

    let mut state = state;
    state.capabilities.ffmpeg = false;

    let app = app(state.clone());
    let query = link(&state, "root", "vid");
    let (status, headers, body) = send(&app, &format!("/transc/v1/root/vid/h144?{query}")).await;

    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert!(body.is_empty());
    let location = headers[axum::http::header::LOCATION].to_str().unwrap();
    assert!(location.starts_with("/file/v1/root/vid?sign="));
    assert!(location.ends_with(&query));

    drop(dir);
}

#[tokio::test]
async fn transcodes_require_a_valid_link_and_a_video() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let app = app(state.clone());

    // Missing signature.
    let (status, _, _) = send(&app, "/transc/v1/root/vid/h144").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    insert_typed_media(
        &state.db,
        root_id,
        "song.mp3",
        "aud",
        MediaType::Audio,
        0,
        0,
        None,
        10,
    )
    .await;
    std::fs::write(state.config.media_dir.join("song.mp3"), b"mp3").unwrap();

    let query = link(&state, "root", "aud");
    let (status, _, _) = send(&app, &format!("/transc/v1/root/aud/h144?{query}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    drop(dir);
}

/// Waits until a background transcode has written a non-empty cache file.
async fn wait_for_file(path: &Path) {
    for _ in 0..600 {
        if tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
        {
            return;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("{} was not cached in time", path.display());
}
