//! `GET /thumb/v1/{album}/{media}/{spec}` — preview generation and caching.

use axum::http::StatusCode;

use crate::common::{
    NewMedia, app, at, ffmpeg_available, insert_media, insert_typed_media, link, root_album_id,
    send, setup, write_png, write_sample_animation, write_sample_video,
};
use wepics_axum::models::MediaType;

#[tokio::test]
async fn previews_are_resized_encoded_and_cached() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let source = state.config.media_dir.join("photo.png");
    write_png(&source, 800, 600);
    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.png",
            hash: "media1",
            date: Some(at(1)),
            size: 10,
        },
    )
    .await;

    let app = app(state.clone());
    let query = link(&state, "root", "media1");

    // 800x600 scaled down to width 144 -> 108 px high, WebP encoded.
    let (status, headers, body) = send(&app, &format!("/thumb/v1/root/media1/w144?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[axum::http::header::CONTENT_TYPE], "image/webp");
    assert_eq!(&body[..4], b"RIFF");
    assert_eq!(&body[8..12], b"WEBP");
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (144, 108));

    // Cached under the cache root, never inside the media tree.
    let cached = state.thumb_dir().join("w144").join("media1.webp");
    assert!(cached.is_file(), "generated previews are cached");
    assert!(
        !state.config.media_dir.join(".thumbs").exists(),
        "the media tree stays free of generated files"
    );

    // Height mode keeps the aspect ratio.
    let (status, _, body) = send(&app, &format!("/thumb/v1/root/media1/h240?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (320, 240));

    // Square mode covers and centre-crops.
    let (status, _, body) = send(&app, &format!("/thumb/v1/root/media1/q144?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (144, 144));

    // Sizes snap up to the next allowed step.
    let (status, _, _) = send(&app, &format!("/thumb/v1/root/media1/w200?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.thumb_dir().join("w240").join("media1.webp").is_file());

    drop(dir);
}

#[tokio::test]
async fn previews_require_a_valid_signature() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    write_png(&state.config.media_dir.join("photo.png"), 80, 60);
    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.png",
            hash: "media1",
            date: None,
            size: 10,
        },
    )
    .await;

    let app = app(state.clone());
    let exp = chrono::Utc::now().timestamp() + 3600;

    let (status, _, _) = send(&app, "/thumb/v1/root/media1/w144").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(
        &app,
        &format!("/thumb/v1/root/media1/w144?sign=bad&exp={exp}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let expired_at = chrono::Utc::now().timestamp() - 10;
    let expired = state.sign_media("root", "media1", expired_at);
    let (status, _, _) = send(
        &app,
        &format!("/thumb/v1/root/media1/w144?sign={expired}&exp={expired_at}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A token for another media hash does not unlock this file.
    let other = state.sign_media("root", "other", exp);
    let (status, _, _) = send(
        &app,
        &format!("/thumb/v1/root/media1/w144?sign={other}&exp={exp}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Right token, unknown media.
    let missing_exp = chrono::Utc::now().timestamp() + 3600;
    let missing_token = state.sign_media("root", "missing", missing_exp);
    let (status, _, _) = send(
        &app,
        &format!("/thumb/v1/root/missing/w144?sign={missing_token}&exp={missing_exp}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

#[tokio::test]
async fn vector_previews_are_rendered() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let svg = state.config.media_dir.join("logo.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200">
            <rect width="400" height="200" fill="#3050ff"/>
          </svg>"##,
    )
    .unwrap();

    insert_typed_media(
        &state.db,
        root_id,
        "logo.svg",
        "vec",
        MediaType::Vector,
        400,
        200,
        Some(at(1)),
        200,
    )
    .await;

    let app = app(state.clone());
    let query = link(&state, "root", "vec");
    let (status, _, body) = send(&app, &format!("/thumb/v1/root/vec/w144?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (144, 72));

    drop(dir);
}

#[tokio::test]
async fn animated_previews_use_a_cached_frame() {
    let (dir, state) = setup().await;

    if !ffmpeg_available() {
        return;
    }

    let root_id = root_album_id(&state).await;
    let source = state.config.media_dir.join("loop.gif");
    if !write_sample_animation(&source, 64, 48, 1) {
        return;
    }

    insert_typed_media(
        &state.db,
        root_id,
        "loop.gif",
        "ani",
        MediaType::Animated,
        64,
        48,
        None,
        1024,
    )
    .await;
    sqlx::query(
        "UPDATE media SET duration_sec = 1.0, framerate = 10.0, frame_count = 10 \
         WHERE hash = 'ani'",
    )
    .execute(&state.db)
    .await
    .unwrap();

    let app = app(state.clone());
    let query = link(&state, "root", "ani");
    let (status, _, body) = send(&app, &format!("/thumb/v1/root/ani/w48?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    let preview = image::load_from_memory(&body).unwrap();
    // 48 snaps up to 144, and the 64 px source is not upscaled.
    assert_eq!((preview.width(), preview.height()), (64, 48));
    assert!(state.frame_path("ani").is_file());

    drop(dir);
}

#[tokio::test]
async fn previews_redirect_to_the_original_without_libvips() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    write_png(&state.config.media_dir.join("photo.png"), 80, 60);
    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.png",
            hash: "media1",
            date: None,
            size: 10,
        },
    )
    .await;

    let mut state = state;
    state.capabilities.vips = false;

    let app = app(state.clone());
    let query = link(&state, "root", "media1");
    let (status, headers, body) = send(&app, &format!("/thumb/v1/root/media1/w144?{query}")).await;

    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert!(body.is_empty());
    let location = headers[axum::http::header::LOCATION].to_str().unwrap();
    assert!(location.starts_with("/file/v1/root/media1?sign="));
    assert!(location.ends_with(&query));

    drop(dir);
}

#[tokio::test]
async fn video_previews_serve_a_cached_frame_without_libvips() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "vid",
        MediaType::Video,
        80,
        60,
        None,
        10,
    )
    .await;
    std::fs::write(state.config.media_dir.join("clip.mp4"), b"mp4").unwrap();

    // Pretend FFmpeg exported the frame earlier; libvips itself is missing.
    write_png(&state.frame_path("vid"), 80, 60);
    let mut state = state;
    state.capabilities.vips = false;
    state.capabilities.ffmpeg = false;

    let app = app(state.clone());
    let query = link(&state, "root", "vid");
    let (status, headers, body) = send(&app, &format!("/thumb/v1/root/vid/w144?{query}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[axum::http::header::CONTENT_TYPE], "image/png");
    assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n");

    drop(dir);
}

#[tokio::test]
async fn video_previews_fail_without_libvips_and_a_frame() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "vid",
        MediaType::Video,
        80,
        60,
        None,
        10,
    )
    .await;
    std::fs::write(state.config.media_dir.join("clip.mp4"), b"mp4").unwrap();

    let mut state = state;
    state.capabilities.vips = false;
    state.capabilities.ffmpeg = false;

    let app = app(state.clone());
    let query = link(&state, "root", "vid");
    let (status, _, _) = send(&app, &format!("/thumb/v1/root/vid/w144?{query}")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    drop(dir);
}

#[tokio::test]
async fn only_images_and_videos_have_previews() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

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

    let app = app(state.clone());
    let query = link(&state, "root", "aud");
    let (status, _, _) = send(&app, &format!("/thumb/v1/root/aud/w144?{query}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    drop(dir);
}

#[tokio::test]
async fn video_previews_are_rendered_from_a_cached_frame() {
    let (dir, state) = setup().await;

    if !ffmpeg_available() {
        return;
    }

    let root_id = root_album_id(&state).await;
    let source = state.config.media_dir.join("clip.mp4");
    if !write_sample_video(&source, 320, 240, 3) {
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
    sqlx::query(
        "UPDATE media SET duration_sec = 3.0, framerate = 25.0, frame_count = 75 \
         WHERE hash = 'vid'",
    )
    .execute(&state.db)
    .await
    .unwrap();

    let app = app(state.clone());
    let query = link(&state, "root", "vid");

    let (status, headers, body) = send(&app, &format!("/thumb/v1/root/vid/w144?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[axum::http::header::CONTENT_TYPE], "image/webp");
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (144, 108));

    // The original-size frame is cached as lossless PNG and reused for other
    // preview sizes without decoding the video again.
    let frame_path = state.frame_path("vid");
    let frame = std::fs::read(&frame_path).expect("frame must be cached");
    assert_eq!(&frame[..8], b"\x89PNG\r\n\x1a\n");
    let cached_frame = image::load_from_memory(&frame).unwrap();
    assert_eq!((cached_frame.width(), cached_frame.height()), (320, 240));

    let (status, _, body) = send(&app, &format!("/thumb/v1/root/vid/h240?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (320, 240));

    drop(dir);
}

#[tokio::test]
async fn preview_does_not_leak_outside_the_media_tree() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    // A row whose name tries to escape the album directory.
    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "../secret.png",
            hash: "evil",
            date: None,
            size: 1,
        },
    )
    .await;

    let app = app(state.clone());
    let query = link(&state, "root", "evil");
    let (status, _, _) = send(&app, &format!("/thumb/v1/root/evil/w144?{query}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}
