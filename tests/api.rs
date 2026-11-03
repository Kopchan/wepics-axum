use std::str::FromStr;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tower::ServiceExt;

use wepics_axum::controllers::{AlbumPage, MediaPage};
use wepics_axum::models::{Album, Media, ROOT_ALBUM_HASH, natural_sort_key};
use wepics_axum::routes;
use wepics_axum::state::AppState;

async fn setup_raw() -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();

    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let state = AppState::new(pool, dir.path().join("media"));
    (dir, state)
}

/// Mirrors what `AppState::connect` does at startup.
async fn setup() -> (TempDir, AppState) {
    let (dir, state) = setup_raw().await;
    state.initialize().await.unwrap();
    (dir, state)
}

fn app(state: AppState) -> Router {
    routes::app(state)
}

async fn send(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    send_with(app, Method::GET, uri, HeaderMap::new()).await
}

async fn send_range(app: &Router, uri: &str, range: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::RANGE,
        HeaderValue::from_str(range).expect("range value is ASCII"),
    );

    send_with(app, Method::GET, uri, headers).await
}

async fn send_with(
    app: &Router,
    method: Method,
    uri: &str,
    headers: HeaderMap,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    *request.headers_mut() = headers;

    let response = app.clone().oneshot(request).await.unwrap();

    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();

    (status, headers, body)
}

async fn insert_album(pool: &SqlitePool, name: &str, hash: &str) -> Album {
    sqlx::query_as(
        "INSERT INTO albums (name, hash, sort_name) VALUES (?1, ?2, ?3)
         RETURNING id, name, hash, parent_id, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(hash)
    .bind(natural_sort_key(name))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn insert_child(
    pool: &SqlitePool,
    parent_id: i64,
    name: &str,
    created_at: DateTime<Utc>,
) -> Album {
    sqlx::query_as(
        "INSERT INTO albums (name, hash, sort_name, parent_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         RETURNING id, name, hash, parent_id, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(format!("{parent_id}-{name}"))
    .bind(natural_sort_key(name))
    .bind(parent_id)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

struct NewMedia<'a> {
    name: &'a str,
    hash: &'a str,
    date: Option<DateTime<Utc>>,
    size: i64,
}

async fn insert_media(pool: &SqlitePool, album_id: i64, media: NewMedia<'_>) -> Media {
    sqlx::query_as(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height, date, size, sort_name)
           VALUES (?1, ?2, 'image', ?3, 800, 600, ?4, ?5, ?6)
           RETURNING id, name, hash, "type", album_id, width, height, duration_sec,
                     date, size, sort_name, created_at, updated_at"#,
    )
    .bind(media.name)
    .bind(media.hash)
    .bind(album_id)
    .bind(media.date)
    .bind(media.size)
    .bind(natural_sort_key(media.name))
    .fetch_one(pool)
    .await
    .unwrap()
}

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).unwrap()
}

fn parse_page(body: &[u8]) -> MediaPage {
    serde_json::from_slice(body).unwrap_or_else(|err| {
        panic!(
            "invalid MediaPage json ({err}): {}",
            String::from_utf8_lossy(body)
        )
    })
}

fn parse_album_page(body: &[u8]) -> AlbumPage {
    serde_json::from_slice(body).unwrap_or_else(|err| {
        panic!(
            "invalid AlbumPage json ({err}): {}",
            String::from_utf8_lossy(body)
        )
    })
}

fn album_names(page: &AlbumPage) -> Vec<&str> {
    page.items.iter().map(|album| album.name.as_str()).collect()
}

