//! `GET /file/v1/{album}/{media}` — originals, ranges and link validation.

use axum::http::{HeaderMap, Method, StatusCode, header};

use crate::common::{
    NewMedia, app, at, insert_media, link, root_album_id, send, send_range, send_with, setup,
};
use wepics_axum::models::ROOT_ALBUM_HASH;

#[tokio::test]
async fn files_are_served_with_inline_disposition_and_caching() {
    let (dir, state) = setup().await;
    let album = crate::common::insert_album(&state.db, "Trip", "abc123").await;
    let media = insert_media(
        &state.db,
        album.id,
        NewMedia {
            name: "cat.jpg",
            hash: "h1",
            date: Some(at(1)),
            size: 8,
        },
    )
    .await;

    let album_dir = state.media_dir.join(&album.hash);
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join(&media.name), b"jpegdata").unwrap();

    let app = app(state.clone());
    let query = link(&state, "abc123", "h1");
    let (status, headers, body) = send(&app, &format!("/file/v1/abc123/h1?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"jpegdata");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("inline"),
    );
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=3600, immutable"
    );

    // Metadata exists, but the file on disk does not.
    let missing = link(&state, "abc123", "missing");
    let (status, _, _) = send(&app, &format!("/file/v1/abc123/missing?{missing}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

#[tokio::test]
async fn root_album_files_live_directly_in_media_dir() {
    let (dir, state) = setup().await;
    let app = app(state.clone());

    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);
    let _ = body;

    let root_id = root_album_id(&state).await;
    let media = insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "root.png",
            hash: "h2",
            date: None,
            size: 3,
        },
    )
    .await;
    std::fs::write(state.media_dir.join(&media.name), b"png").unwrap();

    let query = link(&state, ROOT_ALBUM_HASH, "h2");
    let (status, _, body) = send(&app, &format!("/file/v1/root/h2?{query}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"png");

    drop(dir);
}

#[tokio::test]
async fn files_support_range_requests_and_head() {
    let (dir, state) = setup().await;
    let album = crate::common::insert_album(&state.db, "Trip", "abc123").await;
    let media = insert_media(
        &state.db,
        album.id,
        NewMedia {
            name: "clip.jpg",
            hash: "h1",
            date: Some(at(1)),
            size: 8,
        },
    )
    .await;

    let album_dir = state.media_dir.join(&album.hash);
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join(&media.name), b"jpegdata").unwrap();

    let app = app(state.clone());
    let query = link(&state, "abc123", "h1");
    let uri = format!("/file/v1/abc123/h1?{query}");

    let (status, headers, body) = send(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert!(headers.contains_key(header::ETAG));
    assert!(headers.contains_key(header::LAST_MODIFIED));
    assert_eq!(body, b"jpegdata");

    // A matching validator is answered with 304 by `ServeFile`.
    let mut conditional = HeaderMap::new();
    conditional.insert(header::IF_NONE_MATCH, headers[header::ETAG].clone());
    let (status, _, body) = send_with(&app, Method::GET, &uri, conditional).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());

    // Multi-range requests are ignored and served as a normal full response.
    let (status, headers, body) = send_range(&app, &uri, "bytes=0-1,3-4").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_RANGE).is_none());
    assert_eq!(body, b"jpegdata");

    // Closed range.
    let (status, headers, body) = send_range(&app, &uri, "bytes=0-3").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 0-3/8");
    assert_eq!(headers[header::CONTENT_LENGTH], "4");
    assert_eq!(body, b"jpeg");

    // Open-ended range.
    let (status, headers, body) = send_range(&app, &uri, "bytes=4-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 4-7/8");
    assert_eq!(body, b"data");

    // Suffix range.
    let (status, headers, body) = send_range(&app, &uri, "bytes=-2").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 6-7/8");
    assert_eq!(body, b"ta");

    // Start past the end.
    let (status, headers, body) = send_range(&app, &uri, "bytes=99-").await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes */8");
    assert!(body.is_empty());

    // HEAD is served by the GET handler without a body.
    let (status, headers, body) = send_with(&app, Method::HEAD, &uri, HeaderMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_LENGTH], "8");
    assert!(body.is_empty());

    drop(dir);
}

#[tokio::test]
async fn files_require_a_valid_signature() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.png",
            hash: "media1",
            date: None,
            size: 4,
        },
    )
    .await;
    std::fs::write(state.media_dir.join("photo.png"), b"data").unwrap();

    let app = app(state.clone());
    let exp = chrono::Utc::now().timestamp() + 3600;

    let (status, _, _) = send(&app, "/file/v1/root/media1").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(&app, &format!("/file/v1/root/media1?hash=bad&exp={exp}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let expired_at = chrono::Utc::now().timestamp() - 10;
    let expired = state.sign_media("root", "media1", expired_at);
    let (status, _, _) = send(
        &app,
        &format!("/file/v1/root/media1?hash={expired}&exp={expired_at}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A token for another media hash does not unlock this file.
    let other = state.sign_media("root", "other", exp);
    let (status, _, _) = send(
        &app,
        &format!("/file/v1/root/media1?hash={other}&exp={exp}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    drop(dir);
}
