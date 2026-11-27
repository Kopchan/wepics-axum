//! `GET /transc/v2/{album}/{media}/{direction}{size}` — streaming video previews.

use axum::http::StatusCode;

use crate::common::{
    app, ffmpeg_available, insert_typed_media, link, root_album_id, send, setup, write_sample_video,
};
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

    let (status, headers, body) = send(&app, &format!("/transc/v2/root/vid/h144?{query}")).await;
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
    let (status, headers, body) = send(&app, &format!("/transc/v2/root/vid/h144?{query}")).await;

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
    let (status, _, _) = send(&app, "/transc/v2/root/vid/h144").await;
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
    let (status, _, _) = send(&app, &format!("/transc/v2/root/aud/h144?{query}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    drop(dir);
}