#[tokio::test]
async fn root_album_is_created_at_startup() {
    let (dir, state) = setup().await;
    let media_dir = state.media_dir.clone();
    let pool = state.db.clone();
    let app = app(state);

    assert!(media_dir.is_dir(), "media dir must exist after startup");

    let (status, _, body) = send(&app, "/api/album/root").await;
    assert_eq!(status, StatusCode::OK);

    let album: Album = serde_json::from_slice(&body).unwrap();
    assert_eq!(album.hash, ROOT_ALBUM_HASH);
    assert_eq!(album.name, "root");

    // Second request reuses the row instead of creating a duplicate.
    let (status, _, body) = send(&app, "/api/album/root").await;
    assert_eq!(status, StatusCode::OK);
    let again: Album = serde_json::from_slice(&body).unwrap();
    assert_eq!(again.id, album.id);

    let roots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums WHERE hash = ?1")
        .bind(ROOT_ALBUM_HASH)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(roots, 1);

    drop(dir);
}

#[tokio::test]
async fn requests_never_create_the_root_album() {
    let (dir, state) = setup_raw().await;
    let app = app(state.clone());

    let (status, _, _) = send(&app, "/api/album/root").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(albums, 0, "requests must not create albums");
    assert!(!state.media_dir.exists());

    // Startup is the only place that bootstraps storage.
    state.initialize().await.unwrap();
    let (status, _, _) = send(&app, "/api/album/root").await;
    assert_eq!(status, StatusCode::OK);

    drop(dir);
}

#[tokio::test]
async fn album_by_hash_and_unknown_hash() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;
    let app = app(state);

    let (status, _, body) = send(&app, "/api/album/abc123").await;
    assert_eq!(status, StatusCode::OK);
    let fetched: Album = serde_json::from_slice(&body).unwrap();

    // `sort_name` is internal: clients never see it, so it deserializes empty.
    assert_eq!(fetched.sort_name, "");
    assert_eq!(fetched.id, album.id);
    assert_eq!(fetched.name, album.name);
    assert_eq!(fetched.hash, album.hash);
    assert_eq!(fetched.parent_id, album.parent_id);
    assert_eq!(fetched.created_at, album.created_at);

    let (status, _, _) = send(&app, "/api/album/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

#[tokio::test]
async fn media_listing_is_paginated_and_sorted_by_date_by_default() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;

    // Inserted out of order on purpose: dates day 3, 1, 5, 2, 4.
    for (name, day) in [
        ("m3.jpg", 3),
        ("m1.jpg", 1),
        ("m5.jpg", 5),
        ("m2.jpg", 2),
        ("m4.jpg", 4),
    ] {
        let media = NewMedia {
            name,
            hash: name,
            date: Some(at(day)),
            size: 10,
        };
        insert_media(&state.db, album.id, media).await;
    }

    let app = app(state);

    let (status, _, body) = send(&app, "/api/album/abc123/media?page=2&limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_page(&body);
    assert_eq!(page.total, 5);
    assert_eq!(page.pages, 3);
    assert_eq!(page.page, 2);
    assert_eq!(page.limit, 2);
    let names: Vec<_> = page.items.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["m3.jpg", "m4.jpg"]);

    // Default limit is 30.
    let (_, _, body) = send(&app, "/api/album/abc123/media").await;
    let page = parse_page(&body);
    assert_eq!(page.limit, 30);
    assert_eq!(page.items.len(), 5);

    drop(dir);
}

#[tokio::test]
async fn name_sort_is_natural_and_reversible() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;

    for (index, name) in ["img10.jpg", "img2.jpg", "img1.jpg"].iter().enumerate() {
        let media = NewMedia {
            name,
            hash: name,
            date: Some(at(index as u32 + 1)),
            size: 10,
        };
        insert_media(&state.db, album.id, media).await;
    }

    let app = app(state);

    let (_, _, body) = send(&app, "/api/album/abc123/media?sort=name").await;
    let names: Vec<_> = parse_page(&body)
        .items
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, vec!["img1.jpg", "img2.jpg", "img10.jpg"]);

    let (_, _, body) = send(&app, "/api/album/abc123/media?sort=-name").await;
    let names: Vec<_> = parse_page(&body)
        .items
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, vec!["img10.jpg", "img2.jpg", "img1.jpg"]);

    drop(dir);
}

#[tokio::test]
async fn size_sort_is_supported() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;

    for (name, size) in [("b.jpg", 200), ("a.jpg", 100), ("c.jpg", 300)] {
        let media = NewMedia {
            name,
            hash: name,
            date: Some(at(1)),
            size,
        };
        insert_media(&state.db, album.id, media).await;
    }

    let app = app(state);

    let (_, _, body) = send(&app, "/api/album/abc123/media?sort=-size").await;
    let names: Vec<_> = parse_page(&body)
        .items
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, vec!["c.jpg", "b.jpg", "a.jpg"]);

    drop(dir);
}

#[tokio::test]
async fn invalid_pagination_and_sort_are_bad_requests() {
    let (dir, state) = setup().await;
    insert_album(&state.db, "Trip", "abc123").await;
    let app = app(state);

    for uri in [
        "/api/album/abc123/media?page=0",
        "/api/album/abc123/media?limit=0",
        "/api/album/abc123/media?limit=101",
        "/api/album/abc123/media?sort=width",
        "/api/album/abc123/media?sort=--name",
    ] {
        let (status, _, body) = send(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "uri: {uri}");
        assert!(
            String::from_utf8_lossy(&body).contains("error"),
            "uri: {uri}, body: {}",
            String::from_utf8_lossy(&body)
        );
    }

    drop(dir);
}

#[tokio::test]
async fn files_are_served_from_the_media_dir() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;
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

    let album_dir = dir.path().join("media").join(&album.hash);
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join(format!("{}.jpg", media.hash)), b"jpegdata").unwrap();

    let app = app(state);

    let (status, headers, body) = send(&app, "/files/abc123/h1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers[header::CONTENT_LENGTH], "8");
    assert_eq!(body, b"jpegdata");

    // Metadata exists, but the file on disk does not.
    let (status, _, _) = send(&app, "/files/abc123/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

#[tokio::test]
async fn root_album_files_live_directly_in_media_dir() {
    let (dir, state) = setup().await;
    let app = app(state.clone());

    let (status, _, body) = send(&app, "/api/album/root").await;
    assert_eq!(status, StatusCode::OK);
    let root: Album = serde_json::from_slice(&body).unwrap();

    let media = insert_media(
        &state.db,
        root.id,
        NewMedia {
            name: "root.png",
            hash: "h2",
            date: None,
            size: 3,
        },
    )
    .await;
    std::fs::write(
        dir.path().join("media").join(format!("{}.png", media.hash)),
        b"png",
    )
    .unwrap();

    let (status, _, body) = send(&app, "/files/root/h2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"png");

    drop(dir);
}

#[tokio::test]
async fn album_children_are_paginated_and_sorted() {
    let (dir, state) = setup().await;
    let parent = insert_album(&state.db, "Trip", "trip").await;
    let other = insert_album(&state.db, "Other", "other").await;

    // Inserted out of order on purpose.
    for (name, day) in [("img10.jpg", 1), ("img2.jpg", 2), ("img1.jpg", 3)] {
        insert_child(&state.db, parent.id, name, at(day)).await;
    }
    insert_child(&state.db, other.id, "elsewhere.jpg", at(1)).await;

    let app = app(state);

    // Default: natural order by name.
    let (status, _, body) = send(&app, "/api/albums/trip/child").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_album_page(&body);
    assert_eq!(page.total, 3);
    assert_eq!(page.pages, 1);
    assert_eq!(page.page, 1);
    assert_eq!(page.limit, 30);
    assert_eq!(
        album_names(&page),
        vec!["img1.jpg", "img2.jpg", "img10.jpg"]
    );

    // `sort_name` is internal and must not leak into the JSON.
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["items"][0].get("sort_name").is_none());

    let (_, _, body) = send(&app, "/api/albums/trip/child?sort=-name").await;
    let page = parse_album_page(&body);
    assert_eq!(
        album_names(&page),
        vec!["img10.jpg", "img2.jpg", "img1.jpg"]
    );

    let (_, _, body) = send(&app, "/api/albums/trip/child?sort=created_at").await;
    let page = parse_album_page(&body);
    assert_eq!(
        album_names(&page),
        vec!["img10.jpg", "img2.jpg", "img1.jpg"]
    );

    let (_, _, body) = send(&app, "/api/albums/trip/child?sort=-created_at").await;
    let page = parse_album_page(&body);
    assert_eq!(
        album_names(&page),
        vec!["img1.jpg", "img2.jpg", "img10.jpg"]
    );

    let (status, _, body) = send(&app, "/api/albums/trip/child?page=2&limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_album_page(&body);
    assert_eq!(page.page, 2);
    assert_eq!(page.limit, 2);
    assert_eq!(page.pages, 2);
    assert_eq!(album_names(&page), vec!["img10.jpg"]);

    drop(dir);
}

#[tokio::test]
async fn album_children_reject_bad_requests() {
    let (dir, state) = setup().await;
    insert_album(&state.db, "Trip", "trip").await;
    let app = app(state);

    for uri in [
        "/api/albums/trip/child?page=0",
        "/api/albums/trip/child?limit=0",
        "/api/albums/trip/child?limit=101",
        "/api/albums/trip/child?sort=width",
        "/api/albums/trip/child?sort=--name",
    ] {
        let (status, _, body) = send(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "uri: {uri}");
        assert!(
            String::from_utf8_lossy(&body).contains("error"),
            "uri: {uri}, body: {}",
            String::from_utf8_lossy(&body)
        );
    }

    let (status, _, _) = send(&app, "/api/albums/nope/child").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

#[tokio::test]
async fn files_support_range_requests_and_long_lived_caching() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;
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

    let album_dir = dir.path().join("media").join(&album.hash);
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join(format!("{}.jpg", media.hash)), b"jpegdata").unwrap();

    let app = app(state);

    // Full response advertises ranges and immutable caching.
    let (status, headers, body) = send(&app, "/files/abc123/h1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body, b"jpegdata");

    // Closed range.
    let (status, headers, body) = send_range(&app, "/files/abc123/h1", "bytes=0-3").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 0-3/8");
    assert_eq!(headers[header::CONTENT_LENGTH], "4");
    assert_eq!(body, b"jpeg");

    // Open-ended range.
    let (status, headers, body) = send_range(&app, "/files/abc123/h1", "bytes=4-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 4-7/8");
    assert_eq!(body, b"data");

    // Suffix range.
    let (status, headers, body) = send_range(&app, "/files/abc123/h1", "bytes=-2").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 6-7/8");
    assert_eq!(body, b"ta");

    // Start past the end.
    let (status, headers, body) = send_range(&app, "/files/abc123/h1", "bytes=99-").await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes */8");
    assert!(body.is_empty());

    // HEAD is served by the GET handler without a body.
    let (status, headers, body) =
        send_with(&app, Method::HEAD, "/files/abc123/h1", HeaderMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_LENGTH], "8");
    assert!(body.is_empty());

    drop(dir);
}

#[tokio::test]
async fn cors_is_enabled_for_every_route() {
    let (dir, state) = setup().await;
    let app = app(state);

    let (status, headers, _) = send(&app, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let exposed = headers[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .unwrap();
    assert!(exposed.contains("content-range"), "exposed: {exposed}");

    let mut preflight = HeaderMap::new();
    preflight.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://example.com"),
    );
    preflight.insert(
        header::ACCESS_CONTROL_REQUEST_METHOD,
        HeaderValue::from_static("GET"),
    );
    preflight.insert(
        header::ACCESS_CONTROL_REQUEST_HEADERS,
        HeaderValue::from_static("range"),
    );

    let (status, headers, body) =
        send_with(&app, Method::OPTIONS, "/files/abc123/h1", preflight).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_METHODS],
        "GET, HEAD, OPTIONS"
    );
    assert!(
        headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("range")
    );
    assert_eq!(headers[header::ACCESS_CONTROL_MAX_AGE], "86400");
    assert!(body.is_empty());

    drop(dir);
}
